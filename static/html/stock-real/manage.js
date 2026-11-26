(function () {
    'use strict';

    const MAX_KV_KEY_BYTES = 128;
    const FINNHUB_REGISTER_URL = 'https://finnhub.io/register';

    const urlParams = new URLSearchParams(window.location.search || '');
    const isEmbedded = (function () {
        if (urlParams.get('embed') === '1') return true;
        try {
            return window.self !== window.top;
        } catch (_) {
            return true;
        }
    })();
    if (isEmbedded) {
        document.body.classList.add('is-embedded');
    }

    // =========================================================================
    // 1. DOM 引用
    // =========================================================================
    const pluginTitleEl = document.getElementById('plugin-title');
    const pluginDescEl = document.getElementById('plugin-desc');
    const pluginBadgesEl = document.getElementById('plugin-badges');
    const obsUrlCodeEl = document.getElementById('obs-url-code');
    const pluginTechMetaEl = document.getElementById('plugin-tech-meta');
    const copyViewBtnEl = document.getElementById('copy-view-btn');
    const copyObsNoSafeBtnEl = document.getElementById('copy-obs-nosafe-btn');
    const reloadAllBtnEl = document.getElementById('reload-all-btn');

    const mainTabBtns = Array.from(document.querySelectorAll('[data-main-tab]'));
    const tabApiBadgeEl = document.getElementById('tab-api-badge');
    const tabCountAccountsEl = document.getElementById('tab-count-accounts');
    const tabCountQuotesEl = document.getElementById('tab-count-quotes');

    const secSettingsEl = document.getElementById('sec-settings');
    const secAccountsEl = document.getElementById('sec-accounts');
    const secFeedEl = document.getElementById('sec-feed');

    const runtimeStateBadgeEl = document.getElementById('runtime-state-badge');
    const apiKeyInputEl = document.getElementById('api-key-input');
    const toggleKeyVisBtnEl = document.getElementById('toggle-key-vis-btn');
    const saveKeyBtnEl = document.getElementById('save-key-btn');
    const testConnBtnEl = document.getElementById('test-conn-btn');
    const connTestResultEl = document.getElementById('conn-test-result');
    const copyFinnhubUrlBtnEl = document.getElementById('copy-finnhub-url-btn');

    const cfgInitialBalanceEl = document.getElementById('cfg-initial-balance');
    const cfgPollSecEl = document.getElementById('cfg-poll-sec');
    const cfgWatchlistEl = document.getElementById('cfg-watchlist');
    const cfgTopNEl = document.getElementById('cfg-top-n');
    const cfgCooldownMsEl = document.getElementById('cfg-cooldown-ms');
    const saveParamsBtnEl = document.getElementById('save-params-btn');
    const resetParamsBtnEl = document.getElementById('reset-params-btn');

    const accountsSummaryTextEl = document.getElementById('accounts-summary-text');
    const resetSeasonBtnEl = document.getElementById('reset-season-btn');
    const adjustBalanceFormEl = document.getElementById('adjust-balance-form');
    const adjUidEl = document.getElementById('adj-uid');
    const adjNameEl = document.getElementById('adj-name');
    const adjOpEl = document.getElementById('adj-op');
    const adjValEl = document.getElementById('adj-val');
    const accountsListEl = document.getElementById('accounts-list');

    const manageFeedListEl = document.getElementById('manage-feed-list');
    const manageQuotesGridEl = document.getElementById('manage-quotes-grid');

    const authModalEl = document.getElementById('auth-modal');
    const authFormEl = document.getElementById('auth-form');
    const authTokenInputEl = document.getElementById('auth-token-input');
    const authErrorEl = document.getElementById('auth-error-text');
    const toastEl = document.getElementById('toast');

    // =========================================================================
    // 2. 运行时状态
    // =========================================================================
    let currentMeta = null;
    let currentCfg = {};
    let activeMainTab = 'settings';
    let toastTimer = null;
    let seasonResetArmed = false;
    let seasonResetTimer = null;

    const accountsMap = new Map();
    const quotesMap = new Map();
    let runtimeStatus = null;
    let recentFeed = [];

    let offCfgWatch = null;
    let offAcctWatch = null;
    let offQuotesWatch = null;
    let offRuntimeWatch = null;
    let offTopicSub = null;

    // =========================================================================
    // 3. 基础工具函数
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

    function formatPoints(val) {
        const num = Number(val || 0);
        return num.toLocaleString('en-US', {
            minimumFractionDigits: 0,
            maximumFractionDigits: 2,
        });
    }

    function formatPct(pct) {
        const n = Number(pct || 0);
        return (n > 0 ? '+' : '') + n.toFixed(2) + '%';
    }

    function showToast(msg, isError) {
        if (toastTimer) clearTimeout(toastTimer);
        toastEl.textContent = String(msg || '');
        toastEl.className = isError ? 'toast visible error' : 'toast visible';
        toastTimer = setTimeout(function () {
            toastEl.className = 'toast';
        }, 3200);
    }

    function handleApiError(err, fallbackMsg) {
        if (err && err.code === 'UNAUTHORIZED') {
            authErrorEl.classList.add('hidden');
            authModalEl.classList.add('open');
            authTokenInputEl.focus();
            return;
        }
        const text = err && err.message ? err.message : (fallbackMsg || '操作失败');
        showToast(text, true);
    }

    function makeBadge(text, cls) {
        const span = document.createElement('span');
        span.className = cls ? ('badge ' + cls) : 'badge';
        span.textContent = text;
        return span;
    }

    function makeBtn(label, cls, onClick) {
        const btn = document.createElement('button');
        btn.type = 'button';
        if (cls) btn.className = cls;
        btn.textContent = label;
        btn.addEventListener('click', onClick);
        return btn;
    }

    function resolveCanonicalObsUrl(meta) {
        const rel = meta && meta.route ? meta.route : ('/' + Dambo.plugin.name + '/');
        return new URL(rel, window.location.origin).toString();
    }

    function switchMainTab(tabName) {
        const valid = ['settings', 'accounts', 'feed'];
        activeMainTab = valid.indexOf(tabName) !== -1 ? tabName : 'settings';
        mainTabBtns.forEach(function (btn) {
            btn.classList.toggle('active', btn.getAttribute('data-main-tab') === activeMainTab);
        });
        secSettingsEl.classList.toggle('hidden', activeMainTab !== 'settings');
        secAccountsEl.classList.toggle('hidden', activeMainTab !== 'accounts');
        secFeedEl.classList.toggle('hidden', activeMainTab !== 'feed');
    }

    function calculateAccountMetrics(acct) {
        const cash = Number(acct.cash || 0);
        let holdingsVal = 0;
        const holdingItems = [];
        const pos = acct.positions && typeof acct.positions === 'object' ? acct.positions : {};
        Object.keys(pos).forEach(function (sym) {
            const p = pos[sym];
            if (!p || Number(p.shares) <= 0) return;
            const q = quotesMap.get(sym);
            const markPrice = q && Number(q.c) > 0 ? Number(q.c) : Number(p.cost || 0);
            holdingsVal += Number(p.shares) * markPrice;
            holdingItems.push({
                ticker: sym,
                shares: Number(p.shares),
                cost: Number(p.cost || 0),
                price: markPrice,
                hasLiveQuote: Boolean(q && Number(q.c) > 0),
            });
        });

        const totalEquity = roundMoney(cash + holdingsVal);
        const initBal = Number(acct.initial_balance || currentCfg.initial_balance || 100000);
        const pnl = roundMoney(totalEquity - initBal);
        const pnlPct = initBal > 0 ? (pnl / initBal) * 100 : 0;
        return {
            cash: roundMoney(cash),
            holdingsVal: roundMoney(holdingsVal),
            totalEquity: totalEquity,
            initBal: initBal,
            pnl: pnl,
            pnlPct: pnlPct,
            holdings: holdingItems,
        };
    }

    // =========================================================================
    // 4. 界面渲染（全部采用安全 DOM API 构造）
    // =========================================================================
    function renderMetaHeader(meta) {
        if (!meta) return;
        currentMeta = meta;
        const displayTitle = meta.name || meta.plugin_name;
        pluginTitleEl.textContent = displayTitle;
        document.title = displayTitle + ' · 控制面板';
        pluginDescEl.textContent = meta.description || pluginDescEl.textContent;
        obsUrlCodeEl.textContent = resolveCanonicalObsUrl(meta);

        const hasKey = Boolean(String(currentCfg.finnhub_api_key || '').trim());
        pluginBadgesEl.replaceChildren();
        pluginBadgesEl.appendChild(makeBadge('v' + meta.version, 'accent'));
        pluginBadgesEl.appendChild(makeBadge(hasKey ? 'Key 已配置' : '待配置 Key', hasKey ? 'ok' : 'warn'));
        if (meta.obs && (meta.obs.width || meta.obs.height)) {
            pluginBadgesEl.appendChild(makeBadge('OBS ' + (meta.obs.width || '?') + '×' + (meta.obs.height || '?')));
        }
        pluginTechMetaEl.textContent = '目录: ' + meta.plugin_name + ' · 数据域: ' + meta.namedomain + ' · 初始模拟积分: ' + formatPoints(currentCfg.initial_balance || 100000) + ' · 轮询: ' + (currentCfg.poll_interval_sec || 5) + 's';
    }

    function renderSettingsInputs() {
        const keyStr = String(currentCfg.finnhub_api_key || '').trim();
        if (document.activeElement !== apiKeyInputEl) {
            apiKeyInputEl.value = keyStr;
        }
        tabApiBadgeEl.textContent = keyStr ? '已配置' : '未配置';
        tabApiBadgeEl.className = keyStr ? 'badge ok' : 'badge warn';

        if (document.activeElement !== cfgInitialBalanceEl) {
            cfgInitialBalanceEl.value = String(currentCfg.initial_balance !== undefined ? currentCfg.initial_balance : 100000);
        }
        if (document.activeElement !== cfgPollSecEl) {
            cfgPollSecEl.value = String(currentCfg.poll_interval_sec !== undefined ? currentCfg.poll_interval_sec : 5);
        }
        if (document.activeElement !== cfgWatchlistEl) {
            cfgWatchlistEl.value = String(currentCfg.watchlist_default !== undefined ? currentCfg.watchlist_default : 'NVDA,AAPL,TSLA,MSFT,SPY');
        }
        if (document.activeElement !== cfgTopNEl) {
            cfgTopNEl.value = String(currentCfg.top_n !== undefined ? currentCfg.top_n : 6);
        }
        if (document.activeElement !== cfgCooldownMsEl) {
            cfgCooldownMsEl.value = String(currentCfg.trade_cooldown_ms !== undefined ? currentCfg.trade_cooldown_ms : 1500);
        }
    }

    function renderRuntimeStatusBadge() {
        if (!runtimeStatus || typeof runtimeStatus !== 'object') {
            runtimeStateBadgeEl.className = 'badge';
            runtimeStateBadgeEl.textContent = '待机（打开 OBS 页后自动轮询）';
            return;
        }
        const st = runtimeStatus.state;
        const rpm = Number(runtimeStatus.requests_last_min || 0);
        if (st === 'live') {
            runtimeStateBadgeEl.className = 'badge ok';
            runtimeStateBadgeEl.textContent = '展示端实时运行中 · 近1分 ' + rpm + '/54 次请求';
        } else if (st === 'rate_limited') {
            runtimeStateBadgeEl.className = 'badge danger';
            runtimeStateBadgeEl.textContent = '展示端触发 429 限流冷却中';
        } else if (st === 'invalid_key') {
            runtimeStateBadgeEl.className = 'badge danger';
            runtimeStateBadgeEl.textContent = 'Finnhub API Key 被拒绝';
        } else if (st === 'no_key') {
            runtimeStateBadgeEl.className = 'badge warn';
            runtimeStateBadgeEl.textContent = '等待配置 Finnhub API Key';
        } else {
            runtimeStateBadgeEl.className = 'badge warn';
            runtimeStateBadgeEl.textContent = runtimeStatus.error || '行情请求异常';
        }
    }

    function renderAccountsList() {
        const list = [];
        accountsMap.forEach(function (acct, key) {
            if (!acct || typeof acct !== 'object') return;
            list.push({
                key: key,
                acct: acct,
                metrics: calculateAccountMetrics(acct),
            });
        });

        list.sort(function (a, b) {
            if (b.metrics.totalEquity !== a.metrics.totalEquity) {
                return b.metrics.totalEquity - a.metrics.totalEquity;
            }
            return b.metrics.pnlPct - a.metrics.pnlPct;
        });

        tabCountAccountsEl.textContent = String(list.length);
        let sumEquity = 0;
        list.forEach(function (item) { sumEquity += item.metrics.totalEquity; });
        accountsSummaryTextEl.textContent = '共 ' + list.length + ' 位已开户交易员 · 全场总权益 ' + formatPoints(roundMoney(sumEquity)) + ' 模拟积分';

        if (list.length === 0) {
            const emptyDiv = document.createElement('div');
            emptyDiv.className = 'muted';
            emptyDiv.textContent = '暂无已开户观众。观众在直播间发送“开户”或管理员在上方手动调分均可创建账户。';
            accountsListEl.replaceChildren(emptyDiv);
            return;
        }

        const frag = document.createDocumentFragment();
        list.forEach(function (item, idx) {
            const acct = item.acct;
            const m = item.metrics;

            const card = document.createElement('div');
            card.className = 'acct-card';

            const top = document.createElement('div');
            top.className = 'item-top';

            const left = document.createElement('div');
            left.className = 'row';
            left.appendChild(makeBadge('#' + (idx + 1), idx === 0 ? 'warn' : ''));
            const av = Dambo.utils.createAvatar(acct.face || '', 'acct-avatar');
            av.alt = acct.name || '观众';
            left.appendChild(av);

            const nameEl = document.createElement('strong');
            nameEl.textContent = acct.name || '观众';
            left.appendChild(nameEl);
            if (acct.uid > 0) {
                left.appendChild(makeBadge('UID ' + acct.uid));
            }
            left.appendChild(makeBadge('总权益 ' + formatPoints(m.totalEquity) + ' 积分', 'accent'));
            left.appendChild(makeBadge('可用 ' + formatPoints(m.cash)));
            const retCls = m.pnl > 0 ? 'ok' : (m.pnl < 0 ? 'danger' : '');
            left.appendChild(makeBadge((m.pnl >= 0 ? '盈 +' : '亏 ') + formatPoints(m.pnl) + ' (' + formatPct(m.pnlPct) + ')', retCls));
            top.appendChild(left);

            const right = document.createElement('div');
            right.className = 'row';
            right.appendChild(makeBtn('调分', 'small', function () {
                adjUidEl.value = acct.uid > 0 ? String(acct.uid) : '';
                adjNameEl.value = acct.name || '';
                adjOpEl.value = 'set';
                adjValEl.value = String(m.cash);
                adjValEl.focus();
                showToast('已将 ' + (acct.name || '观众') + ' 填入上方调分表单');
            }));
            right.appendChild(makeBtn('清除账户', 'danger small', async function () {
                try {
                    await Dambo.storage.delete('accounts', item.key);
                    accountsMap.delete(item.key);
                    renderAccountsList();
                    showToast('已删除交易账户 ' + (acct.name || item.key));
                } catch (err) {
                    handleApiError(err, '删除账户失败');
                }
            }));
            top.appendChild(right);
            card.appendChild(top);

            const posRow = document.createElement('div');
            posRow.className = 'pos-list';
            if (m.holdings.length === 0) {
                const emptyPos = document.createElement('span');
                emptyPos.className = 'muted';
                emptyPos.textContent = '当前空仓 · 累计成交 ' + Number(acct.trade_count || 0) + ' 笔';
                posRow.appendChild(emptyPos);
            } else {
                m.holdings.forEach(function (h) {
                    const span = document.createElement('span');
                    span.className = 'pos-badge';
                    const qText = h.hasLiveQuote ? ('现价 $' + h.price.toFixed(2)) : '暂无现价';
                    span.textContent = h.ticker + ' ×' + h.shares + ' · 成本 $' + h.cost.toFixed(2) + ' · ' + qText;
                    posRow.appendChild(span);
                });
            }
            card.appendChild(posRow);
            frag.appendChild(card);
        });

        accountsListEl.replaceChildren(frag);
    }

    function renderFeedAndQuotes() {
        // 1. 交易播报流
        if (!recentFeed || recentFeed.length === 0) {
            const empty = document.createElement('div');
            empty.className = 'muted';
            empty.textContent = '暂无交易播报记录';
            manageFeedListEl.replaceChildren(empty);
        } else {
            const fFrag = document.createDocumentFragment();
            recentFeed.slice(0, 20).forEach(function (ev) {
                const box = document.createElement('div');
                box.className = 'item-box';
                const top = document.createElement('div');
                top.className = 'item-top';

                const left = document.createElement('div');
                left.className = 'row';
                left.appendChild(makeBadge(ev.type || 'event', ev.ok ? 'accent' : 'warn'));
                const txt = document.createElement('span');
                txt.textContent = ev.summary || '';
                left.appendChild(txt);
                top.appendChild(left);

                const timeEl = document.createElement('span');
                timeEl.className = 'muted';
                timeEl.textContent = ev.ts ? new Date(ev.ts).toLocaleTimeString() : '';
                top.appendChild(timeEl);

                box.appendChild(top);
                fFrag.appendChild(box);
            });
            manageFeedListEl.replaceChildren(fFrag);
        }

        // 2. 缓存行情快照
        const syms = Array.from(quotesMap.keys()).sort();
        tabCountQuotesEl.textContent = String(syms.length);
        if (syms.length === 0) {
            const emptyQ = document.createElement('div');
            emptyQ.className = 'muted';
            emptyQ.textContent = '尚无缓存的美股报价（需配置 Key 并打开 OBS 展示页）';
            manageQuotesGridEl.replaceChildren(emptyQ);
            return;
        }

        const qFrag = document.createDocumentFragment();
        syms.forEach(function (sym) {
            const q = quotesMap.get(sym);
            const card = document.createElement('div');
            card.className = 'quote-box';

            const head = document.createElement('div');
            head.className = 'between';
            const sEl = document.createElement('strong');
            sEl.textContent = sym;
            const dp = Number(q.dp || 0);
            const badgeCls = dp > 0 ? 'ok' : (dp < 0 ? 'danger' : '');
            head.appendChild(sEl);
            head.appendChild(makeBadge((dp > 0 ? '+' : '') + dp.toFixed(2) + '%', badgeCls));
            card.appendChild(head);

            const priceEl = document.createElement('div');
            priceEl.style.fontSize = '16px';
            priceEl.style.fontWeight = '700';
            priceEl.textContent = '$' + Number(q.c || 0).toFixed(2);
            card.appendChild(priceEl);

            const timeEl = document.createElement('div');
            timeEl.className = 'muted';
            timeEl.textContent = '昨收 $' + Number(q.pc || 0).toFixed(2) + (q.fetchedAt ? (' · ' + new Date(q.fetchedAt).toLocaleTimeString()) : '');
            card.appendChild(timeEl);

            qFrag.appendChild(card);
        });
        manageQuotesGridEl.replaceChildren(qFrag);
    }

    // =========================================================================
    // 5. 数据加载与实时订阅
    // =========================================================================
    async function reloadConfigAndMeta() {
        const meta = await Dambo.plugin.meta({ force: true });
        const cfgSchema = (meta && meta.config) || {};
        const flat = {};
        Object.keys(cfgSchema).forEach(function (k) {
            flat[k] = cfgSchema[k].value;
        });
        currentCfg = flat;
        renderMetaHeader(meta);
        renderSettingsInputs();
        renderAccountsList();
    }

    async function loadInitialStores() {
        const results = await Promise.all([
            Dambo.storage.list('accounts').catch(function () { return {}; }),
            Dambo.memory.list('quotes').catch(function () { return {}; }),
            Dambo.memory.get('runtime', 'status').catch(function () { return null; }),
            Dambo.memory.get('runtime', 'recent_feed').catch(function () { return null; }),
        ]);

        accountsMap.clear();
        Object.keys(results[0] || {}).forEach(function (k) {
            accountsMap.set(k, results[0][k]);
        });

        quotesMap.clear();
        Object.keys(results[1] || {}).forEach(function (k) {
            if (results[1][k] && Number(results[1][k].c) > 0) {
                quotesMap.set(k, results[1][k]);
            }
        });

        runtimeStatus = results[2];
        if (Array.isArray(results[3])) {
            recentFeed = results[3].slice(0, 30);
        }

        renderRuntimeStatusBadge();
        renderAccountsList();
        renderFeedAndQuotes();
    }

    function ensureWatchesBound() {
        if (!offCfgWatch) {
            offCfgWatch = Dambo.config.watch(function () {
                reloadConfigAndMeta().catch(function (err) {
                    handleApiError(err, '刷新配置失败');
                });
            });
        }

        if (!offAcctWatch) {
            offAcctWatch = Dambo.storage.watch('accounts', function (ev) {
                if (!ev) return;
                if (ev.op === 'clear') {
                    accountsMap.clear();
                    renderAccountsList();
                    return;
                }
                if (ev.op === 'delete' && Array.isArray(ev.keys)) {
                    ev.keys.forEach(function (k) { accountsMap.delete(k); });
                    renderAccountsList();
                    return;
                }
                if (ev.values && typeof ev.values === 'object') {
                    Object.keys(ev.values).forEach(function (k) { accountsMap.set(k, ev.values[k]); });
                    renderAccountsList();
                    return;
                }
                Dambo.storage.list('accounts').then(function (all) {
                    accountsMap.clear();
                    Object.keys(all || {}).forEach(function (k) { accountsMap.set(k, all[k]); });
                    renderAccountsList();
                }).catch(function () {});
            });
        }

        if (!offQuotesWatch) {
            offQuotesWatch = Dambo.memory.watch('quotes', function (ev) {
                if (!ev) return;
                if (ev.op === 'clear') {
                    quotesMap.clear();
                    renderAccountsList();
                    renderFeedAndQuotes();
                    return;
                }
                if (ev.values && typeof ev.values === 'object') {
                    Object.keys(ev.values).forEach(function (k) {
                        if (ev.values[k] && Number(ev.values[k].c) > 0) quotesMap.set(k, ev.values[k]);
                    });
                    renderAccountsList();
                    renderFeedAndQuotes();
                }
            });
        }

        if (!offRuntimeWatch) {
            offRuntimeWatch = Dambo.memory.watch('runtime', function (ev) {
                if (!ev || !ev.values) return;
                if (Object.prototype.hasOwnProperty.call(ev.values, 'status')) {
                    runtimeStatus = ev.values.status;
                    renderRuntimeStatusBadge();
                }
                if (Object.prototype.hasOwnProperty.call(ev.values, 'recent_feed') && Array.isArray(ev.values.recent_feed)) {
                    recentFeed = ev.values.recent_feed;
                    renderFeedAndQuotes();
                }
            });
        }

        if (!offTopicSub) {
            offTopicSub = Dambo.topics.subscribe('@self', 'trade_feed', function (msg) {
                if (!msg || !msg.payload) return;
                const ev = msg.payload;
                recentFeed.unshift(ev);
                if (recentFeed.length > 30) recentFeed = recentFeed.slice(0, 30);
                renderFeedAndQuotes();
            });
        }
    }

    // =========================================================================
    // 6. 交互事件处理
    // =========================================================================
    mainTabBtns.forEach(function (btn) {
        btn.addEventListener('click', function () {
            switchMainTab(btn.getAttribute('data-main-tab'));
        });
    });

    copyViewBtnEl.addEventListener('click', async function () {
        const fullUrl = resolveCanonicalObsUrl(currentMeta);
        try {
            await navigator.clipboard.writeText(fullUrl);
            showToast('已复制 OBS 链接');
        } catch (_) {
            showToast('OBS 链接: ' + fullUrl);
        }
    });

    copyObsNoSafeBtnEl.addEventListener('click', async function () {
        const fullUrl = resolveCanonicalObsUrl(currentMeta) + '?safe_top=0';
        try {
            await navigator.clipboard.writeText(fullUrl);
            showToast('已复制纯净 OBS 链接 (?safe_top=0)');
        } catch (_) {
            showToast('OBS 链接: ' + fullUrl);
        }
    });

    reloadAllBtnEl.addEventListener('click', function () {
        initManagePage();
    });

    toggleKeyVisBtnEl.addEventListener('click', function () {
        const isPw = apiKeyInputEl.type === 'password';
        apiKeyInputEl.type = isPw ? 'text' : 'password';
        toggleKeyVisBtnEl.textContent = isPw ? '隐藏' : '显示';
    });

    saveKeyBtnEl.addEventListener('click', async function () {
        const nextKey = apiKeyInputEl.value.trim();
        try {
            await Dambo.config.set('finnhub_api_key', nextKey);
            showToast(nextKey ? 'Finnhub API Key 已保存并生效' : '已清空 Finnhub API Key');
            await reloadConfigAndMeta();
        } catch (err) {
            handleApiError(err, '保存 API Key 失败');
        }
    });

    testConnBtnEl.addEventListener('click', async function () {
        const keyToTest = apiKeyInputEl.value.trim() || String(currentCfg.finnhub_api_key || '').trim();
        if (!keyToTest) {
            connTestResultEl.className = 'test-result error';
            connTestResultEl.textContent = '请先输入 Finnhub API Key 后再测试连通性';
            return;
        }
        testConnBtnEl.disabled = true;
        connTestResultEl.className = 'test-result';
        connTestResultEl.textContent = '正在请求 Finnhub 接口检测 AAPL 实时报价…';

        const startMs = typeof performance !== 'undefined' ? performance.now() : Date.now();
        try {
            const url = 'https://finnhub.io/api/v1/quote?symbol=AAPL&token=' + encodeURIComponent(keyToTest);
            const res = await fetch(url, { method: 'GET' });
            const latencyMs = Math.round((typeof performance !== 'undefined' ? performance.now() : Date.now()) - startMs);

            if (res.status === 429) {
                connTestResultEl.className = 'test-result error';
                connTestResultEl.textContent = 'HTTP 429 限流（' + latencyMs + ' ms）：已超出 60 次/分钟免费额度，请等待约 30 秒重试';
                return;
            }
            if (res.status === 401 || res.status === 403) {
                connTestResultEl.className = 'test-result error';
                connTestResultEl.textContent = 'HTTP ' + res.status + ' 鉴权失败（' + latencyMs + ' ms）：API Key 无效或尚未激活';
                return;
            }
            if (!res.ok) {
                connTestResultEl.className = 'test-result error';
                connTestResultEl.textContent = 'Finnhub 返回异常状态 HTTP ' + res.status + '（' + latencyMs + ' ms）';
                return;
            }

            const data = await res.json();
            const c = Number(data && data.c);
            const pc = Number(data && data.pc);
            const dp = Number.isFinite(Number(data && data.dp))
                ? Number(data.dp)
                : (pc > 0 ? ((c - pc) / pc) * 100 : 0);

            if (!Number.isFinite(c) || c <= 0) {
                connTestResultEl.className = 'test-result error';
                connTestResultEl.textContent = '接口连通（' + latencyMs + ' ms）但未返回有效 AAPL 价格，请检查 Key 权限';
                return;
            }

            connTestResultEl.className = 'test-result ok';
            connTestResultEl.textContent = '连通成功！AAPL 现价 $' + c.toFixed(2) + ' · 涨跌 ' + formatPct(dp) + ' · 延迟 ' + latencyMs + ' ms';
        } catch (err) {
            connTestResultEl.className = 'test-result error';
            connTestResultEl.textContent = '网络请求失败：' + (err && err.message ? err.message : '无法访问 finnhub.io');
        } finally {
            testConnBtnEl.disabled = false;
        }
    });

    copyFinnhubUrlBtnEl.addEventListener('click', async function () {
        try {
            await navigator.clipboard.writeText(FINNHUB_REGISTER_URL);
            showToast('已复制注册地址：' + FINNHUB_REGISTER_URL);
        } catch (_) {
            showToast('请手动复制：' + FINNHUB_REGISTER_URL);
        }
    });

    saveParamsBtnEl.addEventListener('click', async function () {
        const initBal = Number(cfgInitialBalanceEl.value);
        const pollSec = Number(cfgPollSecEl.value);
        const topN = Number(cfgTopNEl.value);
        const cooldownMs = Number(cfgCooldownMsEl.value);
        const watchlist = cfgWatchlistEl.value.trim();
        try {
            await Dambo.config.set({
                initial_balance: Math.round(initBal),
                poll_interval_sec: Math.round(pollSec),
                watchlist_default: watchlist,
                top_n: Math.round(topN),
                trade_cooldown_ms: Math.round(cooldownMs),
            });
            showToast('赛季与引擎参数已保存');
            await reloadConfigAndMeta();
        } catch (err) {
            handleApiError(err, '保存参数失败');
        }
    });

    resetParamsBtnEl.addEventListener('click', async function () {
        try {
            await Dambo.config.reset();
            showToast('已恢复全部默认参数（含清空 Key）');
            await reloadConfigAndMeta();
        } catch (err) {
            handleApiError(err, '恢复默认失败');
        }
    });

    adjustBalanceFormEl.addEventListener('submit', async function (ev) {
        ev.preventDefault();
        const name = adjNameEl.value.trim();
        const amount = Number(adjValEl.value);
        if (!name || !Number.isFinite(amount)) {
            showToast('请填写有效观众昵称与模拟积分数额', true);
            return;
        }

        const uidNum = adjUidEl.value.trim() ? Math.max(0, Math.floor(Number(adjUidEl.value))) : 0;
        const acctKey = uidNum > 0 ? ('uid:' + uidNum) : sanitizeKvKey('name:' + name, 'name');
        const op = adjOpEl.value;
        const defaultBal = Number(currentCfg.initial_balance || 100000);

        let acct = accountsMap.get(acctKey);
        if (!acct || typeof acct !== 'object') {
            acct = {
                uid: uidNum,
                name: name,
                face: '',
                cash: op === 'set' ? Math.max(0, roundMoney(amount)) : Math.max(0, roundMoney(defaultBal + amount)),
                initial_balance: defaultBal,
                positions: {},
                realized_pnl: 0,
                trade_count: 0,
                created_at: Date.now(),
                updated_at: Date.now(),
            };
        } else {
            acct = JSON.parse(JSON.stringify(acct));
            acct.name = name;
            if (uidNum > 0) acct.uid = uidNum;
            acct.cash = op === 'set'
                ? Math.max(0, roundMoney(amount))
                : Math.max(0, roundMoney(Number(acct.cash || 0) + amount));
            acct.updated_at = Date.now();
        }

        try {
            await Dambo.storage.set('accounts', acctKey, acct);
            accountsMap.set(acctKey, acct);
            renderAccountsList();
            showToast('已更新 ' + acct.name + ' 可用模拟积分为 ' + formatPoints(acct.cash));
            adjNameEl.value = '';
            adjUidEl.value = '';
        } catch (err) {
            handleApiError(err, '调整积分失败');
        }
    });

    resetSeasonBtnEl.addEventListener('click', async function () {
        if (!seasonResetArmed) {
            seasonResetArmed = true;
            resetSeasonBtnEl.textContent = '再次点击确认重置新赛季';
            seasonResetTimer = setTimeout(function () {
                seasonResetArmed = false;
                resetSeasonBtnEl.textContent = '重置新赛季（清空全员账本）';
            }, 3500);
            return;
        }
        if (seasonResetTimer) clearTimeout(seasonResetTimer);
        seasonResetArmed = false;
        resetSeasonBtnEl.textContent = '重置新赛季（清空全员账本）';

        try {
            const count = await Dambo.storage.clearNamespace('accounts');
            accountsMap.clear();
            recentFeed = [];
            await Dambo.memory.mset('runtime', {
                leaderboard: [],
                recent_feed: [],
            }).catch(function () {});
            await Dambo.topics.publish('trade_feed', {
                id: 'season_reset_' + Date.now(),
                type: 'info',
                ok: true,
                name: '系统',
                summary: '主播已开启新赛季（重置 ' + count + ' 个观众账本），发送“开户”重新领取模拟积分',
                ts: Date.now(),
            }, { public: true }).catch(function () {});
            renderAccountsList();
            renderFeedAndQuotes();
            showToast('新赛季已开启，共清空 ' + count + ' 个观众账本');
        } catch (err) {
            handleApiError(err, '重置赛季失败');
        }
    });

    authFormEl.addEventListener('submit', async function (ev) {
        ev.preventDefault();
        try {
            await Dambo.plugin.unlockManage(authTokenInputEl.value);
            authModalEl.classList.remove('open');
            authTokenInputEl.value = '';
            authErrorEl.classList.add('hidden');
            showToast('远程管理解锁成功');
            await initManagePage();
        } catch (err) {
            authErrorEl.textContent = (err && err.message) ? err.message : '口令验证失败';
            authErrorEl.classList.remove('hidden');
        }
    });

    // =========================================================================
    // 7. 初始化入口与扫码 #t= 自动解锁
    // =========================================================================
    function applyInitialTabFromUrl() {
        const qTab = urlParams.get('tab');
        if (qTab) {
            switchMainTab(qTab);
            return;
        }
        const rawHash = window.location.hash || '';
        if (rawHash.indexOf('#') === 0) {
            const hParams = new URLSearchParams(rawHash.slice(1));
            const hTab = hParams.get('tab');
            if (hTab) switchMainTab(hTab);
        }
    }

    async function tryHashTokenUnlock() {
        const rawHash = window.location.hash || '';
        if (!rawHash) return;
        const trimmed = rawHash.indexOf('#') === 0 ? rawHash.slice(1) : rawHash;
        const params = new URLSearchParams(trimmed);
        const token = params.get('t');
        if (!token) return;
        params.delete('t');
        const restHash = params.toString();
        const cleanUrl = window.location.pathname + window.location.search + (restHash ? ('#' + restHash) : '');
        try {
            window.history.replaceState(null, '', cleanUrl);
        } catch (_) {}
        try {
            await Dambo.plugin.unlockManage(token);
            showToast('已通过二维码口令自动解锁');
        } catch (err) {
            authErrorEl.textContent = (err && err.message) ? err.message : '二维码口令无效，请手动输入';
            authErrorEl.classList.remove('hidden');
            authModalEl.classList.add('open');
            throw err;
        }
    }

    async function initManagePage() {
        ensureWatchesBound();
        try {
            await reloadConfigAndMeta();
            await loadInitialStores();
        } catch (err) {
            handleApiError(err, '初始化控制面板失败');
        }
    }

    (async function bootManagePage() {
        applyInitialTabFromUrl();
        try {
            await tryHashTokenUnlock();
        } catch (_) {
            return;
        }
        await initManagePage();
    })();
})();
