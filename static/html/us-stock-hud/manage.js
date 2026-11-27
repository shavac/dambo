(function () {
    'use strict';

    const Engine = window.UsStockEngine;
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

    // -------------------------------------------------------------------------
    // 1. DOM References
    // -------------------------------------------------------------------------
    const pluginTitleEl = document.getElementById('plugin-title');
    const pluginDescEl = document.getElementById('plugin-desc');
    const pluginBadgesEl = document.getElementById('plugin-badges');
    const obsUrlCodeEl = document.getElementById('obs-url-code');
    const reloadAllBtnEl = document.getElementById('reload-all-btn');
    const copyObsBtnEl = document.getElementById('copy-obs-btn');
    const copyObsTransparentBtnEl = document.getElementById('copy-obs-transparent-btn');

    const tabBtns = Array.from(document.querySelectorAll('[data-tab]'));
    const tabModeBadgeEl = document.getElementById('tab-mode-badge');
    const tabAccountsCountEl = document.getElementById('tab-accounts-count');
    const tabQuotesCountEl = document.getElementById('tab-quotes-count');
    const panels = {
        config: document.getElementById('panel-config'),
        sim: document.getElementById('panel-sim'),
        accounts: document.getElementById('panel-accounts'),
        telemetry: document.getElementById('panel-telemetry'),
    };

    const runtimeHudBadgeEl = document.getElementById('runtime-hud-badge');
    const cfgFinnhubKeyEl = document.getElementById('cfg-finnhub-key');
    const toggleKeyBtnEl = document.getElementById('toggle-key-btn');
    const saveKeyBtnEl = document.getElementById('save-key-btn');
    const testKeyBtnEl = document.getElementById('test-key-btn');
    const keyTestResultEl = document.getElementById('key-test-result');
    const saveAllCfgBtnEl = document.getElementById('save-all-cfg-btn');
    const resetAllCfgBtnEl = document.getElementById('reset-all-cfg-btn');

    // Config inputs
    const cfgEls = {
        market_data_mode: document.getElementById('cfg-market-mode'),
        market_session_override: document.getElementById('cfg-session-override'),
        poll_interval_sec: document.getElementById('cfg-poll-sec'),
        enable_finnhub_ws: document.getElementById('cfg-enable-ws'),
        allow_trading_when_closed: document.getElementById('cfg-trade-closed'),
        marquee_major_tickers: document.getElementById('cfg-marquee-major'),
        watchlist_tickers: document.getElementById('cfg-watchlist'),
        require_fan_medal: document.getElementById('cfg-require-medal'),
        guard_bypass_medal: document.getElementById('cfg-guard-bypass'),
        room_medal_name: document.getElementById('cfg-room-medal'),
        min_medal_level: document.getElementById('cfg-min-medal-lv'),
        focus_auto_rotate_sec: document.getElementById('cfg-rot-sec'),
        focus_lock_lv0_sec: document.getElementById('cfg-lock-lv0'),
        focus_lock_lv1_sec: document.getElementById('cfg-lock-lv1'),
        focus_lock_lv2_sec: document.getElementById('cfg-lock-lv2'),
        focus_lock_lv3_sec: document.getElementById('cfg-lock-lv3'),
        pos_card_duration_sec: document.getElementById('cfg-pos-dur'),
        initial_balance: document.getElementById('cfg-init-bal'),
        min_trade_usd: document.getElementById('cfg-min-trade'),
        short_leverage: document.getElementById('cfg-short-lev'),
        liquidation_maint_ratio: document.getElementById('cfg-liq-ratio'),
        commission_bps: document.getElementById('cfg-comm-bps'),
        trade_cooldown_ms: document.getElementById('cfg-cooldown'),
        whale_threshold_usd: document.getElementById('cfg-whale-usd'),
        banner_duration_sec: document.getElementById('cfg-banner-sec'),
        allow_fractional_shares: document.getElementById('cfg-frac-shares'),
        hud_title: document.getElementById('cfg-hud-title'),
        bg_mode: document.getElementById('cfg-bg-mode'),
        bg_opacity: document.getElementById('cfg-bg-opacity'),
        color_up: document.getElementById('cfg-color-up'),
        color_down: document.getElementById('cfg-color-down'),
        color_gold: document.getElementById('cfg-color-gold'),
        leaderboard_top_n: document.getElementById('cfg-lb-topn'),
        leaderboard_sort: document.getElementById('cfg-lb-sort'),
        holding_rotate_sec: document.getElementById('cfg-hold-rot'),
        marquee_speed_sec: document.getElementById('cfg-mq-speed'),
        reserve_overlay_corner: document.getElementById('cfg-safe-corner'),
    };

    // Simulator & Remote Control
    const simFormEl = document.getElementById('sim-danmaku-form');
    const simUidEl = document.getElementById('sim-uid');
    const simUnameEl = document.getElementById('sim-uname');
    const simMedalNameEl = document.getElementById('sim-medal-name');
    const simMedalLvEl = document.getElementById('sim-medal-lv');
    const simGuardLevelEl = document.getElementById('sim-guard-level');
    const simContentEl = document.getElementById('sim-content');
    const presetBtns = Array.from(document.querySelectorAll('[data-preset]'));
    const ctrlSymbolEl = document.getElementById('ctrl-symbol');
    const ctrlPinBtnEl = document.getElementById('ctrl-pin-btn');
    const ctrlPumpBtnEl = document.getElementById('ctrl-pump-btn');
    const ctrlDumpBtnEl = document.getElementById('ctrl-dump-btn');
    const ctrlClearFeedBtnEl = document.getElementById('ctrl-clear-feed-btn');

    // Accounts
    const accountsSummaryLineEl = document.getElementById('accounts-summary-line');
    const resetSeasonBtnEl = document.getElementById('reset-season-btn');
    const adjustCashFormEl = document.getElementById('adjust-cash-form');
    const adjUidEl = document.getElementById('adj-uid');
    const adjNameEl = document.getElementById('adj-name');
    const adjOpEl = document.getElementById('adj-op');
    const adjValEl = document.getElementById('adj-val');
    const adjGuardEl = document.getElementById('adj-guard');
    const accountsListEl = document.getElementById('accounts-list');

    // Telemetry
    const telemetryLineEl = document.getElementById('telemetry-line');
    const focusQueueBoxEl = document.getElementById('focus-queue-box');
    const manageFeedListEl = document.getElementById('manage-feed-list');
    const manageQuotesGridEl = document.getElementById('manage-quotes-grid');

    // Auth Modal & Toast
    const authModalEl = document.getElementById('auth-modal');
    const authFormEl = document.getElementById('auth-form');
    const authTokenInputEl = document.getElementById('auth-token-input');
    const authErrorEl = document.getElementById('auth-error-text');
    const toastEl = document.getElementById('toast');

    // -------------------------------------------------------------------------
    // 2. Runtime State
    // -------------------------------------------------------------------------
    let currentMeta = null;
    let currentCfg = Object.assign({}, Engine.DEFAULT_CONFIG);
    const accountsMap = new Map();
    const quotesMap = new Map();
    const guardOverridesMap = new Map();
    let runtimeTelemetry = null;
    let runtimeFocus = null;
    let recentFeed = [];
    let toastTimer = null;
    let resetSeasonArmed = false;
    let resetSeasonTimer = null;

    // -------------------------------------------------------------------------
    // 3. UI Helpers (Safe DOM APIs only)
    // -------------------------------------------------------------------------
    function showToast(msg, isErr) {
        if (toastTimer) clearTimeout(toastTimer);
        toastEl.textContent = String(msg || '');
        toastEl.className = isErr ? 'toast visible error' : 'toast visible';
        toastTimer = setTimeout(function () {
            toastEl.className = 'toast';
        }, 3200);
    }

    function handleApiError(err, fallback) {
        if (err && err.code === 'UNAUTHORIZED') {
            authErrorEl.classList.add('hidden');
            authModalEl.classList.add('open');
            authTokenInputEl.focus();
            return;
        }
        showToast((err && err.message) || fallback || '操作失败', true);
    }

    function makeBadge(text, cls) {
        const span = document.createElement('span');
        span.className = cls ? ('badge ' + cls) : 'badge';
        span.textContent = text;
        return span;
    }

    function switchTab(name) {
        Object.keys(panels).forEach(function (k) {
            panels[k].classList.toggle('hidden', k !== name);
        });
        tabBtns.forEach(function (btn) {
            btn.classList.toggle('active', btn.getAttribute('data-tab') === name);
        });
    }

    function resolveObsUrl(meta, extraQuery) {
        const rel = meta && meta.route ? meta.route : ('/' + Dambo.plugin.name + '/');
        const url = new URL(rel, window.location.origin);
        if (extraQuery) {
            Object.keys(extraQuery).forEach(function (k) {
                url.searchParams.set(k, extraQuery[k]);
            });
        }
        return url.toString();
    }

    async function copyText(str, label) {
        try {
            await navigator.clipboard.writeText(str);
            showToast('已复制' + label + '：' + str);
        } catch (_) {
            showToast('请手动复制：' + str);
        }
    }

    // -------------------------------------------------------------------------
    // 4. Config Form Sync
    // -------------------------------------------------------------------------
    function populateConfigForm(cfg) {
        currentCfg = Object.assign({}, Engine.DEFAULT_CONFIG, cfg || {});
        cfgFinnhubKeyEl.value = String(currentCfg.finnhub_api_key || '');

        Object.keys(cfgEls).forEach(function (key) {
            const el = cfgEls[key];
            if (!el) return;
            if (el.type === 'checkbox') {
                el.checked = Boolean(currentCfg[key]);
            } else {
                el.value = currentCfg[key] !== undefined && currentCfg[key] !== null
                    ? String(currentCfg[key])
                    : '';
            }
        });

        const hasKey = Boolean(String(currentCfg.finnhub_api_key || '').trim());
        const effDemo = currentCfg.market_data_mode === 'demo' ||
            (currentCfg.market_data_mode === 'auto' && !hasKey);
        tabModeBadgeEl.textContent = effDemo ? '本地演练' : 'Finnhub 实盘';
        tabModeBadgeEl.className = effDemo ? 'badge warn' : 'badge ok';
        renderMetaBadges();
    }

    function collectConfigFromForm() {
        const out = {
            finnhub_api_key: cfgFinnhubKeyEl.value.trim(),
            market_data_mode: cfgEls.market_data_mode.value,
            market_session_override: cfgEls.market_session_override.value,
            poll_interval_sec: Math.round(Engine.clampNum(cfgEls.poll_interval_sec.value, 3, 120, 10)),
            enable_finnhub_ws: Boolean(cfgEls.enable_finnhub_ws.checked),
            allow_trading_when_closed: Boolean(cfgEls.allow_trading_when_closed.checked),
            marquee_major_tickers: cfgEls.marquee_major_tickers.value.trim().slice(0, 200),
            watchlist_tickers: cfgEls.watchlist_tickers.value.trim().slice(0, 200),
            require_fan_medal: Boolean(cfgEls.require_fan_medal.checked),
            guard_bypass_medal: Boolean(cfgEls.guard_bypass_medal.checked),
            room_medal_name: cfgEls.room_medal_name.value.trim().slice(0, 40),
            min_medal_level: Math.round(Engine.clampNum(cfgEls.min_medal_level.value, 1, 40, 1)),
            focus_auto_rotate_sec: Math.round(Engine.clampNum(cfgEls.focus_auto_rotate_sec.value, 3, 120, 10)),
            focus_lock_lv0_sec: Math.round(Engine.clampNum(cfgEls.focus_lock_lv0_sec.value, 3, 60, 8)),
            focus_lock_lv1_sec: Math.round(Engine.clampNum(cfgEls.focus_lock_lv1_sec.value, 4, 60, 12)),
            focus_lock_lv2_sec: Math.round(Engine.clampNum(cfgEls.focus_lock_lv2_sec.value, 5, 120, 20)),
            focus_lock_lv3_sec: Math.round(Engine.clampNum(cfgEls.focus_lock_lv3_sec.value, 5, 120, 25)),
            pos_card_duration_sec: Math.round(Engine.clampNum(cfgEls.pos_card_duration_sec.value, 3, 20, 6)),
            initial_balance: Math.round(Engine.clampNum(cfgEls.initial_balance.value, 1000, 100000000, 100000)),
            min_trade_usd: Math.round(Engine.clampNum(cfgEls.min_trade_usd.value, 1, 100000, 100)),
            short_leverage: Engine.clampNum(cfgEls.short_leverage.value, 1, 10, 2),
            liquidation_maint_ratio: Engine.clampNum(cfgEls.liquidation_maint_ratio.value, 0.05, 0.8, 0.2),
            commission_bps: Math.round(Engine.clampNum(cfgEls.commission_bps.value, 0, 100, 0)),
            trade_cooldown_ms: Math.round(Engine.clampNum(cfgEls.trade_cooldown_ms.value, 0, 15000, 1500)),
            whale_threshold_usd: Math.round(Engine.clampNum(cfgEls.whale_threshold_usd.value, 1000, 10000000, 25000)),
            banner_duration_sec: Math.round(Engine.clampNum(cfgEls.banner_duration_sec.value, 2, 15, 5)),
            allow_fractional_shares: Boolean(cfgEls.allow_fractional_shares.checked),
            hud_title: (cfgEls.hud_title.value.trim() || Engine.DEFAULT_CONFIG.hud_title).slice(0, 48),
            bg_mode: cfgEls.bg_mode.value,
            bg_opacity: Engine.clampNum(cfgEls.bg_opacity.value, 0, 1, 0.95),
            color_up: cfgEls.color_up.value || '#00E676',
            color_down: cfgEls.color_down.value || '#FF3B30',
            color_gold: cfgEls.color_gold.value || '#F59E0B',
            leaderboard_top_n: Math.round(Engine.clampNum(cfgEls.leaderboard_top_n.value, 5, 8, 6)),
            leaderboard_sort: cfgEls.leaderboard_sort.value,
            holding_rotate_sec: Engine.clampNum(cfgEls.holding_rotate_sec.value, 1.5, 15, 3.5),
            marquee_speed_sec: Math.round(Engine.clampNum(cfgEls.marquee_speed_sec.value, 12, 90, 38)),
            reserve_overlay_corner: Boolean(cfgEls.reserve_overlay_corner.checked),
        };
        return out;
    }

    function renderMetaBadges() {
        if (!currentMeta) return;
        const title = currentMeta.name || currentMeta.plugin_name;
        pluginTitleEl.textContent = title;
        document.title = title + ' · 控制面板';
        pluginDescEl.textContent = currentMeta.description || pluginDescEl.textContent;
        obsUrlCodeEl.textContent = resolveObsUrl(currentMeta);

        const hasKey = Boolean(String(currentCfg.finnhub_api_key || '').trim());
        pluginBadgesEl.replaceChildren(
            makeBadge('v' + currentMeta.version, 'accent'),
            makeBadge('1920×1080 HUD', 'accent'),
            makeBadge(hasKey ? 'Finnhub Key 已配置' : '当前无 Key (自动演练)', hasKey ? 'ok' : 'warn')
        );
    }

    // -------------------------------------------------------------------------
    // 5. Render Accounts & Telemetry
    // -------------------------------------------------------------------------
    function renderAccounts() {
        const ranked = Engine.computeLeaderboard(accountsMap, quotesMap, currentCfg);
        tabAccountsCountEl.textContent = String(ranked.length);
        accountsSummaryLineEl.textContent = '共 ' + ranked.length + ' 位已开户交易员（资产实时重算）';

        if (ranked.length === 0) {
            const empty = document.createElement('div');
            empty.className = 'item-card muted';
            empty.textContent = '暂无玩家账户，可通过上方“弹幕演练器”或直播间弹幕开户下单。';
            accountsListEl.replaceChildren(empty);
            return;
        }

        const frag = document.createDocumentFragment();
        ranked.forEach(function (row) {
            const card = document.createElement('div');
            card.className = 'item-card';

            const left = document.createElement('div');
            left.style.flex = '1 1 320px';
            const titleLine = document.createElement('div');
            titleLine.className = 'row';
            const rankSpan = document.createElement('strong');
            rankSpan.textContent = '#' + row.rank + ' ' + row.name;
            titleLine.appendChild(rankSpan);

            const gm = Engine.priorityToGuardMeta(row.guardPriority);
            if (gm.priority > 0) {
                titleLine.appendChild(makeBadge(gm.icon + ' ' + gm.label, 'warn'));
            }
            if (row.uid > 0) {
                titleLine.appendChild(makeBadge('UID ' + row.uid, ''));
            }
            if (row.liquidationCount > 0) {
                titleLine.appendChild(makeBadge('爆仓 ' + row.liquidationCount + ' 次', 'err'));
            }

            const detail = document.createElement('div');
            detail.className = 'muted';
            const holdDesc = row.holdings.length > 0
                ? row.holdings.map(function (h) {
                    return (h.side === 'short' ? '[空]' : '[多]') + h.ticker + '×' + Engine.formatShares(h.shares) + ' (' + Engine.formatPct(h.pnlPct) + ')';
                }).join(' · ')
                : '空仓';
            detail.textContent = '总资产 ' + Engine.formatUsd(row.totalEquity) +
                ' · 收益 ' + Engine.formatPct(row.pnlPct) +
                ' · 现金 ' + Engine.formatUsd(row.cash) +
                ' · ' + holdDesc;

            left.append(titleLine, detail);

            const actions = document.createElement('div');
            actions.className = 'row';
            const delBtn = document.createElement('button');
            delBtn.type = 'button';
            delBtn.className = 'small danger';
            delBtn.textContent = '删除账户';
            delBtn.addEventListener('click', async function () {
                try {
                    await Dambo.storage.delete('accounts', row.key);
                    accountsMap.delete(row.key);
                    renderAccounts();
                    showToast('已删除账户 ' + row.name);
                } catch (err) {
                    handleApiError(err, '删除账户失败');
                }
            });
            actions.appendChild(delBtn);

            card.append(left, actions);
            frag.appendChild(card);
        });
        accountsListEl.replaceChildren(frag);
    }

    function renderTelemetry() {
        if (runtimeTelemetry) {
            const t = runtimeTelemetry;
            runtimeHudBadgeEl.textContent = t.mode === 'demo'
                ? ('展示端在线 · 演练模式')
                : ('展示端在线 · ' + t.apiState + (t.wsState === 'open' ? ' + WS' : ''));
            runtimeHudBadgeEl.className = t.apiState === 'invalid_key' || t.apiState === 'rate_limited'
                ? 'badge warn'
                : 'badge ok';
            telemetryLineEl.textContent = '模式: ' + t.mode +
                ' | WS: ' + t.wsState + ' (' + (t.wsSubscribedCount || 0) + ' 标的)' +
                ' | 近1分钟 REST: ' + (t.requestsLastMin || 0) + '/' + (t.maxReqPerMin || 54);
        } else {
            runtimeHudBadgeEl.textContent = '等待连接展示端';
            runtimeHudBadgeEl.className = 'badge warn';
            telemetryLineEl.textContent = '尚未收到展示端上报数据（请确认已在 OBS/浏览器打开 /us-stock-hud/）';
        }

        if (runtimeFocus) {
            const lock = runtimeFocus.activeLock;
            const lockDesc = lock
                ? ('当前锁定: ' + lock.symbol + ' (' + lock.ownerName + ' Lv' + lock.priority + ')')
                : ('当前自动轮换: ' + runtimeFocus.currentFocus);
            const qDesc = (runtimeFocus.queueLength || 0) > 0
                ? (' · 排队中 (' + runtimeFocus.queueLength + '): ' + (runtimeFocus.queue || []).map(function (q) {
                    return q.symbol + '(' + q.ownerName + ':Lv' + q.priority + ')';
                }).join(', '))
                : ' · 队列空闲';
            focusQueueBoxEl.textContent = lockDesc + qDesc;
        }

        // Recent Feed
        if (recentFeed.length === 0) {
            const emp = document.createElement('div');
            emp.className = 'item-card muted';
            emp.textContent = '暂无成交或查股记录';
            manageFeedListEl.replaceChildren(emp);
        } else {
            const fFrag = document.createDocumentFragment();
            recentFeed.slice(0, 12).forEach(function (ev) {
                const row = document.createElement('div');
                row.className = 'item-card';
                const left = document.createElement('span');
                left.textContent = '[' + (ev.type || 'trade') + '] ' + (ev.name || '观众') + '：' + (ev.summary || '');
                const right = document.createElement('span');
                right.className = 'muted';
                right.textContent = ev.snapshotText || new Date(ev.ts || Date.now()).toLocaleTimeString();
                row.append(left, right);
                fFrag.appendChild(row);
            });
            manageFeedListEl.replaceChildren(fFrag);
        }

        // Quotes Grid
        const syms = Array.from(quotesMap.keys()).sort();
        tabQuotesCountEl.textContent = String(syms.length);
        if (syms.length === 0) {
            const qEmp = document.createElement('div');
            qEmp.className = 'item-card muted';
            qEmp.textContent = '暂无行情缓存';
            manageQuotesGridEl.replaceChildren(qEmp);
        } else {
            const qFrag = document.createDocumentFragment();
            syms.forEach(function (sym) {
                const q = quotesMap.get(sym);
                const tile = document.createElement('div');
                tile.className = 'q-tile';
                const top = document.createElement('div');
                top.className = 'between';
                const sEl = document.createElement('strong');
                sEl.textContent = sym;
                const pctEl = document.createElement('span');
                pctEl.className = Number(q.dp) >= 0 ? 'badge ok' : 'badge err';
                pctEl.textContent = Engine.formatPct(q.dp);
                top.append(sEl, pctEl);

                const pEl = document.createElement('div');
                pEl.style.fontSize = '16px';
                pEl.style.fontWeight = '800';
                pEl.style.marginTop = '4px';
                pEl.textContent = Engine.formatPrice(q.c);

                const srcEl = document.createElement('div');
                srcEl.className = 'muted';
                srcEl.textContent = String(q.source || 'cache');
                tile.append(top, pEl, srcEl);
                qFrag.appendChild(tile);
            });
            manageQuotesGridEl.replaceChildren(qFrag);
        }
    }

    // -------------------------------------------------------------------------
    // 6. Control Helpers & Event Bindings
    // -------------------------------------------------------------------------
    async function sendControlCommand(payload) {
        const msg = Object.assign({}, payload, { nonce: Date.now() + '_' + Math.random().toString(36).slice(2, 7) });
        try {
            await Dambo.memory.set('control', 'command', msg);
            return true;
        } catch (err) {
            handleApiError(err, '发送控制指令失败');
            return false;
        }
    }

    tabBtns.forEach(function (btn) {
        btn.addEventListener('click', function () {
            switchTab(btn.getAttribute('data-tab'));
        });
    });

    copyObsBtnEl.addEventListener('click', function () {
        copyText(resolveObsUrl(currentMeta), 'OBS 链接');
    });
    copyObsTransparentBtnEl.addEventListener('click', function () {
        copyText(resolveObsUrl(currentMeta, { bg_mode: 'transparent' }), '纯透明 OBS 链接');
    });

    toggleKeyBtnEl.addEventListener('click', function () {
        const isPass = cfgFinnhubKeyEl.type === 'password';
        cfgFinnhubKeyEl.type = isPass ? 'text' : 'password';
        toggleKeyBtnEl.textContent = isPass ? '隐藏' : '显示';
    });

    saveKeyBtnEl.addEventListener('click', async function () {
        try {
            await Dambo.config.set('finnhub_api_key', cfgFinnhubKeyEl.value.trim());
            showToast('Finnhub API Key 已保存并热同步');
        } catch (err) {
            handleApiError(err, '保存 Key 失败');
        }
    });

    testKeyBtnEl.addEventListener('click', async function () {
        const key = cfgFinnhubKeyEl.value.trim();
        if (!key) {
            keyTestResultEl.textContent = '请先输入 Finnhub API Key';
            return;
        }
        keyTestResultEl.textContent = '正在请求 Finnhub AAPL 实时报价…';
        try {
            const res = await fetch('https://finnhub.io/api/v1/quote?symbol=AAPL&token=' + encodeURIComponent(key));
            if (res.status === 401 || res.status === 403) {
                keyTestResultEl.textContent = '认证失败 (HTTP ' + res.status + ')：请核对 API Key';
                return;
            }
            if (res.status === 429) {
                keyTestResultEl.textContent = '触发限流 (HTTP 429)，请 30 秒后重试';
                return;
            }
            const data = await res.json();
            if (data && Number(data.c) > 0) {
                keyTestResultEl.textContent = '连通成功！AAPL 当前价 ' + Engine.formatPrice(data.c) + ' (' + Engine.formatPct(data.dp) + ')';
            } else {
                keyTestResultEl.textContent = '请求成功但未返回有效价格';
            }
        } catch (err) {
            keyTestResultEl.textContent = '网络请求失败：' + (err && err.message ? err.message : '请检查网络');
        }
    });

    saveAllCfgBtnEl.addEventListener('click', async function () {
        try {
            const payload = collectConfigFromForm();
            await Dambo.config.set(payload);
            populateConfigForm(payload);
            showToast('全部参数已保存并热更新至 HUD');
        } catch (err) {
            handleApiError(err, '保存配置失败');
        }
    });

    resetAllCfgBtnEl.addEventListener('click', async function () {
        try {
            await Dambo.config.reset();
            const fresh = await Dambo.config.get();
            populateConfigForm(fresh);
            showToast('已恢复全部默认参数');
        } catch (err) {
            handleApiError(err, '恢复默认失败');
        }
    });

    // Simulator Presets
    const PRESETS = {
        'no-medal': { uid: 9001, uname: '路人甲(无牌)', medal: '', lv: 0, guard: '0', content: '买入 NVDA 5000' },
        'lv0-quote': { uid: 9002, uname: '普通粉丝', medal: '小鲸鱼', lv: 8, guard: '0', content: '查股 AAPL' },
        'lv2-preempt': { uid: 9003, uname: '华尔街提督', medal: '小鲸鱼', lv: 22, guard: '2', content: '查股 TSLA' },
        'lv3-whale': { uid: 9004, uname: '氪金总督', medal: '小鲸鱼', lv: 30, guard: '1', content: '买入 NVDA all' },
        'short-tsla': { uid: 9005, uname: '空军司令', medal: '小鲸鱼', lv: 15, guard: '3', content: '做空 TSLA 25000' },
        'cover-tsla': { uid: 9005, uname: '空军司令', medal: '小鲸鱼', lv: 15, guard: '3', content: '平空 TSLA' },
        'pos-card': { uid: 9004, uname: '氪金总督', medal: '小鲸鱼', lv: 30, guard: '1', content: '持仓' },
    };

    presetBtns.forEach(function (btn) {
        btn.addEventListener('click', async function () {
            const p = PRESETS[btn.getAttribute('data-preset')];
            if (!p) return;
            simUidEl.value = String(p.uid);
            simUnameEl.value = p.uname;
            simMedalNameEl.value = p.medal;
            simMedalLvEl.value = String(p.lv);
            simGuardLevelEl.value = p.guard;
            simContentEl.value = p.content;

            const ok = await sendControlCommand({
                action: 'sim_danmaku',
                item: {
                    kind: 'chat',
                    uid: p.uid,
                    uname: p.uname,
                    medal_name: p.medal || undefined,
                    medal_level: p.lv || undefined,
                    guard_level: Number(p.guard) > 0 ? Number(p.guard) : undefined,
                    content: p.content,
                    timestamp: Date.now(),
                },
            });
            if (ok) showToast('已发送演练弹幕：' + p.uname + ' -> ' + p.content);
        });
    });

    simFormEl.addEventListener('submit', async function (e) {
        e.preventDefault();
        const uid = Math.max(0, Math.floor(Number(simUidEl.value || 0)));
        const uname = simUnameEl.value.trim() || '测试玩家';
        const medal = simMedalNameEl.value.trim();
        const lv = Math.max(0, Math.floor(Number(simMedalLvEl.value || 0)));
        const guard = Number(simGuardLevelEl.value || 0);
        const content = simContentEl.value.trim();
        if (!content) return;

        const ok = await sendControlCommand({
            action: 'sim_danmaku',
            item: {
                kind: 'chat',
                uid: uid,
                uname: uname,
                medal_name: medal || undefined,
                medal_level: medal ? lv : undefined,
                guard_level: guard > 0 ? guard : undefined,
                content: content,
                timestamp: Date.now(),
            },
        });
        if (ok) showToast('已推送测试弹幕：' + content);
    });

    ctrlPinBtnEl.addEventListener('click', async function () {
        const sym = Engine.normalizeTicker(ctrlSymbolEl.value);
        if (!sym) {
            showToast('请输入合法美股代码', true);
            return;
        }
        if (await sendControlCommand({ action: 'pin_focus', symbol: sym })) {
            showToast('已切换主图焦点至 ' + sym);
        }
    });

    ctrlPumpBtnEl.addEventListener('click', async function () {
        const sym = Engine.normalizeTicker(ctrlSymbolEl.value);
        if (!sym) {
            showToast('请输入合法美股代码', true);
            return;
        }
        if (await sendControlCommand({ action: 'price_shock', symbol: sym, pct: 48 })) {
            showToast('已触发 ' + sym + ' 暴涨 +48% 演练');
        }
    });

    ctrlDumpBtnEl.addEventListener('click', async function () {
        const sym = Engine.normalizeTicker(ctrlSymbolEl.value);
        if (!sym) {
            showToast('请输入合法美股代码', true);
            return;
        }
        if (await sendControlCommand({ action: 'price_shock', symbol: sym, pct: -25 })) {
            showToast('已触发 ' + sym + ' 暴跌 -25% 演练');
        }
    });

    ctrlClearFeedBtnEl.addEventListener('click', async function () {
        if (await sendControlCommand({ action: 'clear_feed' })) {
            recentFeed = [];
            renderTelemetry();
            showToast('已清空成交流水展示');
        }
    });

    // Accounts Management
    adjustCashFormEl.addEventListener('submit', async function (e) {
        e.preventDefault();
        const uid = Math.max(0, Math.floor(Number(adjUidEl.value || 0)));
        const name = adjNameEl.value.trim();
        if (!name) return;
        const player = { uid: uid, uname: name };
        const key = Engine.accountKeyForPlayer(player);
        const val = Number(adjValEl.value || 0);
        const op = adjOpEl.value;
        const guardOv = adjGuardEl.value;

        let acct = accountsMap.get(key);
        if (!acct) {
            acct = Engine.createInitialAccount(player, 0, currentCfg, Date.now());
        } else {
            acct = JSON.parse(JSON.stringify(acct));
            acct.name = name;
        }

        if (op === 'set') {
            acct.cash = Engine.roundMoney(Math.max(0, val));
        } else {
            acct.cash = Engine.roundMoney(Math.max(0, Number(acct.cash || 0) + val));
        }
        acct.updated_at = Date.now();

        try {
            if (guardOv !== '') {
                const pri = Number(guardOv);
                acct.guard_priority = pri;
                if (pri === 0) {
                    await Dambo.storage.delete('guard_overrides', key);
                    guardOverridesMap.delete(key);
                } else {
                    await Dambo.storage.set('guard_overrides', key, pri);
                    guardOverridesMap.set(key, pri);
                }
            }
            await Dambo.storage.set('accounts', key, acct);
            accountsMap.set(key, acct);
            renderAccounts();
            showToast('已更新玩家 ' + name + ' 账户');
        } catch (err) {
            handleApiError(err, '更新账户失败');
        }
    });

    resetSeasonBtnEl.addEventListener('click', async function () {
        if (!resetSeasonArmed) {
            resetSeasonArmed = true;
            resetSeasonBtnEl.textContent = '再次点击确认清空全部账户！';
            if (resetSeasonTimer) clearTimeout(resetSeasonTimer);
            resetSeasonTimer = setTimeout(function () {
                resetSeasonArmed = false;
                resetSeasonBtnEl.textContent = '重置新赛季 (清空全员账本)';
            }, 4000);
            return;
        }
        resetSeasonArmed = false;
        if (resetSeasonTimer) clearTimeout(resetSeasonTimer);
        resetSeasonBtnEl.textContent = '重置新赛季 (清空全员账本)';

        try {
            await Dambo.storage.clearNamespace('accounts');
            accountsMap.clear();
            renderAccounts();
            showToast('新赛季已开启，全部玩家账本已清空');
        } catch (err) {
            handleApiError(err, '重置赛季失败');
        }
    });

    authFormEl.addEventListener('submit', async function (e) {
        e.preventDefault();
        try {
            await Dambo.plugin.unlockManage(authTokenInputEl.value);
            authModalEl.classList.remove('open');
            authTokenInputEl.value = '';
            showToast('管理员口令验证成功');
            loadAll();
        } catch (err) {
            authErrorEl.textContent = (err && err.message) || '口令错误';
            authErrorEl.classList.remove('hidden');
        }
    });

    reloadAllBtnEl.addEventListener('click', loadAll);

    // -------------------------------------------------------------------------
    // 7. Data Loading & Live Watches
    // -------------------------------------------------------------------------
    async function loadAll() {
        try {
            const results = await Promise.all([
                Dambo.plugin.meta({ force: true }),
                Dambo.config.get(),
                Dambo.storage.list('accounts').catch(function () { return {}; }),
                Dambo.storage.list('guard_overrides').catch(function () { return {}; }),
                Dambo.memory.list('quotes').catch(function () { return {}; }),
                Dambo.memory.list('runtime').catch(function () { return {}; }),
            ]);
            currentMeta = results[0];
            populateConfigForm(results[1]);

            accountsMap.clear();
            Object.keys(results[2] || {}).forEach(function (k) {
                accountsMap.set(k, results[2][k]);
            });

            guardOverridesMap.clear();
            Object.keys(results[3] || {}).forEach(function (k) {
                guardOverridesMap.set(k, Number(results[3][k]));
            });

            quotesMap.clear();
            Object.keys(results[4] || {}).forEach(function (k) {
                quotesMap.set(k, results[4][k]);
            });

            const rt = results[5] || {};
            runtimeTelemetry = rt.telemetry || null;
            runtimeFocus = rt.focus || null;
            if (Array.isArray(rt.recent_feed)) {
                recentFeed = rt.recent_feed;
            }

            renderMetaBadges();
            renderAccounts();
            renderTelemetry();
        } catch (err) {
            handleApiError(err, '加载控制台数据失败');
        }
    }

    Dambo.config.watch(function () {
        Dambo.config.get().then(populateConfigForm).catch(function () {});
    });

    Dambo.storage.watch('accounts', function (ev) {
        if (!ev) return;
        if (ev.op === 'clear') {
            accountsMap.clear();
            renderAccounts();
            return;
        }
        if (ev.op === 'delete' && Array.isArray(ev.keys)) {
            ev.keys.forEach(function (k) { accountsMap.delete(k); });
            renderAccounts();
            return;
        }
        if (ev.values) {
            Object.keys(ev.values).forEach(function (k) { accountsMap.set(k, ev.values[k]); });
            renderAccounts();
            return;
        }
        Dambo.storage.list('accounts').then(function (all) {
            accountsMap.clear();
            Object.keys(all || {}).forEach(function (k) { accountsMap.set(k, all[k]); });
            renderAccounts();
        }).catch(function () {});
    });

    Dambo.memory.watch('quotes', function (ev) {
        if (ev && ev.values) {
            Object.keys(ev.values).forEach(function (k) { quotesMap.set(k, ev.values[k]); });
            renderAccounts();
            renderTelemetry();
        }
    });

    Dambo.memory.watch('runtime', function (ev) {
        if (ev && ev.values) {
            if (ev.values.telemetry) runtimeTelemetry = ev.values.telemetry;
            if (ev.values.focus) runtimeFocus = ev.values.focus;
            if (Array.isArray(ev.values.recent_feed)) recentFeed = ev.values.recent_feed;
            renderTelemetry();
        }
    });

    Dambo.topics.subscribe('@self', 'trade_feed', function (msg) {
        if (msg && msg.payload) {
            recentFeed.unshift(msg.payload);
            if (recentFeed.length > 30) recentFeed.length = 30;
            renderTelemetry();
        }
    });

    loadAll();
})();
