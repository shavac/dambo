(function (root, factory) {
    if (typeof module === 'object' && module.exports) {
        module.exports = factory(root.UsStockEngine);
    } else {
        root.UsStockMarket = factory(root.UsStockEngine);
    }
})(typeof globalThis !== 'undefined' ? globalThis : window, function (defaultEngine) {
    'use strict';

    const MAX_REQ_PER_MIN = 54;
    const MIN_SPACING_MS = 1100;
    const NEGATIVE_CACHE_MS = 60000;
    const RATE_LIMIT_COOLDOWN_MS = 30000;
    const METRIC_CACHE_TTL_MS = 12 * 3600 * 1000;
    const PROFILE_CACHE_TTL_MS = 24 * 3600 * 1000;
    const MAX_INTRADAY_POINTS = 360;
    const MAX_SPARK_POINTS = 48;
    const MAX_WS_SUBS = 45;

    function createMarketEngine(options) {
        const opts = options || {};
        const Engine = opts.engine || defaultEngine || (typeof globalThis !== 'undefined' ? globalThis.UsStockEngine : null);
        if (!Engine) {
            throw new Error('UsStockEngine is required');
        }
        const onQuoteUpdate = typeof opts.onQuoteUpdate === 'function' ? opts.onQuoteUpdate : function () {};
        const onStatusChange = typeof opts.onStatusChange === 'function' ? opts.onStatusChange : function () {};

        let cfg = Object.assign({}, Engine.DEFAULT_CONFIG);
        const quotesMap = new Map();       // symbol -> quote object
        const intradayMap = new Map();     // symbol -> [{ t, p }]
        const metricsMap = new Map();      // symbol -> { w52High, w52Low, avgVol, fetchedAt }
        const profileMap = new Map();      // symbol -> { name, fetchedAt }
        const invalidUntilMap = new Map(); // symbol -> ts

        // REST rate limiter & queue
        const reqTimestamps = [];
        const fetchQueue = [];
        const queuedTickers = new Map();
        const inFlightFetches = new Map();
        let queueTimer = null;
        let pollTimer = null;
        let pollCursor = 0;
        let demoTimer = null;
        let rateLimitUntilMs = 0;
        let apiState = 'demo'; // 'live' | 'demo' | 'rate_limited' | 'invalid_key' | 'error'
        let apiError = '';

        // Finnhub WS
        let ws = null;
        let wsState = 'closed'; // 'closed' | 'connecting' | 'open'
        let wsReconnectAttempt = 0;
        let wsReconnectTimer = null;
        const subscribedSet = new Set();
        let desiredSymbols = [];
        let lastIntradayAppendAt = new Map();

        function getApiKey() {
            return String(cfg.finnhub_api_key || '').trim();
        }

        function resolveEffectiveMode() {
            const mode = String(cfg.market_data_mode || 'auto').toLowerCase();
            if (mode === 'demo') return 'demo';
            if (mode === 'finnhub') return 'finnhub';
            return getApiKey() ? 'finnhub' : 'demo';
        }

        function pruneReqWindow(now) {
            while (reqTimestamps.length > 0 && now - reqTimestamps[0] >= 60000) {
                reqTimestamps.shift();
            }
        }

        function notifyStatus() {
            pruneReqWindow(Date.now());
            onStatusChange(getTelemetry());
        }

        function getTelemetry() {
            const eff = resolveEffectiveMode();
            const now = Date.now();
            pruneReqWindow(now);
            return {
                mode: eff,
                apiState: eff === 'demo' ? 'demo' : apiState,
                apiError: apiError,
                wsState: wsState,
                wsSubscribedCount: subscribedSet.size,
                rateLimitUntilMs: rateLimitUntilMs,
                requestsLastMin: reqTimestamps.length,
                maxReqPerMin: MAX_REQ_PER_MIN,
                cachedQuotesCount: quotesMap.size,
            };
        }

        function seedIntradayIfSparse(symbol, quote) {
            const cur = intradayMap.get(symbol);
            if (cur && cur.length >= 8) return;
            const c = Number(quote.c || 0);
            if (c <= 0) return;
            const pc = Number(quote.pc) > 0 ? Number(quote.pc) : c;
            const o = Number(quote.o) > 0 ? Number(quote.o) : pc;
            const l = Number(quote.l) > 0 ? Math.min(Number(quote.l), o, c) : Math.min(o, c) * 0.996;
            const h = Number(quote.h) > 0 ? Math.max(Number(quote.h), o, c) : Math.max(o, c) * 1.004;

            const pts = [];
            const now = Date.now();
            const totalPts = 32;
            // Deterministic pseudo-random wave per symbol so chart never jitters on re-seed
            let seed = 0;
            for (let k = 0; k < symbol.length; k++) seed += symbol.charCodeAt(k) * (k + 1);

            for (let i = 0; i < totalPts; i++) {
                const u = i / (totalPts - 1);
                let base = o + (c - o) * u;
                const wave = Math.sin(u * Math.PI * 3.2 + seed) * (h - l) * 0.28 +
                    Math.cos(u * Math.PI * 6.5 + seed * 0.7) * (h - l) * 0.12;
                let p = i === 0 ? o : (i === totalPts - 1 ? c : base + wave);
                p = Math.max(l, Math.min(h, p));
                pts.push({
                    t: now - (totalPts - 1 - i) * 180000,
                    p: Engine.roundPrice(p),
                });
            }
            intradayMap.set(symbol, pts);
        }

        function appendIntradayTick(symbol, price, tsMs, force) {
            const pts = intradayMap.get(symbol) || [];
            const lastAt = lastIntradayAppendAt.get(symbol) || 0;
            if (!force && tsMs - lastAt < 1000 && pts.length > 0) {
                pts[pts.length - 1].p = price;
                pts[pts.length - 1].t = tsMs;
                return;
            }
            lastIntradayAppendAt.set(symbol, tsMs);
            pts.push({ t: tsMs, p: price });
            if (pts.length > MAX_INTRADAY_POINTS) {
                pts.splice(0, pts.length - MAX_INTRADAY_POINTS);
            }
            intradayMap.set(symbol, pts);
        }

        function seedDemoTicker(symbol) {
            const sym = Engine.normalizeTicker(symbol);
            if (!sym) return null;
            const existing = quotesMap.get(sym);
            if (existing && existing.c > 0) return existing;

            const dir = Engine.STOCK_DIRECTORY[sym];
            const basePrice = dir ? dir.price : (40 + (sym.charCodeAt(0) % 180));
            const driftPct = ((sym.charCodeAt(0) % 9) - 4) * 0.0045;
            const pc = Engine.roundPrice(basePrice / (1 + driftPct));
            const o = Engine.roundPrice(pc * (1 + driftPct * 0.45));
            const c = Engine.roundPrice(basePrice);
            const spread = Math.max(c * 0.014, 0.25);
            const h = Engine.roundPrice(Math.max(o, c) + spread * 0.65);
            const l = Engine.roundPrice(Math.max(0.01, Math.min(o, c) - spread * 0.65));
            const d = Engine.roundPrice(c - pc);
            const dp = pc > 0 ? Math.round(((c - pc) / pc) * 10000) / 100 : 0;

            const quote = {
                symbol: sym,
                name: dir ? dir.name : (sym + ' Inc.'),
                c: c,
                pc: pc,
                o: o,
                h: h,
                l: l,
                d: d,
                dp: dp,
                volume: dir ? Math.round(dir.avgVol * 0.58) : 4500000,
                w52Low: dir ? dir.w52Low : Engine.roundPrice(c * 0.68),
                w52High: dir ? dir.w52High : Engine.roundPrice(c * 1.32),
                extPrice: null,
                extLabel: '',
                source: 'demo',
                fetchedAt: Date.now(),
            };
            quotesMap.set(sym, quote);
            seedIntradayIfSparse(sym, quote);
            return quote;
        }

        function getQuote(symbol) {
            const sym = Engine.normalizeTicker(symbol);
            if (!sym) return null;
            const q = quotesMap.get(sym);
            if (q) return q;
            if (resolveEffectiveMode() === 'demo') {
                return seedDemoTicker(sym);
            }
            const dir = Engine.STOCK_DIRECTORY[sym];
            if (!dir) return null;
            return null;
        }

        function getSeries(symbol, maxPts) {
            const sym = Engine.normalizeTicker(symbol);
            if (!sym) return [];
            const pts = intradayMap.get(sym) || [];
            if (maxPts && pts.length > maxPts) {
                return pts.slice(pts.length - maxPts);
            }
            return pts;
        }

        function getSparklinePrices(symbol) {
            const pts = getSeries(symbol, MAX_SPARK_POINTS);
            return pts.map(function (pt) { return pt.p; });
        }

        // -----------------------------------------------------------------
        // Finnhub REST Queue & Fetchers
        // -----------------------------------------------------------------
        function computeWaitBeforeNextReqMs() {
            const now = Date.now();
            if (rateLimitUntilMs > now) {
                return rateLimitUntilMs - now;
            }
            pruneReqWindow(now);
            let waitMs = 0;
            if (reqTimestamps.length >= MAX_REQ_PER_MIN) {
                waitMs = Math.max(waitMs, (reqTimestamps[0] + 60100) - now);
            }
            if (reqTimestamps.length > 0) {
                const sinceLast = now - reqTimestamps[reqTimestamps.length - 1];
                if (sinceLast < MIN_SPACING_MS) {
                    waitMs = Math.max(waitMs, MIN_SPACING_MS - sinceLast);
                }
            }
            return waitMs;
        }

        async function fetchFinnhubJson(pathAndQuery) {
            const key = getApiKey();
            if (!key) {
                const e = new Error('NO_KEY');
                e.code = 'NO_KEY';
                throw e;
            }
            reqTimestamps.push(Date.now());
            notifyStatus();

            const sep = pathAndQuery.indexOf('?') === -1 ? '?' : '&';
            const url = 'https://finnhub.io/api/v1' + pathAndQuery + sep + 'token=' + encodeURIComponent(key);

            let res;
            try {
                res = await fetch(url, { method: 'GET' });
            } catch (netErr) {
                apiState = 'error';
                apiError = '网络请求失败：' + (netErr && netErr.message ? netErr.message : '请检查网络');
                notifyStatus();
                throw netErr;
            }

            if (res.status === 429) {
                rateLimitUntilMs = Date.now() + RATE_LIMIT_COOLDOWN_MS;
                apiState = 'rate_limited';
                apiError = '触及 Finnhub 60次/分钟 限流，冷却 30s';
                notifyStatus();
                const e429 = new Error('HTTP_429');
                e429.code = 'RATE_LIMITED';
                throw e429;
            }
            if (res.status === 401 || res.status === 403) {
                apiState = 'invalid_key';
                apiError = 'Finnhub API Key 无效 (HTTP ' + res.status + ')';
                notifyStatus();
                const eAuth = new Error('HTTP_AUTH');
                eAuth.code = 'INVALID_KEY';
                throw eAuth;
            }
            if (!res.ok) {
                apiState = 'error';
                apiError = 'Finnhub 返回 HTTP ' + res.status;
                notifyStatus();
                throw new Error('HTTP_' + res.status);
            }
            return res.json();
        }

        async function executeQuoteFetch(symbol) {
            const data = await fetchFinnhubJson('/quote?symbol=' + encodeURIComponent(symbol));
            const c = Number(data && data.c);
            const pc = Number(data && data.pc);
            if (!Number.isFinite(c) || c <= 0) {
                invalidUntilMap.set(symbol, Date.now() + NEGATIVE_CACHE_MS);
                apiState = 'live';
                notifyStatus();
                const eSym = new Error('INVALID_TICKER');
                eSym.code = 'INVALID_TICKER';
                throw eSym;
            }

            const prev = quotesMap.get(symbol);
            const dir = Engine.STOCK_DIRECTORY[symbol];
            const metric = metricsMap.get(symbol);
            const profile = profileMap.get(symbol);
            const validPc = Number.isFinite(pc) && pc > 0 ? Engine.roundPrice(pc) : Engine.roundPrice(c);
            const o = Number.isFinite(Number(data.o)) && Number(data.o) > 0 ? Engine.roundPrice(Number(data.o)) : validPc;
            const h = Number.isFinite(Number(data.h)) && Number(data.h) > 0 ? Engine.roundPrice(Number(data.h)) : Math.max(c, o);
            const l = Number.isFinite(Number(data.l)) && Number(data.l) > 0 ? Engine.roundPrice(Number(data.l)) : Math.min(c, o);
            const d = Number.isFinite(Number(data.d)) ? Engine.roundPrice(Number(data.d)) : Engine.roundPrice(c - validPc);
            const dp = Number.isFinite(Number(data.dp))
                ? Math.round(Number(data.dp) * 100) / 100
                : (validPc > 0 ? Math.round(((c - validPc) / validPc) * 10000) / 100 : 0);

            const quote = {
                symbol: symbol,
                name: (profile && profile.name) || (dir && dir.name) || (prev && prev.name) || symbol,
                c: Engine.roundPrice(c),
                pc: validPc,
                o: o,
                h: h,
                l: l,
                d: d,
                dp: dp,
                volume: (prev && prev.volume) || (metric && metric.avgVol) || (dir && dir.avgVol) || 0,
                w52Low: (metric && metric.w52Low) || (dir && dir.w52Low) || Engine.roundPrice(l * 0.78),
                w52High: (metric && metric.w52High) || (dir && dir.w52High) || Engine.roundPrice(h * 1.24),
                extPrice: prev && prev.extPrice ? prev.extPrice : null,
                extLabel: prev && prev.extLabel ? prev.extLabel : '',
                source: 'finnhub_rest',
                fetchedAt: Date.now(),
            };

            apiState = 'live';
            apiError = '';
            quotesMap.set(symbol, quote);
            seedIntradayIfSparse(symbol, quote);
            appendIntradayTick(symbol, quote.c, quote.fetchedAt, true);
            notifyStatus();
            onQuoteUpdate(symbol, quote);
            return quote;
        }

        function enqueueQuoteFetch(symbol, priorityHigh) {
            if (inFlightFetches.has(symbol)) {
                return inFlightFetches.get(symbol);
            }
            if (queuedTickers.has(symbol)) {
                const task = queuedTickers.get(symbol);
                if (priorityHigh && !task.priorityHigh) {
                    task.priorityHigh = true;
                    const idx = fetchQueue.indexOf(task);
                    if (idx > 0) {
                        fetchQueue.splice(idx, 1);
                        fetchQueue.unshift(task);
                    }
                }
                return task.promise;
            }

            let resolveFn;
            let rejectFn;
            const promise = new Promise(function (resolve, reject) {
                resolveFn = resolve;
                rejectFn = reject;
            });
            const task = {
                symbol: symbol,
                priorityHigh: Boolean(priorityHigh),
                resolve: resolveFn,
                reject: rejectFn,
                promise: promise,
            };
            queuedTickers.set(symbol, task);
            if (priorityHigh) {
                fetchQueue.unshift(task);
            } else {
                fetchQueue.push(task);
            }
            scheduleDrain();
            return promise;
        }

        function scheduleDrain() {
            if (queueTimer !== null || fetchQueue.length === 0) return;
            const delay = Math.max(15, computeWaitBeforeNextReqMs());
            queueTimer = setTimeout(drainNextQueueTask, delay);
        }

        function drainNextQueueTask() {
            queueTimer = null;
            if (fetchQueue.length === 0) return;
            if (resolveEffectiveMode() !== 'finnhub' || !getApiKey()) {
                while (fetchQueue.length > 0) {
                    const t = fetchQueue.shift();
                    queuedTickers.delete(t.symbol);
                    t.resolve(seedDemoTicker(t.symbol));
                }
                return;
            }

            const waitMs = computeWaitBeforeNextReqMs();
            if (waitMs > 0) {
                scheduleDrain();
                return;
            }

            const task = fetchQueue.shift();
            queuedTickers.delete(task.symbol);

            const p = executeQuoteFetch(task.symbol)
                .then(function (q) {
                    inFlightFetches.delete(task.symbol);
                    task.resolve(q);
                    return q;
                })
                .catch(function (err) {
                    inFlightFetches.delete(task.symbol);
                    task.reject(err);
                    throw err;
                })
                .finally(function () {
                    if (fetchQueue.length > 0) scheduleDrain();
                });
            inFlightFetches.set(task.symbol, p);
        }

        async function enrichSymbolMetadata(symbol) {
            const sym = Engine.normalizeTicker(symbol);
            if (!sym || resolveEffectiveMode() !== 'finnhub' || !getApiKey()) return;
            if (rateLimitUntilMs > Date.now()) return;
            pruneReqWindow(Date.now());
            if (reqTimestamps.length >= MAX_REQ_PER_MIN - 8) return;

            const now = Date.now();
            const cachedMetric = metricsMap.get(sym);
            if (!cachedMetric || now - cachedMetric.fetchedAt > METRIC_CACHE_TTL_MS) {
                try {
                    const m = await fetchFinnhubJson('/stock/metric?symbol=' + encodeURIComponent(sym) + '&metric=all');
                    const met = m && m.metric ? m.metric : {};
                    const w52H = Number(met['52WeekHigh'] || 0);
                    const w52L = Number(met['52WeekLow'] || 0);
                    const avgVolM = Number(met['10DayAverageTradingVolume'] || 0);
                    const entry = {
                        w52High: w52H > 0 ? Engine.roundPrice(w52H) : 0,
                        w52Low: w52L > 0 ? Engine.roundPrice(w52L) : 0,
                        avgVol: avgVolM > 0 ? Math.round(avgVolM * 1e6) : 0,
                        fetchedAt: Date.now(),
                    };
                    metricsMap.set(sym, entry);
                    const q = quotesMap.get(sym);
                    if (q) {
                        if (entry.w52High > 0) q.w52High = entry.w52High;
                        if (entry.w52Low > 0) q.w52Low = entry.w52Low;
                        if (entry.avgVol > 0 && !q.volume) q.volume = entry.avgVol;
                        onQuoteUpdate(sym, q);
                    }
                } catch (_) {
                    // Best-effort metadata enrichment
                }
            }

            if (!Engine.STOCK_DIRECTORY[sym]) {
                const cachedProf = profileMap.get(sym);
                if (!cachedProf || now - cachedProf.fetchedAt > PROFILE_CACHE_TTL_MS) {
                    try {
                        const prof = await fetchFinnhubJson('/stock/profile2?symbol=' + encodeURIComponent(sym));
                        if (prof && prof.name) {
                            profileMap.set(sym, { name: String(prof.name), fetchedAt: Date.now() });
                            const q = quotesMap.get(sym);
                            if (q) {
                                q.name = String(prof.name);
                                onQuoteUpdate(sym, q);
                            }
                        }
                    } catch (_) {}
                }
            }
        }

        async function ensureQuote(ticker, optsEnsure) {
            const sym = Engine.normalizeTicker(ticker);
            if (!sym) {
                return { ok: false, reason: '股票代码格式无效：' + String(ticker || '') };
            }
            const o = optsEnsure || {};

            if (resolveEffectiveMode() === 'demo') {
                const q = seedDemoTicker(sym);
                return { ok: true, quote: q, staleNote: '' };
            }

            const negUntil = invalidUntilMap.get(sym) || 0;
            if (negUntil > Date.now()) {
                return { ok: false, reason: sym + ' 无有效美股报价（请确认美股代码）' };
            }
            if (!getApiKey()) {
                return { ok: false, reason: '尚未配置 Finnhub API Key' };
            }

            const maxAgeMs = o.forceFresh
                ? 2500
                : Math.max(Engine.clampNum(cfg.poll_interval_sec, 3, 120, 10) * 1000, 4000);
            const cached = quotesMap.get(sym);
            if (cached && cached.c > 0 && Date.now() - cached.fetchedAt <= maxAgeMs) {
                return { ok: true, quote: cached, staleNote: '' };
            }

            if (rateLimitUntilMs > Date.now()) {
                if (cached && cached.c > 0 && Date.now() - cached.fetchedAt <= 300000) {
                    return { ok: true, quote: cached, staleNote: '（限流缓存）' };
                }
                const remSec = Math.ceil((rateLimitUntilMs - Date.now()) / 1000);
                return { ok: false, reason: '触发 Finnhub 429 限流，请 ' + remSec + 's 后重试' };
            }

            try {
                const fresh = await enqueueQuoteFetch(sym, true);
                if (o.withMetrics) {
                    enrichSymbolMetadata(sym).catch(function () {});
                }
                return { ok: true, quote: fresh, staleNote: '' };
            } catch (err) {
                if (cached && cached.c > 0 && err && err.code === 'RATE_LIMITED') {
                    return { ok: true, quote: cached, staleNote: '（限流缓存）' };
                }
                if (err && err.code === 'INVALID_TICKER') {
                    return { ok: false, reason: '未找到美股代码 ' + sym + ' 的有效报价' };
                }
                if (err && err.code === 'INVALID_KEY') {
                    return { ok: false, reason: 'Finnhub API Key 无效，请在管理端核对' };
                }
                if (cached && cached.c > 0) {
                    return { ok: true, quote: cached, staleNote: '（缓存价）' };
                }
                return { ok: false, reason: sym + ' 行情获取失败，请稍后重试' };
            }
        }

        // -----------------------------------------------------------------
        // Finnhub WebSocket Live Trade Stream
        // -----------------------------------------------------------------
        function sendWsSub(type, symbol) {
            if (!ws || ws.readyState !== 1) return;
            try {
                ws.send(JSON.stringify({ type: type, symbol: symbol }));
            } catch (_) {}
        }

        function syncWsSubscriptions() {
            if (!ws || ws.readyState !== 1) return;
            const targetList = desiredSymbols.slice(0, MAX_WS_SUBS);
            const targetSet = new Set(targetList);

            subscribedSet.forEach(function (sym) {
                if (!targetSet.has(sym)) {
                    sendWsSub('unsubscribe', sym);
                    subscribedSet.delete(sym);
                }
            });

            targetList.forEach(function (sym) {
                if (!subscribedSet.has(sym)) {
                    sendWsSub('subscribe', sym);
                    subscribedSet.add(sym);
                }
            });
            notifyStatus();
        }

        function closeWs() {
            if (wsReconnectTimer !== null) {
                clearTimeout(wsReconnectTimer);
                wsReconnectTimer = null;
            }
            subscribedSet.clear();
            wsState = 'closed';
            if (ws) {
                try {
                    ws.onopen = null;
                    ws.onmessage = null;
                    ws.onerror = null;
                    ws.onclose = null;
                    ws.close();
                } catch (_) {}
                ws = null;
            }
        }

        function scheduleWsReconnect() {
            if (wsReconnectTimer !== null) return;
            if (resolveEffectiveMode() !== 'finnhub' || !cfg.enable_finnhub_ws || !getApiKey()) {
                return;
            }
            const delay = Math.min(30000, 1500 * Math.pow(1.6, Math.min(wsReconnectAttempt, 6)));
            wsReconnectAttempt += 1;
            wsReconnectTimer = setTimeout(function () {
                wsReconnectTimer = null;
                connectWsIfNeeded();
            }, delay);
        }

        function handleWsMessage(rawStr) {
            let msg;
            try {
                msg = JSON.parse(rawStr);
            } catch (_) {
                return;
            }
            if (!msg || msg.type !== 'trade' || !Array.isArray(msg.data)) return;

            const session = Engine.getUsMarketSession(Date.now(), cfg.market_session_override);
            const touched = new Map();
            for (let i = 0; i < msg.data.length; i++) {
                const tr = msg.data[i];
                if (!tr || !tr.s) continue;
                const sym = Engine.normalizeTicker(tr.s);
                const price = Number(tr.p);
                if (!sym || !Number.isFinite(price) || price <= 0) continue;
                const vol = Number(tr.v || 0);
                const tMs = Number(tr.t) > 0 ? Number(tr.t) : Date.now();
                const prev = touched.get(sym) || { p: price, volAdd: 0, t: tMs };
                prev.p = price;
                prev.volAdd += (Number.isFinite(vol) ? vol : 0);
                prev.t = tMs;
                touched.set(sym, prev);
            }

            touched.forEach(function (info, sym) {
                let q = quotesMap.get(sym);
                if (!q) {
                    q = seedDemoTicker(sym) || {
                        symbol: sym,
                        name: (Engine.STOCK_DIRECTORY[sym] && Engine.STOCK_DIRECTORY[sym].name) || sym,
                        c: info.p,
                        pc: info.p,
                        o: info.p,
                        h: info.p,
                        l: info.p,
                        d: 0,
                        dp: 0,
                        volume: 0,
                        w52Low: Engine.roundPrice(info.p * 0.78),
                        w52High: Engine.roundPrice(info.p * 1.24),
                        extPrice: null,
                        extLabel: '',
                        source: 'finnhub_ws',
                        fetchedAt: Date.now(),
                    };
                }
                q.c = Engine.roundPrice(info.p);
                if (q.h < q.c) q.h = q.c;
                if (q.l <= 0 || q.l > q.c) q.l = q.c;
                if (q.pc > 0) {
                    q.d = Engine.roundPrice(q.c - q.pc);
                    q.dp = Math.round(((q.c - q.pc) / q.pc) * 10000) / 100;
                }
                if (info.volAdd > 0) {
                    q.volume = Math.round((q.volume || 0) + info.volAdd);
                }
                if (session.code === 'pre') {
                    q.extPrice = q.c;
                    q.extLabel = '盘前';
                } else if (session.code === 'after') {
                    q.extPrice = q.c;
                    q.extLabel = '盘后';
                }
                q.source = 'finnhub_ws';
                q.fetchedAt = Date.now();
                quotesMap.set(sym, q);
                appendIntradayTick(sym, q.c, info.t, false);
                onQuoteUpdate(sym, q);
            });
        }

        function connectWsIfNeeded() {
            const shouldConnect =
                resolveEffectiveMode() === 'finnhub' &&
                Boolean(cfg.enable_finnhub_ws) &&
                Boolean(getApiKey()) &&
                typeof WebSocket === 'function';

            if (!shouldConnect) {
                closeWs();
                notifyStatus();
                return;
            }
            if (ws && (ws.readyState === 0 || ws.readyState === 1)) {
                return;
            }

            closeWs();
            wsState = 'connecting';
            notifyStatus();

            try {
                const socket = new WebSocket('wss://ws.finnhub.io?token=' + encodeURIComponent(getApiKey()));
                ws = socket;
                socket.onopen = function () {
                    if (ws !== socket) return;
                    wsState = 'open';
                    wsReconnectAttempt = 0;
                    apiState = 'live';
                    apiError = '';
                    syncWsSubscriptions();
                    notifyStatus();
                };
                socket.onmessage = function (ev) {
                    if (ws !== socket) return;
                    handleWsMessage(ev.data);
                };
                socket.onerror = function () {
                    if (ws !== socket) return;
                    wsState = 'closed';
                    notifyStatus();
                };
                socket.onclose = function () {
                    if (ws !== socket) return;
                    wsState = 'closed';
                    subscribedSet.clear();
                    ws = null;
                    notifyStatus();
                    scheduleWsReconnect();
                };
            } catch (_) {
                wsState = 'closed';
                ws = null;
                scheduleWsReconnect();
            }
        }

        // -----------------------------------------------------------------
        // Background REST Poller & Demo Tick Loop
        // -----------------------------------------------------------------
        function stepDemoTicks() {
            if (resolveEffectiveMode() !== 'demo') return;
            const list = desiredSymbols.length > 0 ? desiredSymbols : Engine.POPULAR_FALLBACK_POOL;
            const now = Date.now();
            const session = Engine.getUsMarketSession(now, cfg.market_session_override);
            const volScale = session.code === 'regular' ? 0.0018 : 0.0009;

            for (let i = 0; i < list.length; i++) {
                const sym = list[i];
                const q = seedDemoTicker(sym);
                if (!q) continue;
                const shock = (Math.random() - 0.495) * volScale;
                const nextP = Engine.roundPrice(Math.max(0.5, q.c * (1 + shock)));
                q.c = nextP;
                if (nextP > q.h) q.h = nextP;
                if (nextP < q.l) q.l = nextP;
                q.d = Engine.roundPrice(q.c - q.pc);
                q.dp = q.pc > 0 ? Math.round(((q.c - q.pc) / q.pc) * 10000) / 100 : 0;
                q.volume = Math.round((q.volume || 1000000) + Math.random() * 18000);
                if (session.code === 'pre') {
                    q.extPrice = q.c;
                    q.extLabel = '盘前';
                } else if (session.code === 'after') {
                    q.extPrice = q.c;
                    q.extLabel = '盘后';
                } else {
                    q.extPrice = Engine.roundPrice(q.c * (1 + (Math.random() - 0.5) * 0.002));
                    q.extLabel = session.code === 'closed' ? '盘后参考' : '盘后';
                }
                q.source = 'demo';
                q.fetchedAt = now;
                appendIntradayTick(sym, q.c, now, true);
                onQuoteUpdate(sym, q);
            }
        }

        function simulatePriceShock(symbol, pctDelta) {
            const sym = Engine.normalizeTicker(symbol);
            if (!sym) return null;
            const q = quotesMap.get(sym) || seedDemoTicker(sym);
            if (!q) return null;
            const ratio = 1 + Number(pctDelta || 0) / 100;
            const nextP = Engine.roundPrice(Math.max(0.5, q.c * ratio));
            q.c = nextP;
            if (nextP > q.h) q.h = nextP;
            if (nextP < q.l) q.l = nextP;
            q.d = Engine.roundPrice(q.c - q.pc);
            q.dp = q.pc > 0 ? Math.round(((q.c - q.pc) / q.pc) * 10000) / 100 : 0;
            q.fetchedAt = Date.now();
            appendIntradayTick(sym, q.c, q.fetchedAt, true);
            onQuoteUpdate(sym, q);
            return q;
        }

        function restartLoops() {
            if (pollTimer !== null) {
                clearInterval(pollTimer);
                pollTimer = null;
            }
            if (demoTimer !== null) {
                clearInterval(demoTimer);
                demoTimer = null;
            }

            const mode = resolveEffectiveMode();
            if (mode === 'demo') {
                closeWs();
                for (let i = 0; i < desiredSymbols.length; i++) {
                    seedDemoTicker(desiredSymbols[i]);
                }
                demoTimer = setInterval(stepDemoTicks, 1200);
                notifyStatus();
                return;
            }

            connectWsIfNeeded();
            const intervalMs = Engine.clampNum(cfg.poll_interval_sec, 3, 120, 10) * 1000;
            const tickPoll = function () {
                if (resolveEffectiveMode() !== 'finnhub' || !getApiKey()) return;
                if (rateLimitUntilMs > Date.now()) return;
                if (fetchQueue.length >= 4) return;
                if (desiredSymbols.length === 0) return;

                // Prioritize symbols that have never been fetched yet
                let targetSym = null;
                for (let i = 0; i < desiredSymbols.length; i++) {
                    const candidate = desiredSymbols[(pollCursor + i) % desiredSymbols.length];
                    if (!quotesMap.has(candidate)) {
                        targetSym = candidate;
                        pollCursor = (pollCursor + i + 1) % desiredSymbols.length;
                        break;
                    }
                }
                if (!targetSym) {
                    targetSym = desiredSymbols[pollCursor % desiredSymbols.length];
                    pollCursor = (pollCursor + 1) % desiredSymbols.length;
                }
                enqueueQuoteFetch(targetSym, false).catch(function () {});
            };
            pollTimer = setInterval(tickPoll, intervalMs);
            tickPoll();
        }

        function setConfig(nextCfg) {
            const prevKey = getApiKey();
            const prevMode = resolveEffectiveMode();
            const prevWs = Boolean(cfg.enable_finnhub_ws);
            const prevPoll = Engine.clampNum(cfg.poll_interval_sec, 3, 120, 10);

            cfg = Object.assign({}, Engine.DEFAULT_CONFIG, nextCfg || {});
            const nextKey = getApiKey();
            const nextMode = resolveEffectiveMode();
            const nextWs = Boolean(cfg.enable_finnhub_ws);
            const nextPoll = Engine.clampNum(cfg.poll_interval_sec, 3, 120, 10);

            if (nextKey !== prevKey) {
                apiError = '';
                apiState = nextMode === 'demo' ? 'demo' : 'live';
                rateLimitUntilMs = 0;
                invalidUntilMap.clear();
                closeWs();
            }

            if (
                pollTimer === null &&
                demoTimer === null ||
                nextKey !== prevKey ||
                nextMode !== prevMode ||
                nextWs !== prevWs ||
                nextPoll !== prevPoll
            ) {
                restartLoops();
            } else {
                notifyStatus();
            }
        }

        function setWatchUniverse(symbols) {
            const seen = new Set();
            const list = [];
            for (let i = 0; i < symbols.length; i++) {
                const sym = Engine.normalizeTicker(symbols[i]);
                if (sym && !seen.has(sym)) {
                    seen.add(sym);
                    list.push(sym);
                }
            }
            desiredSymbols = list;
            if (resolveEffectiveMode() === 'demo') {
                for (let i = 0; i < desiredSymbols.length; i++) {
                    seedDemoTicker(desiredSymbols[i]);
                }
            } else {
                syncWsSubscriptions();
            }
        }

        function hydrateQuotes(obj) {
            if (!obj || typeof obj !== 'object') return;
            Object.keys(obj).forEach(function (sym) {
                const q = obj[sym];
                if (q && Number(q.c) > 0) {
                    quotesMap.set(sym, q);
                    seedIntradayIfSparse(sym, q);
                }
            });
        }

        function destroy() {
            if (queueTimer !== null) clearTimeout(queueTimer);
            if (pollTimer !== null) clearInterval(pollTimer);
            if (demoTimer !== null) clearInterval(demoTimer);
            queueTimer = null;
            pollTimer = null;
            demoTimer = null;
            closeWs();
        }

        return {
            quotesMap: quotesMap,
            setConfig: setConfig,
            setWatchUniverse: setWatchUniverse,
            hydrateQuotes: hydrateQuotes,
            getQuote: getQuote,
            getSeries: getSeries,
            getSparklinePrices: getSparklinePrices,
            ensureQuote: ensureQuote,
            enrichSymbolMetadata: enrichSymbolMetadata,
            simulatePriceShock: simulatePriceShock,
            getTelemetry: getTelemetry,
            resolveEffectiveMode: resolveEffectiveMode,
            destroy: destroy,
        };
    }

    return {
        createMarketEngine: createMarketEngine,
    };
});
