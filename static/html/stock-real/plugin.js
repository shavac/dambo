(function () {
    'use strict';

    // =========================================================================
    // 1. 常量与默认配置
    // =========================================================================
    const TICKER_REGEX = /^[A-Za-z]{1,5}(\.[A-Za-z])?$/;
    const MAX_REQ_PER_MIN = 54; // 针对 Finnhub 免费版 60次/分钟 预留安全余量
    const MIN_SPACING_MS = 1100;
    const NEGATIVE_CACHE_MS = 60000;
    const RATE_LIMIT_COOLDOWN_MS = 30000;
    const RECENT_TICKER_TTL_MS = 600000;
    const MAX_FEED_ITEMS = 30;
    const MAX_KV_KEY_BYTES = 128;

    const FALLBACK_CONFIG = {
        finnhub_api_key: '',
        initial_balance: 100000,
        poll_interval_sec: 5,
        watchlist_default: 'NVDA,AAPL,TSLA,MSFT,SPY',
        top_n: 6,
        trade_cooldown_ms: 1500,
    };

    // =========================================================================
    // 2. DOM 引用
    // =========================================================================
    const appShellEl = document.getElementById('app-shell');
    const marketStatusPillEl = document.getElementById('market-status-pill');
    const wsBadgeEl = document.getElementById('ws-badge');
    const alertBannerEl = document.getElementById('alert-banner');
    const alertTextEl = document.getElementById('alert-text');
    const quoteMetaTextEl = document.getElementById('quote-meta-text');
    const quoteGridEl = document.getElementById('quote-grid');
    const feedMetaTextEl = document.getElementById('feed-meta-text');
    const tradeFeedListEl = document.getElementById('trade-feed-list');
    const rankMetaTextEl = document.getElementById('rank-meta-text');
    const leaderboardListEl = document.getElementById('leaderboard-list');
    const statsPillEl = document.getElementById('stats-pill');

    if (Dambo.utils.getQueryBool('safe_top', true) === false) {
        appShellEl.classList.add('no-safe-top');
    }

    // =========================================================================
    // 3. 运行时状态
    // =========================================================================
    let cfg = Object.assign({}, FALLBACK_CONFIG);
    /** @type {Map<string, any>} */
    const accountsMap = new Map();
    /** @type {Map<string, any>} */
    const quotesMap = new Map();
    /** @type {Map<string, number>} */
    const recentTickersMap = new Map();
    /** @type {Map<string, number>} */
    const invalidTickerUntil = new Map();
    /** @type {Array<any>} */
    let recentFeed = [];

    /** @type {Map<string, number>} */
    const userCooldownMap = new Map();
    /** @type {Set<string>} */
    const userBusySet = new Set();

    // Finnhub 请求队列与限流状态
    const requestTimestamps = [];
    const fetchQueue = [];
    const queuedTickers = new Map();
    const inFlightFetches = new Map();
    let queueDrainTimer = null;
    let pollTimer = null;
    let pollCursor = 0;
    let statusCountdownTimer = null;
    let lastApiState = 'no_key'; // 'no_key' | 'live' | 'rate_limited' | 'invalid_key' | 'error'
    let lastApiError = '';
    let rateLimitUntilMs = 0;
    let runtimeSyncTimer = null;

    // =========================================================================
    // 4. 通用格式化与安全工具
    // =========================================================================
    function fnv1aHex(str) {
        let h = 2166136261;
        for (let i = 0; i < str.length; i++) {
            h ^= str.charCodeAt(i);
            h = Math.imul(h, 16777619);
        }
        return (h >>> 0).toString(16).padStart(8, '0');
    }

    function utf8ByteLength(str) {
        if (typeof TextEncoder === 'function') {
            return new TextEncoder().encode(str).length;
        }
        return unescape(encodeURIComponent(str)).length;
    }

    function sanitizeKvKey(raw, prefixFallback) {
        let s = String(raw || '').replace(/[\u0000-\u001f\u007f/\\]/g, '_').trim();
        if (!s || s === '.' || s === '..') {
            s = (prefixFallback || 'acct') + '_' + fnv1aHex(String(raw || 'anon'));
        }
        if (utf8ByteLength(s) <= MAX_KV_KEY_BYTES) return s;
        const suffix = '_' + fnv1aHex(s);
        let cut = s.slice(0, 64);
        while (cut.length > 1 && utf8ByteLength(cut + suffix) > MAX_KV_KEY_BYTES) {
            cut = cut.slice(0, -1);
        }
        return cut + suffix;
    }

    function roundMoney(v) {
        if (!Number.isFinite(v)) return 0;
        return Math.round(v * 100) / 100;
    }

    function roundPrice(v) {
        if (!Number.isFinite(v)) return 0;
        return Math.round(v * 10000) / 10000;
    }

    function clampNum(v, min, max, fallback) {
        const n = Number(v);
        if (!Number.isFinite(n)) return fallback;
        return Math.min(max, Math.max(min, n));
    }

    function formatPoints(val) {
        const num = Number(val || 0);
        return num.toLocaleString('en-US', {
            minimumFractionDigits: 0,
            maximumFractionDigits: 2,
        });
    }

    function formatPrice(val) {
        const num = Number(val || 0);
        return '$' + num.toFixed(num >= 1000 ? 1 : 2);
    }

    function formatPct(pct) {
        const n = Number(pct || 0);
        const sign = n > 0 ? '+' : '';
        return sign + n.toFixed(2) + '%';
    }

    function normalizeTicker(raw) {
        const t = String(raw || '').trim().toUpperCase();
        return TICKER_REGEX.test(t) ? t : null;
    }

    function getDefaultWatchlist() {
        const raw = String(cfg.watchlist_default || 'NVDA,AAPL,TSLA,MSFT,SPY');
        const seen = new Set();
        const list = [];
        raw.split(/[\s,，]+/).forEach(function (part) {
            const t = normalizeTicker(part);
            if (t && !seen.has(t)) {
                seen.add(t);
                list.push(t);
            }
        });
        return list.slice(0, 12);
    }

    function getActiveTickers() {
        const now = Date.now();
        const held = [];
        const seen = new Set();

        accountsMap.forEach(function (acct) {
            const pos = acct && acct.positions;
            if (!pos || typeof pos !== 'object') return;
            Object.keys(pos).forEach(function (sym) {
                const p = pos[sym];
                if (p && Number(p.shares) > 0) {
                    const t = normalizeTicker(sym);
                    if (t && !seen.has(t)) {
                        seen.add(t);
                        held.push(t);
                    }
                }
            });
        });

        recentTickersMap.forEach(function (ts, sym) {
            if (now - ts > RECENT_TICKER_TTL_MS) {
                recentTickersMap.delete(sym);
                return;
            }
            if (!seen.has(sym)) {
                seen.add(sym);
                held.push(sym);
            }
        });

        const defaults = getDefaultWatchlist();
        for (let i = 0; i < defaults.length; i++) {
            if (!seen.has(defaults[i])) {
                seen.add(defaults[i]);
                held.push(defaults[i]);
            }
        }
        return held;
    }

    function accountKeyForPlayer(player) {
        const uidNum = Number(player && player.uid);
        if (Number.isFinite(uidNum) && uidNum > 0) {
            return 'uid:' + Math.floor(uidNum);
        }
        const uname = String((player && player.uname) || '观众').trim() || '观众';
        return sanitizeKvKey('name:' + uname, 'name');
    }

    function calculateAccountEquity(acct) {
        const cash = Number(acct.cash || 0);
        let holdingsVal = 0;
        const holdingSummary = [];
        const positions = acct.positions && typeof acct.positions === 'object' ? acct.positions : {};
        Object.keys(positions).forEach(function (sym) {
            const p = positions[sym];
            if (!p || Number(p.shares) <= 0) return;
            const q = quotesMap.get(sym);
            const markPrice = q && Number(q.c) > 0 ? Number(q.c) : Number(p.cost || 0);
            holdingsVal += Number(p.shares) * markPrice;
            holdingSummary.push({
                ticker: sym,
                shares: Number(p.shares),
                cost: Number(p.cost || 0),
                price: markPrice,
            });
        });

        const totalEquity = roundMoney(cash + holdingsVal);
        const initBal = Number(acct.initial_balance || cfg.initial_balance || 100000);
        const pnl = roundMoney(totalEquity - initBal);
        const pnlPct = initBal > 0 ? ((pnl / initBal) * 100) : 0;

        return {
            cash: roundMoney(cash),
            holdingsVal: roundMoney(holdingsVal),
            totalEquity: totalEquity,
            initialBalance: initBal,
            pnl: pnl,
            pnlPct: pnlPct,
            holdings: holdingSummary,
        };
    }

    // =========================================================================
    // 5. Finnhub 美股行情引擎（60 req/min 队列节流 + 本地缓存）
    // =========================================================================
    function pruneRequestWindow(now) {
        while (requestTimestamps.length > 0 && now - requestTimestamps[0] >= 60000) {
            requestTimestamps.shift();
        }
    }

    function getApiKey() {
        return String(cfg.finnhub_api_key || '').trim();
    }

    function computeWaitBeforeNextRequestMs() {
        const now = Date.now();
        if (rateLimitUntilMs > now) {
            return rateLimitUntilMs - now;
        }
        pruneRequestWindow(now);
        let waitMs = 0;
        if (requestTimestamps.length >= MAX_REQ_PER_MIN) {
            waitMs = Math.max(waitMs, (requestTimestamps[0] + 60050) - now);
        }
        if (requestTimestamps.length > 0) {
            const sinceLast = now - requestTimestamps[requestTimestamps.length - 1];
            if (sinceLast < MIN_SPACING_MS) {
                waitMs = Math.max(waitMs, MIN_SPACING_MS - sinceLast);
            }
        }
        return waitMs;
    }

    function updateStatusUI() {
        const now = Date.now();
        const key = getApiKey();
        pruneRequestWindow(now);

        if (!key) {
            lastApiState = 'no_key';
            marketStatusPillEl.className = 'status-pill warn';
            marketStatusPillEl.textContent = '未配置 Key';
            alertBannerEl.className = 'alert-banner';
            alertBannerEl.classList.remove('hidden');
            alertTextEl.textContent = '未配置 Finnhub API Key，请在管理面 /stock-real/m 填入免费 Key 后开启美股真实报价';
            quoteMetaTextEl.textContent = '等待配置 Key';
            scheduleRuntimeSync();
            return;
        }

        if (rateLimitUntilMs > now) {
            const remSec = Math.ceil((rateLimitUntilMs - now) / 1000);
            lastApiState = 'rate_limited';
            marketStatusPillEl.className = 'status-pill error';
            marketStatusPillEl.textContent = '限流冷却 ' + remSec + 's';
            alertBannerEl.className = 'alert-banner is-error';
            alertBannerEl.classList.remove('hidden');
            alertTextEl.textContent = '触发 Finnhub API 429 限流保护（60次/分），冷却 ' + remSec + 's 后自动恢复轮询';
            quoteMetaTextEl.textContent = '冷却中 · 仅使用本地缓存报价';
            if (!statusCountdownTimer) {
                statusCountdownTimer = setInterval(function () {
                    if (Date.now() >= rateLimitUntilMs) {
                        clearInterval(statusCountdownTimer);
                        statusCountdownTimer = null;
                        lastApiState = 'live';
                        scheduleDrain();
                    }
                    updateStatusUI();
                }, 1000);
            }
            scheduleRuntimeSync();
            return;
        }

        if (statusCountdownTimer) {
            clearInterval(statusCountdownTimer);
            statusCountdownTimer = null;
        }

        if (lastApiState === 'invalid_key') {
            marketStatusPillEl.className = 'status-pill error';
            marketStatusPillEl.textContent = 'Key 无效';
            alertBannerEl.className = 'alert-banner is-error';
            alertBannerEl.classList.remove('hidden');
            alertTextEl.textContent = lastApiError || 'Finnhub API Key 无效或未通过认证，请在 /stock-real/m 核对';
        } else if (lastApiState === 'error') {
            marketStatusPillEl.className = 'status-pill warn';
            marketStatusPillEl.textContent = '网络异常';
            alertBannerEl.className = 'alert-banner';
            alertBannerEl.classList.remove('hidden');
            alertTextEl.textContent = lastApiError || '行情请求暂时异常，正在自动重试';
        } else {
            lastApiState = 'live';
            marketStatusPillEl.className = 'status-pill ok';
            marketStatusPillEl.textContent = 'Finnhub 实时';
            alertBannerEl.classList.add('hidden');
        }

        quoteMetaTextEl.textContent = '轮询 ' + clampNum(cfg.poll_interval_sec, 2, 120, 5) + 's · 近1分 ' + requestTimestamps.length + '/' + MAX_REQ_PER_MIN + ' 次';
        scheduleRuntimeSync();
    }

    async function executeFinnhubFetch(ticker) {
        const key = getApiKey();
        if (!key) {
            throw new Error('NO_KEY');
        }

        const now = Date.now();
        requestTimestamps.push(now);
        updateStatusUI();

        const url = 'https://finnhub.io/api/v1/quote?symbol=' +
            encodeURIComponent(ticker) +
            '&token=' +
            encodeURIComponent(key);

        let res;
        try {
            res = await fetch(url, { method: 'GET' });
        } catch (netErr) {
            lastApiState = 'error';
            lastApiError = '连接 Finnhub 网络失败：' + (netErr && netErr.message ? netErr.message : '请检查网络');
            updateStatusUI();
            throw netErr;
        }

        if (res.status === 429) {
            rateLimitUntilMs = Date.now() + RATE_LIMIT_COOLDOWN_MS;
            lastApiState = 'rate_limited';
            updateStatusUI();
            const e429 = new Error('HTTP_429');
            e429.code = 'RATE_LIMITED';
            throw e429;
        }

        if (res.status === 401 || res.status === 403) {
            lastApiState = 'invalid_key';
            lastApiError = 'Finnhub API Key 被拒绝（HTTP ' + res.status + '），请在管理端检查 Key';
            updateStatusUI();
            const eAuth = new Error('HTTP_AUTH');
            eAuth.code = 'INVALID_KEY';
            throw eAuth;
        }

        if (!res.ok) {
            lastApiState = 'error';
            lastApiError = 'Finnhub 返回异常 HTTP ' + res.status;
            updateStatusUI();
            throw new Error('HTTP_' + res.status);
        }

        const data = await res.json();
        const price = Number(data && data.c);
        const prevClose = Number(data && data.pc);
        if (!Number.isFinite(price) || price <= 0) {
            invalidTickerUntil.set(ticker, Date.now() + NEGATIVE_CACHE_MS);
            lastApiState = 'live';
            updateStatusUI();
            const eSym = new Error('INVALID_TICKER');
            eSym.code = 'INVALID_TICKER';
            throw eSym;
        }

        let dp = Number(data && data.dp);
        if (!Number.isFinite(dp)) {
            dp = prevClose > 0 ? ((price - prevClose) / prevClose) * 100 : 0;
        }

        const quote = {
            symbol: ticker,
            c: roundPrice(price),
            pc: roundPrice(Number.isFinite(prevClose) ? prevClose : price),
            dp: Math.round(dp * 100) / 100,
            fetchedAt: Date.now(),
        };

        lastApiState = 'live';
        lastApiError = '';
        quotesMap.set(ticker, quote);
        Dambo.memory.set('quotes', ticker, quote).catch(function () {});
        renderAllUI();
        updateStatusUI();
        return quote;
    }

    function enqueueQuoteFetch(ticker, priorityHigh) {
        if (inFlightFetches.has(ticker)) {
            return inFlightFetches.get(ticker);
        }
        if (queuedTickers.has(ticker)) {
            const existingTask = queuedTickers.get(ticker);
            if (priorityHigh && !existingTask.priorityHigh) {
                existingTask.priorityHigh = true;
                const idx = fetchQueue.indexOf(existingTask);
                if (idx > 0) {
                    fetchQueue.splice(idx, 1);
                    fetchQueue.unshift(existingTask);
                }
            }
            return existingTask.promise;
        }

        let resolveFn;
        let rejectFn;
        const promise = new Promise(function (resolve, reject) {
            resolveFn = resolve;
            rejectFn = reject;
        });

        const task = {
            ticker: ticker,
            priorityHigh: Boolean(priorityHigh),
            resolve: resolveFn,
            reject: rejectFn,
            promise: promise,
        };

        queuedTickers.set(ticker, task);
        if (priorityHigh) {
            fetchQueue.unshift(task);
        } else {
            fetchQueue.push(task);
        }
        scheduleDrain();
        return promise;
    }

    function scheduleDrain() {
        if (queueDrainTimer !== null || fetchQueue.length === 0) return;
        const delay = Math.max(10, computeWaitBeforeNextRequestMs());
        queueDrainTimer = setTimeout(drainNextQueueTask, delay);
    }

    function drainNextQueueTask() {
        queueDrainTimer = null;
        if (fetchQueue.length === 0) return;

        if (!getApiKey()) {
            while (fetchQueue.length > 0) {
                const t = fetchQueue.shift();
                queuedTickers.delete(t.ticker);
                const e = new Error('NO_KEY');
                e.code = 'NO_KEY';
                t.reject(e);
            }
            updateStatusUI();
            return;
        }

        const waitMs = computeWaitBeforeNextRequestMs();
        if (waitMs > 0) {
            scheduleDrain();
            return;
        }

        const task = fetchQueue.shift();
        queuedTickers.delete(task.ticker);

        const p = executeFinnhubFetch(task.ticker)
            .then(function (q) {
                inFlightFetches.delete(task.ticker);
                task.resolve(q);
                return q;
            })
            .catch(function (err) {
                inFlightFetches.delete(task.ticker);
                task.reject(err);
                throw err;
            })
            .finally(function () {
                if (fetchQueue.length > 0) scheduleDrain();
            });

        inFlightFetches.set(task.ticker, p);
    }

    async function fetchQuoteForTrade(ticker) {
        const sym = normalizeTicker(ticker);
        if (!sym) {
            return { ok: false, reason: '股票代码格式无效：' + String(ticker || '') };
        }
        const negUntil = invalidTickerUntil.get(sym) || 0;
        if (negUntil > Date.now()) {
            return { ok: false, reason: sym + ' 无有效美股报价（请确认代码拼写）' };
        }
        if (!getApiKey()) {
            return { ok: false, reason: '尚未配置 Finnhub API Key，无法获取美股报价' };
        }

        const freshTtlMs = Math.max(clampNum(cfg.poll_interval_sec, 2, 120, 5) * 1000, 4000);
        const cached = quotesMap.get(sym);
        if (cached && cached.c > 0 && Date.now() - cached.fetchedAt <= freshTtlMs) {
            return { ok: true, quote: cached, staleNote: '' };
        }

        if (rateLimitUntilMs > Date.now()) {
            if (cached && cached.c > 0 && Date.now() - cached.fetchedAt <= 300000) {
                return { ok: true, quote: cached, staleNote: '（限流缓存价）' };
            }
            const remSec = Math.ceil((rateLimitUntilMs - Date.now()) / 1000);
            return { ok: false, reason: '触发 Finnhub 限流，请在 ' + remSec + 's 后重试' };
        }

        try {
            const fresh = await enqueueQuoteFetch(sym, true);
            return { ok: true, quote: fresh, staleNote: '' };
        } catch (err) {
            if (cached && cached.c > 0 && err && err.code === 'RATE_LIMITED' && Date.now() - cached.fetchedAt <= 300000) {
                return { ok: true, quote: cached, staleNote: '（限流缓存价）' };
            }
            if (err && err.code === 'INVALID_TICKER') {
                return { ok: false, reason: '未找到美股代码 ' + sym + ' 的有效报价' };
            }
            if (err && err.code === 'RATE_LIMITED') {
                return { ok: false, reason: '行情请求触及 429 限流，请稍后再试' };
            }
            if (err && err.code === 'INVALID_KEY') {
                return { ok: false, reason: 'Finnhub API Key 无效，请在控制面板更新' };
            }
            return { ok: false, reason: sym + ' 行情获取失败，请稍后重试' };
        }
    }

    function restartBackgroundPoller() {
        if (pollTimer !== null) {
            clearInterval(pollTimer);
            pollTimer = null;
        }
        const intervalMs = clampNum(cfg.poll_interval_sec, 2, 120, 5) * 1000;
        const tickPoll = function () {
            if (!getApiKey()) return;
            if (rateLimitUntilMs > Date.now()) return;
            if (fetchQueue.length >= 3) return;
            const list = getActiveTickers();
            if (list.length === 0) return;
            const sym = list[pollCursor % list.length];
            pollCursor = (pollCursor + 1) % list.length;
            enqueueQuoteFetch(sym, false).catch(function () {});
        };
        pollTimer = setInterval(tickPoll, intervalMs);
        tickPoll();
    }

    // =========================================================================
    // 6. 中英文双语弹幕指令解析与模拟交易引擎
    // =========================================================================
    function parseDanmakuCommand(rawText) {
        if (typeof rawText !== 'string') return null;
        const text = rawText.replace(/[\s\u3000]+/g, ' ').trim();
        if (!text || text.length > 64) return null;

        if (/^(?:开户|查询|账户|持仓|open|info|pos|balance)$/i.test(text)) {
            return { kind: 'open' };
        }

        let m = text.match(/^(?:(?:买入|买)\s*|buy\s+)([A-Za-z]{1,5}(?:\.[A-Za-z])?)\s+(\d+)$/i);
        if (m) {
            const sym = normalizeTicker(m[1]);
            const qty = parseInt(m[2], 10);
            if (!sym) return null;
            return { kind: 'buy', ticker: sym, shares: qty };
        }

        m = text.match(/^(?:(?:全仓|梭哈)\s*|(?:allin|all-in|max)\s+)([A-Za-z]{1,5}(?:\.[A-Za-z])?)$/i);
        if (m) {
            const sym = normalizeTicker(m[1]);
            if (!sym) return null;
            return { kind: 'allin', ticker: sym };
        }

        m = text.match(/^(?:(?:卖出|卖)\s*|sell\s+)([A-Za-z]{1,5}(?:\.[A-Za-z])?)\s+(\d+)$/i);
        if (m) {
            const sym = normalizeTicker(m[1]);
            const qty = parseInt(m[2], 10);
            if (!sym) return null;
            return { kind: 'sell', ticker: sym, shares: qty };
        }

        m = text.match(/^(?:(?:清仓|平仓)\s*|(?:close|exit|sellall)\s+)([A-Za-z]{1,5}(?:\.[A-Za-z])?)$/i);
        if (m) {
            const sym = normalizeTicker(m[1]);
            if (!sym) return null;
            return { kind: 'close', ticker: sym };
        }

        return null;
    }

    function createInitialAccount(player) {
        const initBal = clampNum(cfg.initial_balance, 1000, 100000000, 100000);
        const now = Date.now();
        return {
            uid: Number(player.uid || 0),
            name: player.uname || '观众',
            face: player.face || '',
            cash: initBal,
            initial_balance: initBal,
            positions: {},
            realized_pnl: 0,
            trade_count: 0,
            created_at: now,
            updated_at: now,
        };
    }

    function pushTradeFeed(eventObj) {
        recentFeed.unshift(eventObj);
        if (recentFeed.length > MAX_FEED_ITEMS) {
            recentFeed = recentFeed.slice(0, MAX_FEED_ITEMS);
        }
        Dambo.topics.publish('trade_feed', eventObj, {
            public: true,
            description: '美股模拟盘成交与账户查账播报流',
        }).catch(function () {});
        Dambo.memory.set('runtime', 'recent_feed', recentFeed).catch(function () {});
        renderAllUI();
        scheduleRuntimeSync();
    }

    async function handleTradeCommand(player, cmd) {
        const acctKey = accountKeyForPlayer(player);
        const now = Date.now();
        const cooldownMs = clampNum(cfg.trade_cooldown_ms, 0, 10000, 1500);
        const lastUsed = userCooldownMap.get(acctKey) || 0;
        if (now - lastUsed < cooldownMs || userBusySet.has(acctKey)) {
            return;
        }
        userCooldownMap.set(acctKey, now);
        userBusySet.add(acctKey);

        try {
            let acct = accountsMap.get(acctKey);
            let isFirstOpen = false;
            if (!acct || typeof acct !== 'object') {
                acct = createInitialAccount(player);
                isFirstOpen = true;
            } else {
                acct = JSON.parse(JSON.stringify(acct));
                if (player.uname) acct.name = player.uname;
                if (player.face) acct.face = player.face;
                if (!acct.positions || typeof acct.positions !== 'object') acct.positions = {};
            }

            if (cmd.kind === 'open') {
                if (isFirstOpen) {
                    acct.updated_at = Date.now();
                    accountsMap.set(acctKey, acct);
                    await Dambo.storage.set('accounts', acctKey, acct);
                }
                const eq = calculateAccountEquity(acct);
                const posDesc = eq.holdings.length > 0
                    ? eq.holdings.map(function (h) { return h.ticker + '×' + h.shares; }).join(' ')
                    : '空仓';
                const summary = isFirstOpen
                    ? acct.name + ' 开户成功！初始 ' + formatPoints(acct.cash) + ' 模拟积分'
                    : acct.name + ' 可用 ' + formatPoints(eq.cash) + ' · ' + posDesc + ' · 收益 ' + formatPct(eq.pnlPct);

                pushTradeFeed({
                    id: acctKey + '_' + Date.now(),
                    type: isFirstOpen ? 'open' : 'info',
                    ok: true,
                    uid: acct.uid,
                    name: acct.name,
                    face: acct.face,
                    summary: summary,
                    ts: Date.now(),
                });
                renderAllUI();
                return;
            }

            if ((cmd.kind === 'buy' || cmd.kind === 'sell') && (!Number.isSafeInteger(cmd.shares) || cmd.shares < 1)) {
                pushTradeFeed({
                    id: acctKey + '_' + Date.now(),
                    type: cmd.kind,
                    ok: false,
                    uid: acct.uid,
                    name: acct.name,
                    face: acct.face,
                    summary: acct.name + ' 下单数量至少为 1 股',
                    ts: Date.now(),
                });
                return;
            }

            // 如果是卖出或清仓，先确认持仓存在避免浪费 API 请求
            const existingPos = acct.positions[cmd.ticker];
            if ((cmd.kind === 'sell' || cmd.kind === 'close') && (!existingPos || Number(existingPos.shares) <= 0)) {
                pushTradeFeed({
                    id: acctKey + '_' + Date.now(),
                    type: cmd.kind,
                    ok: false,
                    uid: acct.uid,
                    name: acct.name,
                    face: acct.face,
                    summary: acct.name + ' 未持有 ' + cmd.ticker + '，无法卖出',
                    ts: Date.now(),
                });
                return;
            }

            if (cmd.kind === 'sell' && Number(existingPos.shares) < cmd.shares) {
                pushTradeFeed({
                    id: acctKey + '_' + Date.now(),
                    type: 'sell',
                    ok: false,
                    uid: acct.uid,
                    name: acct.name,
                    face: acct.face,
                    summary: acct.name + ' 仅持有 ' + existingPos.shares + ' 股 ' + cmd.ticker + '，不足 ' + cmd.shares + ' 股',
                    ts: Date.now(),
                });
                return;
            }

            recentTickersMap.set(cmd.ticker, Date.now());
            const qRes = await fetchQuoteForTrade(cmd.ticker);
            if (!qRes.ok) {
                pushTradeFeed({
                    id: acctKey + '_' + Date.now(),
                    type: cmd.kind,
                    ok: false,
                    uid: acct.uid,
                    name: acct.name,
                    face: acct.face,
                    summary: acct.name + ' 操作 ' + cmd.ticker + ' 失败：' + qRes.reason,
                    ts: Date.now(),
                });
                return;
            }

            const price = qRes.quote.c;
            const staleNote = qRes.staleNote || '';

            if (cmd.kind === 'buy' || cmd.kind === 'allin') {
                const targetShares = cmd.kind === 'allin'
                    ? Math.floor((Number(acct.cash) + 1e-9) / price)
                    : cmd.shares;

                if (targetShares < 1) {
                    pushTradeFeed({
                        id: acctKey + '_' + Date.now(),
                        type: cmd.kind,
                        ok: false,
                        uid: acct.uid,
                        name: acct.name,
                        face: acct.face,
                        summary: acct.name + ' 可用 ' + formatPoints(acct.cash) + ' 不足买入 1 股 ' + cmd.ticker + ' (' + formatPrice(price) + ')',
                        ts: Date.now(),
                    });
                    return;
                }

                const totalCost = roundMoney(targetShares * price);
                if (Number(acct.cash) + 1e-6 < totalCost) {
                    pushTradeFeed({
                        id: acctKey + '_' + Date.now(),
                        type: cmd.kind,
                        ok: false,
                        uid: acct.uid,
                        name: acct.name,
                        face: acct.face,
                        summary: acct.name + ' 可用模拟积分不足（需 ' + formatPoints(totalCost) + '，可用 ' + formatPoints(acct.cash) + '）',
                        ts: Date.now(),
                    });
                    return;
                }

                const prevShares = existingPos ? Number(existingPos.shares) : 0;
                const prevCost = existingPos ? Number(existingPos.cost) : 0;
                const nextShares = prevShares + targetShares;
                const weightedCost = roundPrice(((prevShares * prevCost) + totalCost) / nextShares);

                acct.cash = roundMoney(Math.max(0, Number(acct.cash) - totalCost));
                acct.positions[cmd.ticker] = {
                    ticker: cmd.ticker,
                    shares: nextShares,
                    cost: weightedCost,
                };
                acct.trade_count = Number(acct.trade_count || 0) + 1;
                acct.updated_at = Date.now();

                accountsMap.set(acctKey, acct);
                await Dambo.storage.set('accounts', acctKey, acct);

                const prefixWord = cmd.kind === 'allin' ? '全仓买入' : '买入';
                pushTradeFeed({
                    id: acctKey + '_' + Date.now(),
                    type: cmd.kind,
                    ok: true,
                    uid: acct.uid,
                    name: acct.name,
                    face: acct.face,
                    ticker: cmd.ticker,
                    shares: targetShares,
                    price: price,
                    amount: totalCost,
                    summary: acct.name + ' ' + prefixWord + ' ' + cmd.ticker + ' ×' + targetShares + ' @ ' + formatPrice(price) + staleNote,
                    ts: Date.now(),
                });
                return;
            }

            if (cmd.kind === 'sell' || cmd.kind === 'close') {
                const sellShares = cmd.kind === 'close' ? Number(existingPos.shares) : cmd.shares;
                const proceeds = roundMoney(sellShares * price);
                const costBasis = roundMoney(sellShares * Number(existingPos.cost));
                const pnl = roundMoney(proceeds - costBasis);
                const pnlPct = costBasis > 0 ? ((pnl / costBasis) * 100) : 0;

                acct.cash = roundMoney(Number(acct.cash) + proceeds);
                acct.realized_pnl = roundMoney(Number(acct.realized_pnl || 0) + pnl);
                const remaining = Number(existingPos.shares) - sellShares;
                if (remaining <= 0) {
                    delete acct.positions[cmd.ticker];
                } else {
                    acct.positions[cmd.ticker] = {
                        ticker: cmd.ticker,
                        shares: remaining,
                        cost: Number(existingPos.cost),
                    };
                }
                acct.trade_count = Number(acct.trade_count || 0) + 1;
                acct.updated_at = Date.now();

                accountsMap.set(acctKey, acct);
                await Dambo.storage.set('accounts', acctKey, acct);

                const prefixWord = cmd.kind === 'close' ? '清仓' : '卖出';
                const pnlLabel = (pnl >= 0 ? '盈 +' : '亏 ') + formatPoints(pnl) + ' (' + formatPct(pnlPct) + ')';
                pushTradeFeed({
                    id: acctKey + '_' + Date.now(),
                    type: cmd.kind,
                    ok: true,
                    uid: acct.uid,
                    name: acct.name,
                    face: acct.face,
                    ticker: cmd.ticker,
                    shares: sellShares,
                    price: price,
                    amount: proceeds,
                    pnl: pnl,
                    pnlPct: Math.round(pnlPct * 100) / 100,
                    summary: acct.name + ' ' + prefixWord + ' ' + cmd.ticker + ' ×' + sellShares + ' @ ' + formatPrice(price) + ' · ' + pnlLabel + staleNote,
                    ts: Date.now(),
                });
            }
        } catch (err) {
            console.error('[stock-real] trade command failed:', err);
        } finally {
            userBusySet.delete(acctKey);
        }
    }

    // =========================================================================
    // 7. 排行聚合与 runtime 内存镜像
    // =========================================================================
    function computeLeaderboard() {
        const list = [];
        accountsMap.forEach(function (acct, key) {
            if (!acct || typeof acct !== 'object') return;
            const eq = calculateAccountEquity(acct);
            list.push({
                key: key,
                uid: Number(acct.uid || 0),
                name: acct.name || '观众',
                face: acct.face || '',
                cash: eq.cash,
                holdingsVal: eq.holdingsVal,
                totalEquity: eq.totalEquity,
                initialBalance: eq.initialBalance,
                pnl: eq.pnl,
                pnlPct: Math.round(eq.pnlPct * 100) / 100,
                holdings: eq.holdings,
                updatedAt: Number(acct.updated_at || 0),
            });
        });

        list.sort(function (a, b) {
            if (b.totalEquity !== a.totalEquity) return b.totalEquity - a.totalEquity;
            return b.pnlPct - a.pnlPct;
        });

        for (let i = 0; i < list.length; i++) {
            list[i].rank = i + 1;
        }
        return list;
    }

    function scheduleRuntimeSync() {
        if (runtimeSyncTimer !== null) return;
        runtimeSyncTimer = setTimeout(function () {
            runtimeSyncTimer = null;
            const ranked = computeLeaderboard();
            const now = Date.now();
            pruneRequestWindow(now);
            const statusObj = {
                state: lastApiState,
                error: lastApiError,
                rate_limit_until: rateLimitUntilMs,
                requests_last_min: requestTimestamps.length,
                active_tickers: getActiveTickers(),
                updated_at: now,
            };
            Dambo.memory.mset('runtime', {
                status: statusObj,
                leaderboard: ranked.slice(0, 50),
                recent_feed: recentFeed.slice(0, 30),
            }).catch(function () {});
        }, 250);
    }

    // =========================================================================
    // 8. DOM 渲染（使用安全节点操作，禁用模板拼接）
    // =========================================================================
    function renderQuotes() {
        const active = getActiveTickers().slice(0, 8);
        const frag = document.createDocumentFragment();

        for (let i = 0; i < active.length; i++) {
            const sym = active[i];
            const q = quotesMap.get(sym);
            const hasPrice = q && Number(q.c) > 0;
            const dp = hasPrice ? Number(q.dp || 0) : 0;
            const dirClass = !hasPrice ? 'flat' : (dp > 0.001 ? 'up' : (dp < -0.001 ? 'down' : 'flat'));

            const card = document.createElement('div');
            card.className = 'quote-chip ' + dirClass;

            const top = document.createElement('div');
            top.className = 'quote-top';
            const symEl = document.createElement('span');
            symEl.className = 'quote-sym';
            symEl.textContent = sym;
            const pctEl = document.createElement('span');
            pctEl.className = 'quote-pct';
            pctEl.textContent = hasPrice ? ((dp > 0 ? '▲' : (dp < 0 ? '▼' : '')) + formatPct(dp)) : '--';
            top.appendChild(symEl);
            top.appendChild(pctEl);
            card.appendChild(top);

            const priceEl = document.createElement('div');
            priceEl.className = 'quote-price';
            priceEl.textContent = hasPrice ? formatPrice(q.c) : '$--.--';
            card.appendChild(priceEl);

            frag.appendChild(card);
        }

        quoteGridEl.replaceChildren(frag);
        statsPillEl.textContent = accountsMap.size + ' 户 · ' + active.length + ' 标的';
    }

    function feedTagInfo(ev) {
        if (!ev.ok) return { text: '提示', cls: 'warn' };
        if (ev.type === 'buy' || ev.type === 'allin') return { text: ev.type === 'allin' ? '全仓' : '买入', cls: 'buy' };
        if (ev.type === 'sell' || ev.type === 'close') return { text: ev.type === 'close' ? '清仓' : '卖出', cls: 'sell' };
        if (ev.type === 'open') return { text: '开户', cls: '' };
        return { text: '查账', cls: '' };
    }

    function renderTradeFeed() {
        const slice = recentFeed.slice(0, 4);
        feedMetaTextEl.textContent = recentFeed.length > 0
            ? '累计 ' + recentFeed.length + ' 条交互'
            : '等待弹幕下单…';

        if (slice.length === 0) {
            const emptyLi = document.createElement('li');
            emptyLi.className = 'feed-empty';
            emptyLi.textContent = '发送“开户”或“买入 NVDA 10”开启美股模拟交易';
            tradeFeedListEl.replaceChildren(emptyLi);
            return;
        }

        const frag = document.createDocumentFragment();
        for (let i = 0; i < slice.length; i++) {
            const ev = slice[i];
            const li = document.createElement('li');
            li.className = ev.ok ? 'feed-row' : 'feed-row is-fail';

            const av = Dambo.utils.createAvatar(ev.face || '', 'feed-avatar');
            av.alt = ev.name || '观众';
            li.appendChild(av);

            const tagMeta = feedTagInfo(ev);
            const tag = document.createElement('span');
            tag.className = tagMeta.cls ? ('feed-tag ' + tagMeta.cls) : 'feed-tag';
            tag.textContent = tagMeta.text;
            li.appendChild(tag);

            const txt = document.createElement('span');
            txt.className = 'feed-text';
            txt.textContent = ev.summary || '';
            li.appendChild(txt);

            frag.appendChild(li);
        }
        tradeFeedListEl.replaceChildren(frag);
    }

    function renderLeaderboard() {
        const limit = Math.round(clampNum(cfg.top_n, 3, 20, 6));
        const ranked = computeLeaderboard();
        rankMetaTextEl.textContent = '前 ' + Math.min(limit, ranked.length) + ' / ' + ranked.length + ' 户';

        if (ranked.length === 0) {
            const emptyLi = document.createElement('li');
            emptyLi.className = 'feed-empty';
            emptyLi.textContent = '暂无已开户交易员，发送弹幕“开户”即可领取 ' + formatPoints(cfg.initial_balance) + ' 模拟积分';
            leaderboardListEl.replaceChildren(emptyLi);
            return;
        }

        const slice = ranked.slice(0, limit);
        const frag = document.createDocumentFragment();
        for (let i = 0; i < slice.length; i++) {
            const row = slice[i];
            const li = document.createElement('li');
            li.className = 'lb-row' + (row.rank <= 3 ? (' rank-' + row.rank) : '');

            const rankEl = document.createElement('span');
            rankEl.className = 'lb-rank';
            rankEl.textContent = row.rank === 1 ? '🥇' : (row.rank === 2 ? '🥈' : (row.rank === 3 ? '🥉' : ('#' + row.rank)));
            li.appendChild(rankEl);

            const av = Dambo.utils.createAvatar(row.face || '', 'lb-avatar');
            av.alt = row.name;
            li.appendChild(av);

            const info = document.createElement('div');
            info.className = 'lb-info';
            const nameEl = document.createElement('div');
            nameEl.className = 'lb-name';
            nameEl.textContent = row.name;
            info.appendChild(nameEl);

            const posEl = document.createElement('div');
            posEl.className = 'lb-pos';
            if (row.holdings.length > 0) {
                posEl.textContent = '持仓 ' + row.holdings.slice(0, 3).map(function (h) { return h.ticker + '×' + h.shares; }).join(' ') + (row.holdings.length > 3 ? '…' : '');
            } else {
                posEl.textContent = '可用 ' + formatPoints(row.cash);
            }
            info.appendChild(posEl);
            li.appendChild(info);

            const right = document.createElement('div');
            right.className = 'lb-right';
            const eqEl = document.createElement('span');
            eqEl.className = 'lb-equity';
            eqEl.textContent = formatPoints(row.totalEquity);
            right.appendChild(eqEl);

            const retEl = document.createElement('span');
            const dir = row.pnlPct > 0.001 ? 'up' : (row.pnlPct < -0.001 ? 'down' : 'flat');
            retEl.className = 'lb-ret ' + dir;
            retEl.textContent = formatPct(row.pnlPct);
            right.appendChild(retEl);

            li.appendChild(right);
            frag.appendChild(li);
        }
        leaderboardListEl.replaceChildren(frag);
    }

    function renderAllUI() {
        renderQuotes();
        renderTradeFeed();
        renderLeaderboard();
    }

    // =========================================================================
    // 9. 配置加载、持久化监听与弹幕流接入
    // =========================================================================
    async function loadConfigAndApply() {
        const prevKey = getApiKey();
        const prevPoll = clampNum(cfg.poll_interval_sec, 2, 120, 5);
        try {
            const raw = await Dambo.config.get();
            cfg = Object.assign({}, FALLBACK_CONFIG, raw || {});
        } catch (_) {
            cfg = Object.assign({}, FALLBACK_CONFIG);
        }

        const nextKey = getApiKey();
        const nextPoll = clampNum(cfg.poll_interval_sec, 2, 120, 5);
        if (nextKey !== prevKey) {
            lastApiError = '';
            lastApiState = nextKey ? 'live' : 'no_key';
            rateLimitUntilMs = 0;
            invalidTickerUntil.clear();
        }
        updateStatusUI();
        renderAllUI();
        if (pollTimer === null || nextPoll !== prevPoll || nextKey !== prevKey) {
            restartBackgroundPoller();
        }
    }

    async function hydrateInitialData() {
        const results = await Promise.all([
            Dambo.storage.list('accounts').catch(function () { return {}; }),
            Dambo.memory.list('quotes').catch(function () { return {}; }),
            Dambo.memory.get('runtime', 'recent_feed').catch(function () { return null; }),
        ]);

        const acctObj = results[0] || {};
        accountsMap.clear();
        Object.keys(acctObj).forEach(function (k) {
            accountsMap.set(k, acctObj[k]);
        });

        const quoteObj = results[1] || {};
        Object.keys(quoteObj).forEach(function (sym) {
            if (quoteObj[sym] && Number(quoteObj[sym].c) > 0) {
                quotesMap.set(sym, quoteObj[sym]);
            }
        });

        if (Array.isArray(results[2])) {
            recentFeed = results[2].slice(0, MAX_FEED_ITEMS);
        }

        renderAllUI();
    }

    Dambo.storage.watch('accounts', function (ev) {
        if (!ev) return;
        if (ev.op === 'clear') {
            accountsMap.clear();
            renderAllUI();
            scheduleRuntimeSync();
            return;
        }
        if (ev.op === 'delete' && Array.isArray(ev.keys)) {
            ev.keys.forEach(function (k) { accountsMap.delete(k); });
            renderAllUI();
            scheduleRuntimeSync();
            return;
        }
        if (ev.values && typeof ev.values === 'object') {
            Object.keys(ev.values).forEach(function (k) { accountsMap.set(k, ev.values[k]); });
            renderAllUI();
            scheduleRuntimeSync();
            return;
        }
        Dambo.storage.list('accounts').then(function (all) {
            accountsMap.clear();
            Object.keys(all || {}).forEach(function (k) { accountsMap.set(k, all[k]); });
            renderAllUI();
            scheduleRuntimeSync();
        }).catch(function () {});
    });

    Dambo.config.watch(function () {
        loadConfigAndApply();
    });

    const client = Dambo.connect({
        channel: 'feed',
        url: Dambo.utils.getQueryParam('ws') || undefined,
        replaySnapshot: false,
    });

    function updateWsBadge(state) {
        wsBadgeEl.className = 'ws-pill ' + state;
        if (state === 'open') wsBadgeEl.textContent = '弹幕已连接';
        else if (state === 'reconnecting') wsBadgeEl.textContent = '重连中';
        else if (state === 'closed') wsBadgeEl.textContent = '已断开';
        else wsBadgeEl.textContent = '连接中';
    }

    client.on('state', updateWsBadge);
    client.on('room_state', function (rs) {
        if (rs && rs.connected) updateWsBadge('open');
    });

    client.on('chat', function (item, meta) {
        if (meta && meta.isSnapshot) return;
        if (!item || typeof item.content !== 'string') return;
        const cmd = parseDanmakuCommand(item.content);
        if (!cmd) return;
        handleTradeCommand(
            {
                uid: item.uid || 0,
                uname: item.uname || '观众',
                face: item.face || '',
            },
            cmd
        );
    });

    Promise.all([hydrateInitialData(), loadConfigAndApply()]).then(function () {
        scheduleRuntimeSync();
    });
})();
