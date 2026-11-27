(function () {
    'use strict';

    const Engine = window.UsStockEngine;
    const MarketLib = window.UsStockMarket;
    const ChartLib = window.UsStockChart;

    // -------------------------------------------------------------------------
    // 1. DOM References
    // -------------------------------------------------------------------------
    const hudRootEl = document.getElementById('hud-root');
    const hudTitleEl = document.getElementById('hud-title');
    const sessionPillEl = document.getElementById('market-session-pill');
    const sessionTextEl = document.getElementById('market-session-text');
    const etClockEl = document.getElementById('et-clock');
    const dataModePillEl = document.getElementById('data-mode-pill');
    const wsLinkPillEl = document.getElementById('ws-link-pill');
    const topbarSafeCornerEl = document.getElementById('topbar-safe-corner');
    const marqueeMajorTrackEl = document.getElementById('marquee-major-track');
    const marqueeHotTrackEl = document.getElementById('marquee-hot-track');

    const watchlistGridEl = document.getElementById('watchlist-grid');
    const focusSymbolEl = document.getElementById('focus-symbol');
    const focusNameEl = document.getElementById('focus-name');
    const focusLockPillEl = document.getElementById('focus-lock-pill');
    const focusPriceEl = document.getElementById('focus-price');
    const focusChangeEl = document.getElementById('focus-change');
    const alertBannerSlotEl = document.getElementById('alert-banner-slot');
    const hudOpenEl = document.getElementById('hud-open');
    const hudPrevEl = document.getElementById('hud-prev');
    const hudHighEl = document.getElementById('hud-high');
    const hudLowEl = document.getElementById('hud-low');
    const hudVolEl = document.getElementById('hud-vol');
    const hudAvgCostWrapEl = document.getElementById('hud-avg-cost-wrap');
    const hudAvgCostEl = document.getElementById('hud-avg-cost');
    const focusChartCanvasEl = document.getElementById('focus-chart-canvas');

    const superQuoteOverlayEl = document.getElementById('super-quote-overlay');
    const sqSymbolEl = document.getElementById('sq-symbol');
    const sqNameEl = document.getElementById('sq-name');
    const sqOwnerBadgeEl = document.getElementById('sq-owner-badge');
    const sqCountdownEl = document.getElementById('sq-countdown');
    const sqPriceEl = document.getElementById('sq-price');
    const sqChangeEl = document.getElementById('sq-change');
    const sqExtPillEl = document.getElementById('sq-ext-pill');
    const sqDayRangeTextEl = document.getElementById('sq-day-range-text');
    const sqDayFillEl = document.getElementById('sq-day-fill');
    const sqDayDotEl = document.getElementById('sq-day-dot');
    const sqW52RangeTextEl = document.getElementById('sq-w52-range-text');
    const sqW52FillEl = document.getElementById('sq-w52-fill');
    const sqW52DotEl = document.getElementById('sq-w52-dot');
    const sqRoomMetaEl = document.getElementById('sq-room-meta');
    const sqSplitLongEl = document.getElementById('sq-split-long');
    const sqSplitShortEl = document.getElementById('sq-split-short');
    const sqProgressBarEl = document.getElementById('sq-progress-bar');

    const battleLongLabelEl = document.querySelector('.battle-long .battle-label');
    const battleShortLabelEl = document.querySelector('.battle-short .battle-label');
    const battleLongValEl = document.getElementById('battle-long-val');
    const battleShortValEl = document.getElementById('battle-short-val');
    const battleLongFillEl = document.getElementById('battle-long-fill');
    const battleShortFillEl = document.getElementById('battle-short-fill');
    const battleDividerEl = document.getElementById('battle-divider');

    const lbSortPillEl = document.getElementById('lb-sort-pill');
    const lbStatsTextEl = document.getElementById('lb-stats-text');
    const posCardOverlayEl = document.getElementById('pos-card-overlay');
    const leaderboardListEl = document.getElementById('leaderboard-list');
    const tsCountTextEl = document.getElementById('ts-count-text');
    const timeSalesListEl = document.getElementById('time-sales-list');
    const medalGateNoticeEl = document.getElementById('medal-gate-notice');

    // -------------------------------------------------------------------------
    // 2. Runtime State
    // -------------------------------------------------------------------------
    const MAX_FEED_ITEMS = 40;
    let cfg = Object.assign({}, Engine.DEFAULT_CONFIG);
    let currentRoomId = 0;

    const accountsMap = new Map();
    const guardCacheMap = new Map();
    const guardOverrideMap = new Map();
    const cooldownMap = new Map();
    const busySet = new Set();
    let timeSales = [];
    let holdingRotateTick = 0;
    let activeBanner = null;
    let bannerTimerId = null;

    const focusScheduler = Engine.createFocusScheduler('NVDA');
    const posCardQueue = Engine.createPosCardQueue();

    // Marquee DOM cache (in-place updates prevent scrolling reset)
    let majorSymbolsSig = '';
    let hotSymbolsSig = '';
    const marqueeNodePools = {
        major: new Map(), // symbol -> [{ priceEl, pctEl }, ...]
        hot: new Map(),
    };

    let renderRafPending = false;
    let runtimeSyncTimer = null;
    let quotesSyncTimer = null;
    let wsStateLabel = 'connecting';

    const market = MarketLib.createMarketEngine({
        onQuoteUpdate: function (sym, quote) {
            const liqEvents = Engine.checkLiquidationsForSymbol(sym, quote.c, accountsMap, cfg, Date.now());
            if (liqEvents.length > 0) {
                handleLiquidationBatch(liqEvents);
            }
            scheduleRender();
            scheduleQuotesMemorySync();
            scheduleRuntimeSync();
        },
        onStatusChange: function () {
            renderDataModeBadge();
            scheduleRuntimeSync();
        },
    });

    // -------------------------------------------------------------------------
    // 3. Config + Visual Theme Application
    // -------------------------------------------------------------------------
    function applyQueryOverrides(rawCfg) {
        const merged = Object.assign({}, Engine.DEFAULT_CONFIG, rawCfg || {});
        const qpBgMode = Dambo.utils.getQueryParam('bg_mode');
        if (qpBgMode === 'solid' || qpBgMode === 'glass' || qpBgMode === 'transparent') {
            merged.bg_mode = qpBgMode;
        }
        if (Dambo.utils.getQueryBool('transparent', false)) {
            merged.bg_mode = 'transparent';
        }
        const qpOpacity = Dambo.utils.getQueryParam('bg_opacity');
        if (qpOpacity !== null && Number.isFinite(Number(qpOpacity))) {
            merged.bg_opacity = Engine.clampNum(qpOpacity, 0, 1, merged.bg_opacity);
        }
        if (Dambo.utils.getQueryParam('reserve_overlay_corner') !== null) {
            merged.reserve_overlay_corner = Dambo.utils.getQueryBool('reserve_overlay_corner', true);
        }
        return merged;
    }

    function applyVisualConfig() {
        document.body.classList.remove('bg-solid', 'bg-glass', 'bg-transparent');
        const mode = cfg.bg_mode === 'glass' || cfg.bg_mode === 'transparent' ? cfg.bg_mode : 'solid';
        document.body.classList.add('bg-' + mode);

        const rootStyle = document.documentElement.style;
        rootStyle.setProperty('--bg-opacity', String(Engine.clampNum(cfg.bg_opacity, 0, 1, 0.95)));
        rootStyle.setProperty('--color-up', cfg.color_up || '#00E676');
        rootStyle.setProperty('--color-down', cfg.color_down || '#FF3B30');
        rootStyle.setProperty('--color-gold', cfg.color_gold || '#F59E0B');
        rootStyle.setProperty('--marquee-sec', String(Engine.clampNum(cfg.marquee_speed_sec, 12, 90, 38)) + 's');

        hudTitleEl.textContent = cfg.hud_title || Engine.DEFAULT_CONFIG.hud_title;
        topbarSafeCornerEl.classList.toggle('hidden', cfg.reserve_overlay_corner === false);
        lbSortPillEl.textContent = cfg.leaderboard_sort === 'return_pct' ? '收益率' : '总资产';

        if (!cfg.require_fan_medal) {
            medalGateNoticeEl.textContent = '全员开放参与 (免粉丝牌)';
            medalGateNoticeEl.classList.add('is-open');
        } else {
            const roomMedal = String(cfg.room_medal_name || '').trim();
            const lvMin = Engine.clampNum(cfg.min_medal_level, 1, 40, 1);
            let txt = roomMedal
                ? ('需佩戴「' + roomMedal + '」粉丝牌参与')
                : '需佩戴粉丝牌参与';
            if (lvMin > 1) txt += ' (Lv.' + lvMin + '+)';
            medalGateNoticeEl.textContent = txt;
            medalGateNoticeEl.classList.remove('is-open');
        }
    }

    function getMajorMarqueeList() {
        return Engine.parseTickerList(
            cfg.marquee_major_tickers,
            Engine.DEFAULT_CONFIG.marquee_major_tickers,
            12
        );
    }

    function getWatchlist() {
        const list = Engine.parseTickerList(
            cfg.watchlist_tickers,
            Engine.DEFAULT_CONFIG.watchlist_tickers,
            8
        );
        return list.length > 0 ? list : ['NVDA', 'TSLA', 'AAPL', 'MSFT', 'SPY', 'QQQ', 'META', 'AMZN'];
    }

    function refreshWatchUniverse() {
        const major = getMajorMarqueeList();
        const watch = getWatchlist();
        const hot = Engine.computeTopHotTickers(accountsMap, market.quotesMap, watch, 12).map(function (h) {
            return h.symbol;
        });
        const focus = focusScheduler.getState(Date.now()).currentFocus;
        const all = [focus].concat(watch, hot, major);
        market.setWatchUniverse(all);
    }

    // -------------------------------------------------------------------------
    // 4. Helper UI Builders (Safe DOM APIs only)
    // -------------------------------------------------------------------------
    function createGuardBadge(priority) {
        const meta = Engine.priorityToGuardMeta(priority);
        if (meta.priority <= 0) return null;
        const span = document.createElement('span');
        span.className = 'guard-badge ' + meta.cssClass;
        span.textContent = meta.icon + ' ' + meta.label;
        return span;
    }

    function formatShortClock(tsMs) {
        const d = new Date(tsMs || Date.now());
        return String(d.getHours()).padStart(2, '0') + ':' +
            String(d.getMinutes()).padStart(2, '0') + ':' +
            String(d.getSeconds()).padStart(2, '0');
    }

    function showBanner(type, text) {
        if (bannerTimerId !== null) {
            clearTimeout(bannerTimerId);
            bannerTimerId = null;
        }
        const durMs = Engine.clampNum(cfg.banner_duration_sec, 2, 15, 5) * 1000;
        activeBanner = {
            type: type, // 'whale' | 'liq'
            text: text,
            expiresAt: Date.now() + durMs,
        };
        renderBanner();
        bannerTimerId = setTimeout(function () {
            bannerTimerId = null;
            activeBanner = null;
            renderBanner();
        }, durMs);
    }

    function renderBanner() {
        if (!activeBanner || Date.now() >= activeBanner.expiresAt) {
            alertBannerSlotEl.classList.add('hidden');
            alertBannerSlotEl.replaceChildren();
            return;
        }
        alertBannerSlotEl.className = 'alert-banner-slot ' + (activeBanner.type === 'liq' ? 'is-liq' : 'is-whale');
        const iconSpan = document.createElement('span');
        iconSpan.textContent = activeBanner.type === 'liq' ? '💥 爆仓警报' : '🐋 WHALE ALERT';
        const textSpan = document.createElement('span');
        textSpan.textContent = activeBanner.text;
        alertBannerSlotEl.replaceChildren(iconSpan, textSpan);
    }

    // -------------------------------------------------------------------------
    // 5. Render Functions
    // -------------------------------------------------------------------------
    function renderClockAndSession() {
        const sess = Engine.getUsMarketSession(Date.now(), cfg.market_session_override);
        sessionPillEl.className = 'session-pill is-' + sess.code;
        sessionTextEl.textContent = sess.fullLabel;
        etClockEl.textContent = sess.etTimeStr + ' ' + sess.tzName;
    }

    function renderDataModeBadge() {
        const tel = market.getTelemetry();
        if (tel.mode === 'demo') {
            dataModePillEl.className = 'meta-pill warn';
            dataModePillEl.textContent = '演练行情';
        } else if (tel.apiState === 'invalid_key') {
            dataModePillEl.className = 'meta-pill error';
            dataModePillEl.textContent = 'Key 无效';
        } else if (tel.apiState === 'rate_limited') {
            dataModePillEl.className = 'meta-pill warn';
            dataModePillEl.textContent = 'REST 限流冷却';
        } else if (tel.wsState === 'open') {
            dataModePillEl.className = 'meta-pill ok';
            dataModePillEl.textContent = 'Finnhub WS+REST';
        } else {
            dataModePillEl.className = 'meta-pill ok';
            dataModePillEl.textContent = 'Finnhub REST';
        }

        if (wsStateLabel === 'open') {
            wsLinkPillEl.className = 'meta-pill ok';
            wsLinkPillEl.textContent = '弹幕已连接';
        } else if (wsStateLabel === 'reconnecting') {
            wsLinkPillEl.className = 'meta-pill warn';
            wsLinkPillEl.textContent = '弹幕重连中';
        } else {
            wsLinkPillEl.className = 'meta-pill';
            wsLinkPillEl.textContent = '弹幕连接中';
        }
    }

    function buildMarqueeTrack(trackEl, poolMap, symbols) {
        trackEl.replaceChildren();
        poolMap.clear();
        if (!symbols || symbols.length === 0) return;

        // Duplicate list twice for seamless -50% CSS marquee loop
        const frag = document.createDocumentFragment();
        for (let pass = 0; pass < 2; pass++) {
            for (let i = 0; i < symbols.length; i++) {
                const sym = symbols[i];
                const item = document.createElement('div');
                item.className = 'mq-item';
                const symEl = document.createElement('span');
                symEl.className = 'mq-sym';
                symEl.textContent = sym;
                const priceEl = document.createElement('span');
                priceEl.className = 'mq-price tabular';
                const pctEl = document.createElement('span');
                pctEl.className = 'mq-pct tabular';
                item.append(symEl, priceEl, pctEl);
                frag.appendChild(item);

                if (!poolMap.has(sym)) poolMap.set(sym, []);
                poolMap.get(sym).push({ priceEl: priceEl, pctEl: pctEl });
            }
        }
        trackEl.appendChild(frag);
    }

    function updateMarqueeValues(poolMap, symbols) {
        for (let i = 0; i < symbols.length; i++) {
            const sym = symbols[i];
            const nodes = poolMap.get(sym);
            if (!nodes) continue;
            const q = market.getQuote(sym);
            const hasPrice = q && Number(q.c) > 0;
            const dp = hasPrice ? Number(q.dp || 0) : 0;
            const dirClass = !hasPrice ? 'flat' : (dp > 0.001 ? 'is-up' : (dp < -0.001 ? 'is-down' : 'flat'));
            const priceText = hasPrice ? Engine.formatPrice(q.c) : '$--.--';
            const pctText = hasPrice ? ((dp > 0 ? '▲' : (dp < 0 ? '▼' : '')) + Engine.formatPct(dp)) : '--';

            for (let j = 0; j < nodes.length; j++) {
                nodes[j].priceEl.textContent = priceText;
                nodes[j].pctEl.textContent = pctText;
                nodes[j].pctEl.className = 'mq-pct tabular ' + dirClass;
            }
        }
    }

    function renderMarquees() {
        const majors = getMajorMarqueeList();
        const majSig = majors.join(',');
        if (majSig !== majorSymbolsSig) {
            majorSymbolsSig = majSig;
            buildMarqueeTrack(marqueeMajorTrackEl, marqueeNodePools.major, majors);
        }
        updateMarqueeValues(marqueeNodePools.major, majors);

        const hotObjs = Engine.computeTopHotTickers(accountsMap, market.quotesMap, getWatchlist(), 12);
        const hots = hotObjs.map(function (h) { return h.symbol; });
        const hotSig = hots.join(',');
        if (hotSig !== hotSymbolsSig) {
            hotSymbolsSig = hotSig;
            buildMarqueeTrack(marqueeHotTrackEl, marqueeNodePools.hot, hots);
        }
        updateMarqueeValues(marqueeNodePools.hot, hots);
    }

    function renderWatchlistGrid() {
        const list = getWatchlist();
        const now = Date.now();
        const fState = focusScheduler.getState(now);
        const rotMs = Engine.clampNum(cfg.focus_auto_rotate_sec, 3, 120, 10) * 1000;
        const frag = document.createDocumentFragment();

        for (let i = 0; i < 8; i++) {
            const sym = list[i];
            if (!sym) break;
            const q = market.getQuote(sym);
            const hasPrice = q && Number(q.c) > 0;
            const dp = hasPrice ? Number(q.dp || 0) : 0;
            const isUp = dp >= 0;
            const dirClass = !hasPrice ? 'flat' : (dp > 0.001 ? 'is-up' : (dp < -0.001 ? 'is-down' : 'flat'));
            const isFocus = fState.currentFocus === sym;
            const isLocked = isFocus && Boolean(fState.activeLock);

            const card = document.createElement('div');
            card.className = 'watchlist-card' +
                (isFocus ? ' is-focus' : '') +
                (isLocked ? ' is-locked' : '');

            const top = document.createElement('div');
            top.className = 'wl-top';
            const symEl = document.createElement('span');
            symEl.className = 'wl-sym';
            symEl.textContent = sym;
            const pctEl = document.createElement('span');
            pctEl.className = 'wl-pct tabular ' + dirClass;
            pctEl.textContent = hasPrice ? ((dp > 0 ? '+' : '') + dp.toFixed(2) + '%') : '--';
            top.append(symEl, pctEl);

            const bottom = document.createElement('div');
            bottom.className = 'wl-bottom';
            const priceEl = document.createElement('span');
            priceEl.className = 'wl-price tabular';
            priceEl.textContent = hasPrice ? Engine.formatPrice(q.c) : '$--.--';

            const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
            svg.setAttribute('class', 'wl-spark');
            ChartLib.renderSparkline(
                svg,
                market.getSparklinePrices(sym),
                isUp,
                cfg.color_up || '#00E676',
                cfg.color_down || '#FF3B30'
            );
            bottom.append(priceEl, svg);

            card.append(top, bottom);

            if (isFocus) {
                const track = document.createElement('div');
                track.className = 'wl-focus-bar';
                const fill = document.createElement('div');
                fill.className = 'wl-focus-bar-fill';
                let pct = 100;
                if (isLocked && fState.activeLock.durationMs > 0) {
                    pct = Math.max(0, Math.min(100, (fState.remainingMs / fState.activeLock.durationMs) * 100));
                } else if (rotMs > 0) {
                    const elapsed = Math.max(0, now - fState.lastRotateAt);
                    pct = Math.max(0, Math.min(100, 100 - (elapsed / rotMs) * 100));
                }
                fill.style.width = pct.toFixed(1) + '%';
                track.appendChild(fill);
                card.appendChild(track);
            }

            frag.appendChild(card);
        }

        watchlistGridEl.replaceChildren(frag);
    }

    function renderFocusChartAndOverlays() {
        const now = Date.now();
        const fState = focusScheduler.getState(now);
        const sym = fState.currentFocus;
        const q = market.getQuote(sym);
        const dirInfo = Engine.STOCK_DIRECTORY[sym];
        const roomStats = Engine.computeRoomSymbolStats(sym, accountsMap, market.quotesMap);

        focusSymbolEl.textContent = sym;
        focusNameEl.textContent = (q && q.name) || (dirInfo && dirInfo.name) || sym;

        if (fState.activeLock) {
            const lock = fState.activeLock;
            const secLeft = Math.ceil(fState.remainingMs / 1000);
            const gLabel = lock.guardMeta && lock.guardMeta.label ? (lock.guardMeta.icon + ' ' + lock.guardMeta.label + ' ') : '';
            const qTail = fState.queueLength > 0 ? (' · 队列 ' + fState.queueLength) : '';
            focusLockPillEl.className = 'focus-lock-pill' + (lock.priority > 0 ? ' is-guard' : '');
            focusLockPillEl.textContent = gLabel + lock.ownerName + ' 锁定 ' + secLeft + 's' + qTail;
        } else {
            const rotSec = Engine.clampNum(cfg.focus_auto_rotate_sec, 3, 120, 10);
            focusLockPillEl.className = 'focus-lock-pill';
            focusLockPillEl.textContent = '自选自动轮换 ' + rotSec + 's';
        }

        if (q && q.c > 0) {
            const dp = Number(q.dp || 0);
            const dVal = Number(q.d || 0);
            const dirClass = dp > 0.001 ? 'is-up' : (dp < -0.001 ? 'is-down' : 'flat');
            focusPriceEl.textContent = Engine.formatPrice(q.c);
            focusPriceEl.className = 'focus-price tabular ' + dirClass;
            focusChangeEl.textContent = (dVal >= 0 ? '+' : '') + dVal.toFixed(2) + ' (' + Engine.formatPct(dp) + ')';
            focusChangeEl.className = 'focus-change tabular ' + dirClass;

            hudOpenEl.textContent = Engine.formatPrice(q.o);
            hudPrevEl.textContent = Engine.formatPrice(q.pc);
            hudHighEl.textContent = Engine.formatPrice(q.h);
            hudLowEl.textContent = Engine.formatPrice(q.l);
            hudVolEl.textContent = Engine.formatVolume(q.volume);
        } else {
            focusPriceEl.textContent = '$--.--';
            focusPriceEl.className = 'focus-price tabular';
            focusChangeEl.textContent = '--';
            focusChangeEl.className = 'focus-change tabular flat';
            hudOpenEl.textContent = '--';
            hudPrevEl.textContent = '--';
            hudHighEl.textContent = '--';
            hudLowEl.textContent = '--';
            hudVolEl.textContent = '--';
        }

        if (roomStats.overallAvgCost > 0) {
            hudAvgCostWrapEl.classList.remove('hidden');
            hudAvgCostEl.textContent = Engine.formatPrice(roomStats.overallAvgCost);
        } else {
            hudAvgCostWrapEl.classList.add('hidden');
        }

        ChartLib.drawIntradayChart(focusChartCanvasEl, {
            symbol: sym,
            quote: q,
            series: market.getSeries(sym),
            roomStats: roomStats,
            colorUp: cfg.color_up || '#00E676',
            colorDown: cfg.color_down || '#FF3B30',
            colorGold: cfg.color_gold || '#F59E0B',
        });

        // Super Quote Card (shown when activeLock.showSuperCard is true)
        const showSuper = Boolean(fState.activeLock && fState.activeLock.showSuperCard && fState.remainingMs > 0);
        if (!showSuper) {
            superQuoteOverlayEl.classList.add('hidden');
        } else {
            const lock = fState.activeLock;
            superQuoteOverlayEl.classList.remove('hidden');
            superQuoteOverlayEl.classList.toggle('is-guard-priority', lock.priority > 0);

            sqSymbolEl.textContent = sym;
            sqNameEl.textContent = (q && q.name) || (dirInfo && dirInfo.name) || sym;

            sqOwnerBadgeEl.replaceChildren();
            const gBadge = createGuardBadge(lock.priority);
            if (gBadge) sqOwnerBadgeEl.appendChild(gBadge);
            const ownerTxt = document.createElement('span');
            ownerTxt.textContent = lock.ownerName;
            sqOwnerBadgeEl.appendChild(ownerTxt);

            const secLeft = Math.max(1, Math.ceil(fState.remainingMs / 1000));
            sqCountdownEl.textContent = secLeft + 's';

            if (q && q.c > 0) {
                const dp = Number(q.dp || 0);
                const dVal = Number(q.d || 0);
                const dirCls = dp > 0.001 ? 'is-up' : (dp < -0.001 ? 'is-down' : 'flat');
                sqPriceEl.textContent = Engine.formatPrice(q.c);
                sqPriceEl.className = 'sq-price tabular ' + dirCls;
                sqChangeEl.textContent = (dVal >= 0 ? '+' : '') + dVal.toFixed(2) + ' (' + Engine.formatPct(dp) + ')';
                sqChangeEl.className = 'sq-change tabular ' + dirCls;

                if (q.extPrice && q.extPrice > 0) {
                    sqExtPillEl.classList.remove('hidden');
                    sqExtPillEl.textContent = (q.extLabel || '盘前/盘后') + ' ' + Engine.formatPrice(q.extPrice);
                } else {
                    sqExtPillEl.classList.add('hidden');
                }

                const dayLow = Number(q.l || q.c);
                const dayHigh = Number(q.h || q.c);
                const dayRatio = dayHigh > dayLow ? Math.max(0, Math.min(1, (q.c - dayLow) / (dayHigh - dayLow))) : 0.5;
                sqDayRangeTextEl.textContent = Engine.formatPrice(dayLow) + ' ~ ' + Engine.formatPrice(dayHigh);
                sqDayFillEl.style.width = (dayRatio * 100).toFixed(1) + '%';
                sqDayDotEl.style.left = (dayRatio * 100).toFixed(1) + '%';

                const w52Low = Number(q.w52Low || (dayLow * 0.78));
                const w52High = Number(q.w52High || (dayHigh * 1.24));
                const w52Ratio = w52High > w52Low ? Math.max(0, Math.min(1, (q.c - w52Low) / (w52High - w52Low))) : 0.5;
                sqW52RangeTextEl.textContent = Engine.formatPrice(w52Low) + ' ~ ' + Engine.formatPrice(w52High);
                sqW52FillEl.style.width = (w52Ratio * 100).toFixed(1) + '%';
                sqW52DotEl.style.left = (w52Ratio * 100).toFixed(1) + '%';
            }

            if (roomStats.totalNotional > 0) {
                sqRoomMetaEl.textContent = '多 ' + roomStats.longPct + '% (' + roomStats.longHolders + '人) / 空 ' + roomStats.shortPct + '% (' + roomStats.shortHolders + '人)';
                sqSplitLongEl.style.width = roomStats.longPct + '%';
                sqSplitShortEl.style.width = roomStats.shortPct + '%';
            } else {
                sqRoomMetaEl.textContent = '暂无直播间持仓 · 多 50% / 空 50%';
                sqSplitLongEl.style.width = '50%';
                sqSplitShortEl.style.width = '50%';
            }

            const pctRemain = lock.durationMs > 0
                ? Math.max(0, Math.min(100, (fState.remainingMs / lock.durationMs) * 100))
                : 0;
            sqProgressBarEl.style.width = pctRemain.toFixed(1) + '%';
        }
    }

    function renderBattleBar() {
        const sym = focusScheduler.getState(Date.now()).currentFocus;
        const symStats = Engine.computeRoomSymbolStats(sym, accountsMap, market.quotesMap);
        let longUsd = 0;
        let shortUsd = 0;
        let longPct = 50;
        let shortPct = 50;

        if (symStats.totalNotional > 0) {
            longUsd = symStats.longNotional;
            shortUsd = symStats.shortNotional;
            longPct = symStats.longPct;
            shortPct = symStats.shortPct;
            if (battleLongLabelEl) battleLongLabelEl.textContent = sym + ' 看涨做多';
            if (battleShortLabelEl) battleShortLabelEl.textContent = sym + ' 看跌做空';
        } else {
            const globalStats = Engine.computeGlobalBattleStats(accountsMap, market.quotesMap);
            longUsd = globalStats.totalLongUsd;
            shortUsd = globalStats.totalShortUsd;
            longPct = globalStats.longPct;
            shortPct = globalStats.shortPct;
            if (battleLongLabelEl) battleLongLabelEl.textContent = '全服看涨做多';
            if (battleShortLabelEl) battleShortLabelEl.textContent = '全服看跌做空';
        }

        battleLongValEl.textContent = Engine.formatUsd(longUsd, true) + ' (' + longPct.toFixed(1) + '%)';
        battleShortValEl.textContent = '(' + shortPct.toFixed(1) + '%) ' + Engine.formatUsd(shortUsd, true);
        battleLongFillEl.style.width = longPct.toFixed(1) + '%';
        battleShortFillEl.style.width = shortPct.toFixed(1) + '%';
        battleDividerEl.style.left = longPct.toFixed(1) + '%';
    }

    function renderPosCardOverlay() {
        const state = posCardQueue.getState(Date.now());
        if (!state.activeCard || state.remainingMs <= 0) {
            posCardOverlayEl.classList.add('hidden');
            posCardOverlayEl.replaceChildren();
            return;
        }
        const card = state.activeCard;
        const m = card.metrics;
        const pctDir = m.pnlPct > 0.001 ? 'is-up' : (m.pnlPct < -0.001 ? 'is-down' : 'flat');

        const head = document.createElement('div');
        head.className = 'pos-card-head';

        const userWrap = document.createElement('div');
        userWrap.className = 'pos-card-user';
        const av = Dambo.utils.createAvatar(card.face || '', 'pos-avatar');
        av.alt = card.name;
        userWrap.appendChild(av);
        const gBadge = createGuardBadge(card.priority);
        if (gBadge) userWrap.appendChild(gBadge);
        const nameEl = document.createElement('span');
        nameEl.className = 'pos-uname';
        nameEl.textContent = card.name;
        userWrap.appendChild(nameEl);

        const rankPill = document.createElement('span');
        rankPill.className = 'pos-rank-pill tabular';
        rankPill.textContent = '全服第 #' + card.rank + ' · ' + Math.ceil(state.remainingMs / 1000) + 's';
        head.append(userWrap, rankPill);

        const sumGrid = document.createElement('div');
        sumGrid.className = 'pos-summary-grid';

        const eqBox = document.createElement('div');
        const eqK = document.createElement('span');
        eqK.className = 'pos-metric-k';
        eqK.textContent = '总资产';
        const eqV = document.createElement('span');
        eqV.className = 'pos-metric-v tabular is-gold';
        eqV.textContent = Engine.formatUsd(m.totalEquity);
        eqBox.append(eqK, eqV);

        const cashBox = document.createElement('div');
        const cashK = document.createElement('span');
        cashK.className = 'pos-metric-k';
        cashK.textContent = '可用现金';
        const cashV = document.createElement('span');
        cashV.className = 'pos-metric-v tabular';
        cashV.textContent = Engine.formatUsd(m.cash);
        cashBox.append(cashK, cashV);

        const retBox = document.createElement('div');
        const retK = document.createElement('span');
        retK.className = 'pos-metric-k';
        retK.textContent = '总收益率';
        const retV = document.createElement('span');
        retV.className = 'pos-metric-v tabular ' + pctDir;
        retV.textContent = Engine.formatPct(m.pnlPct);
        retBox.append(retK, retV);

        sumGrid.append(eqBox, cashBox, retBox);

        const list = document.createElement('div');
        list.className = 'pos-holdings-list';
        if (m.holdings.length === 0) {
            const empty = document.createElement('div');
            empty.className = 'pos-holding-row';
            empty.textContent = '当前空仓观望 · 100% 现金持有中';
            list.appendChild(empty);
        } else {
            const topHoldings = m.holdings.slice(0, 4);
            for (let i = 0; i < topHoldings.length; i++) {
                const h = topHoldings[i];
                const hRow = document.createElement('div');
                hRow.className = 'pos-holding-row';
                const left = document.createElement('span');
                left.className = h.side === 'short' ? 'is-down' : 'is-up';
                left.textContent = (h.side === 'short' ? '[空] ' : '[多] ') + h.ticker + ' ×' + Engine.formatShares(h.shares);
                const mid = document.createElement('span');
                mid.className = 'tabular';
                mid.textContent = '市值 ' + Engine.formatUsd(h.notional);
                const right = document.createElement('span');
                right.className = 'tabular ' + (h.unrealizedPnl >= 0 ? 'is-up' : 'is-down');
                right.textContent = Engine.formatSignedUsd(h.unrealizedPnl) + ' (' + Engine.formatPct(h.pnlPct) + ')';
                hRow.append(left, mid, right);
                list.appendChild(hRow);
            }
        }

        const progTrack = document.createElement('div');
        progTrack.className = 'pos-progress-track';
        const progBar = document.createElement('div');
        progBar.className = 'pos-progress-bar';
        const pctRem = card.durationMs > 0 ? Math.max(0, Math.min(100, (state.remainingMs / card.durationMs) * 100)) : 0;
        progBar.style.width = pctRem.toFixed(1) + '%';
        progTrack.appendChild(progBar);

        posCardOverlayEl.classList.remove('hidden');
        posCardOverlayEl.replaceChildren(head, sumGrid, list, progTrack);
    }

    function renderLeaderboard() {
        const limit = Math.round(Engine.clampNum(cfg.leaderboard_top_n, 5, 8, 6));
        const ranked = Engine.computeLeaderboard(accountsMap, market.quotesMap, cfg);
        lbStatsTextEl.textContent = ranked.length + ' 位交易员';

        if (ranked.length === 0) {
            const empty = document.createElement('li');
            empty.className = 'empty-state';
            empty.textContent = '佩戴粉丝牌发送“买入(buy) NVDA 10000”立即自动开户入榜';
            leaderboardListEl.replaceChildren(empty);
            return;
        }

        const slice = ranked.slice(0, limit);
        const frag = document.createDocumentFragment();
        for (let i = 0; i < slice.length; i++) {
            const row = slice[i];
            const li = document.createElement('li');
            li.className = 'lb-row' + (row.rank === 1 ? ' rank-1' : '');

            const rankEl = document.createElement('span');
            rankEl.className = 'lb-rank tabular';
            rankEl.textContent = row.rank === 1 ? '👑1' : ('#' + row.rank);

            const av = Dambo.utils.createAvatar(row.face || '', 'lb-avatar');
            av.alt = row.name;

            const identity = document.createElement('div');
            identity.className = 'lb-identity';

            const nameLine = document.createElement('div');
            nameLine.className = 'lb-name-line';
            const gBadge = createGuardBadge(row.guardPriority);
            if (gBadge) nameLine.appendChild(gBadge);
            const nameEl = document.createElement('span');
            nameEl.className = 'lb-name';
            nameEl.textContent = row.name;
            nameLine.appendChild(nameEl);

            const holdEl = document.createElement('span');
            holdEl.className = 'lb-holding-pill tabular';
            if (row.holdings.length > 0) {
                const h = row.holdings[holdingRotateTick % row.holdings.length];
                const dirStr = h.side === 'short' ? '空' : '多';
                const pnlCls = h.unrealizedPnl >= 0 ? 'is-up' : 'is-down';
                holdEl.className = 'lb-holding-pill tabular ' + pnlCls;
                holdEl.textContent = '重仓[' + dirStr + '] ' + h.ticker + ' ' + Engine.formatUsd(h.notional, true) + ' (' + Engine.formatPct(h.pnlPct) + ')';
            } else {
                holdEl.textContent = '现金持币 ' + Engine.formatUsd(row.cash, true);
            }

            identity.append(nameLine, holdEl);

            const nums = document.createElement('div');
            nums.className = 'lb-numbers';
            const eqEl = document.createElement('span');
            eqEl.className = 'lb-equity tabular';
            eqEl.textContent = Engine.formatUsd(row.totalEquity);
            const retEl = document.createElement('span');
            const dir = row.pnlPct > 0.001 ? 'is-up' : (row.pnlPct < -0.001 ? 'is-down' : 'flat');
            retEl.className = 'lb-ret tabular ' + dir;
            retEl.textContent = Engine.formatPct(row.pnlPct);
            nums.append(eqEl, retEl);

            li.append(rankEl, av, identity, nums);
            frag.appendChild(li);
        }

        leaderboardListEl.replaceChildren(frag);
    }

    function tagMetaForFeed(ev) {
        if (ev.type === 'liq') return { text: '爆仓', cls: 'is-liq' };
        if (ev.type === 'buy') return { text: ev.isAllIn ? '梭哈' : '买入', cls: 'is-buy' };
        if (ev.type === 'sell') return { text: '卖出', cls: 'is-sell' };
        if (ev.type === 'short') return { text: '做空', cls: 'is-short' };
        if (ev.type === 'cover') return { text: '平空', cls: 'is-cover' };
        if (ev.type === 'quote') return { text: '查股', cls: 'is-quote' };
        return { text: '持仓', cls: 'is-pos' };
    }

    function renderTimeSales() {
        tsCountTextEl.textContent = timeSales.length + ' 条流水';
        if (timeSales.length === 0) {
            const empty = document.createElement('li');
            empty.className = 'empty-state';
            empty.textContent = '等待佩戴粉丝牌玩家发送交易或查股指令…';
            timeSalesListEl.replaceChildren(empty);
            return;
        }

        const slice = timeSales.slice(0, 10);
        const frag = document.createDocumentFragment();
        for (let i = 0; i < slice.length; i++) {
            const ev = slice[i];
            const li = document.createElement('li');
            li.className = 'ts-row type-' + ev.type + (ev.ok === false ? ' is-fail' : '');

            const timeEl = document.createElement('span');
            timeEl.className = 'ts-time tabular';
            timeEl.textContent = formatShortClock(ev.ts);
            li.appendChild(timeEl);

            const tm = tagMetaForFeed(ev);
            const tagEl = document.createElement('span');
            tagEl.className = 'ts-tag ' + tm.cls;
            tagEl.textContent = tm.text;
            li.appendChild(tagEl);

            const gBadge = createGuardBadge(ev.priority);
            if (gBadge) li.appendChild(gBadge);

            const body = document.createElement('div');
            body.className = 'ts-body';
            const nameEl = document.createElement('span');
            nameEl.className = 'ts-name';
            nameEl.textContent = ev.name || '观众';
            const sumEl = document.createElement('span');
            sumEl.className = 'ts-summary';
            sumEl.textContent = ev.summary || '';
            body.append(nameEl, sumEl);
            li.appendChild(body);

            if (ev.snapshotText) {
                const snapEl = document.createElement('span');
                snapEl.className = 'ts-snapshot tabular';
                snapEl.textContent = ev.snapshotText;
                li.appendChild(snapEl);
            }

            frag.appendChild(li);
        }
        timeSalesListEl.replaceChildren(frag);
    }

    function renderAll() {
        renderClockAndSession();
        renderDataModeBadge();
        renderMarquees();
        renderWatchlistGrid();
        renderFocusChartAndOverlays();
        renderBattleBar();
        renderPosCardOverlay();
        renderLeaderboard();
        renderTimeSales();
        renderBanner();
    }

    function scheduleRender() {
        if (renderRafPending) return;
        renderRafPending = true;
        requestAnimationFrame(function () {
            renderRafPending = false;
            renderAll();
        });
    }

    // -------------------------------------------------------------------------
    // 6. Dambo Runtime Telemetry Sync & Topic Broadcast
    // -------------------------------------------------------------------------
    function pushFeedEvent(ev) {
        timeSales.unshift(ev);
        if (timeSales.length > MAX_FEED_ITEMS) {
            timeSales = timeSales.slice(0, MAX_FEED_ITEMS);
        }
        Dambo.topics.publish('trade_feed', ev, {
            public: true,
            description: '美股实盘 HUD 弹幕成交流水',
        }).catch(function () {});
        Dambo.memory.set('runtime', 'recent_feed', timeSales.slice(0, 30)).catch(function () {});
        scheduleRender();
        scheduleRuntimeSync();
    }

    function scheduleQuotesMemorySync() {
        if (quotesSyncTimer !== null) return;
        quotesSyncTimer = setTimeout(function () {
            quotesSyncTimer = null;
            const out = {};
            let count = 0;
            market.quotesMap.forEach(function (q, sym) {
                if (count >= 40) return;
                out[sym] = {
                    symbol: q.symbol,
                    name: q.name,
                    c: q.c,
                    pc: q.pc,
                    dp: q.dp,
                    h: q.h,
                    l: q.l,
                    o: q.o,
                    source: q.source,
                    fetchedAt: q.fetchedAt,
                };
                count += 1;
            });
            if (count > 0) {
                Dambo.memory.mset('quotes', out).catch(function () {});
            }
        }, 1200);
    }

    function scheduleRuntimeSync() {
        if (runtimeSyncTimer !== null) return;
        runtimeSyncTimer = setTimeout(function () {
            runtimeSyncTimer = null;
            const fState = focusScheduler.getState(Date.now());
            const ranked = Engine.computeLeaderboard(accountsMap, market.quotesMap, cfg);
            Dambo.memory.mset('runtime', {
                telemetry: market.getTelemetry(),
                focus: {
                    currentFocus: fState.currentFocus,
                    activeLock: fState.activeLock,
                    queueLength: fState.queueLength,
                    queue: fState.queue.slice(0, 8),
                },
                leaderboard: ranked.slice(0, 20),
                battle: Engine.computeGlobalBattleStats(accountsMap, market.quotesMap),
                recent_feed: timeSales.slice(0, 30),
                updated_at: Date.now(),
            }).catch(function () {});
        }, 450);
    }

    // -------------------------------------------------------------------------
    // 7. Guard Learning & Danmaku Command Execution Pipeline
    // -------------------------------------------------------------------------
    function learnGuardFromEvent(item) {
        if (!item || typeof item !== 'object') return;
        const pri = Engine.mapBiliGuardToPriority(item.guard_level);
        if (pri <= 0) return;
        const acctKey = Engine.accountKeyForPlayer(item);
        const existing = guardCacheMap.get(acctKey);
        if (!existing || existing.priority !== pri || (item.uname && existing.name !== item.uname)) {
            const record = {
                uid: Number(item.uid || 0),
                name: String(item.uname || '观众'),
                priority: pri,
                biliLevel: Number(item.guard_level),
                updated_at: Date.now(),
            };
            guardCacheMap.set(acctKey, record);
            Dambo.storage.set('guards', acctKey, record).catch(function () {});
        }
        const acct = accountsMap.get(acctKey);
        if (acct && acct.guard_priority !== pri) {
            acct.guard_priority = pri;
            accountsMap.set(acctKey, acct);
            Dambo.storage.set('accounts', acctKey, acct).catch(function () {});
        }
    }

    function handleLiquidationBatch(liqEvents) {
        for (let i = 0; i < liqEvents.length; i++) {
            const ev = liqEvents[i];
            accountsMap.set(ev.acctKey, ev.account);
            Dambo.storage.set('accounts', ev.acctKey, ev.account).catch(function () {});
            pushFeedEvent({
                id: 'liq_' + ev.acctKey + '_' + ev.ts + '_' + i,
                type: 'liq',
                ok: true,
                uid: ev.account.uid,
                name: ev.account.name,
                priority: ev.priority,
                ticker: ev.ticker,
                summary: '强平 ' + ev.ticker + ' ×' + Engine.formatShares(ev.shares) + ' @ ' + Engine.formatPrice(ev.markPrice) + ' · 亏损 ' + Engine.formatUsd(Math.abs(ev.loss)),
                snapshotText: ev.snapshotText,
                ts: ev.ts,
            });
            showBanner('liq', ev.account.name + ' 做空 ' + ev.ticker + ' 触发强平 @ ' + Engine.formatPrice(ev.markPrice) + ' · 损失 ' + Engine.formatUsd(Math.abs(ev.loss)));
        }
        refreshWatchUniverse();
        scheduleRender();
    }

    async function processPlayerCommand(item, cmd, optsProcess) {
        const o = optsProcess || {};
        const bypassCooldown = Boolean(o.bypassCooldown);
        const acctKey = Engine.accountKeyForPlayer(item);
        const now = Date.now();
        const cooldownMs = Engine.clampNum(cfg.trade_cooldown_ms, 0, 15000, 1500);

        if (!bypassCooldown) {
            const last = cooldownMap.get(acctKey) || 0;
            if (now - last < cooldownMs || busySet.has(acctKey)) {
                return;
            }
            cooldownMap.set(acctKey, now);
        }
        busySet.add(acctKey);

        try {
            const priority = Engine.resolvePlayerPriority(item, guardCacheMap, guardOverrideMap);
            let acct = accountsMap.get(acctKey);
            let needPersistAcct = false;
            if (!acct || typeof acct !== 'object') {
                acct = Engine.createInitialAccount(item, priority, cfg, now);
                needPersistAcct = true;
            } else {
                acct = JSON.parse(JSON.stringify(acct));
                if (item.uname) acct.name = item.uname;
                if (item.face) acct.face = item.face;
                if (item.medal_name) acct.medal_name = item.medal_name;
                if (item.medal_level) acct.medal_level = Number(item.medal_level);
                acct.guard_priority = priority;
                if (!acct.positions || typeof acct.positions !== 'object') acct.positions = {};
            }

            // -------------------------------------------------------------
            // Command 1: 持仓(pos)
            // -------------------------------------------------------------
            if (cmd.kind === 'pos') {
                if (needPersistAcct) {
                    accountsMap.set(acctKey, acct);
                    await Dambo.storage.set('accounts', acctKey, acct);
                }
                const ranked = Engine.computeLeaderboard(accountsMap, market.quotesMap, cfg);
                const found = ranked.find(function (r) { return r.key === acctKey; });
                const rank = found ? found.rank : ranked.length;
                const metrics = Engine.computeAccountMetrics(acct, market.quotesMap, cfg);

                posCardQueue.push({
                    acctKey: acctKey,
                    uid: acct.uid,
                    name: acct.name,
                    face: acct.face,
                    priority: priority,
                    rank: rank,
                    metrics: metrics,
                }, Date.now(), cfg);

                const topHold = metrics.holdings.length > 0
                    ? metrics.holdings.slice(0, 2).map(function (h) {
                        return (h.side === 'short' ? '空' : '多') + h.ticker + '×' + Engine.formatShares(h.shares);
                    }).join(' ')
                    : '空仓';

                pushFeedEvent({
                    id: 'pos_' + acctKey + '_' + Date.now(),
                    type: 'pos',
                    ok: true,
                    uid: acct.uid,
                    name: acct.name,
                    priority: priority,
                    summary: '第#' + rank + ' · 总资产 ' + Engine.formatUsd(metrics.totalEquity) + ' (' + Engine.formatPct(metrics.pnlPct) + ')',
                    snapshotText: topHold + ' · 现金 ' + Engine.formatUsd(metrics.cash),
                    ts: Date.now(),
                });
                scheduleRender();
                return;
            }

            // -------------------------------------------------------------
            // Command 2: 查股(quote) 代码
            // -------------------------------------------------------------
            if (cmd.kind === 'quote') {
                const qRes = await market.ensureQuote(cmd.ticker, { withMetrics: true });
                if (!qRes.ok) {
                    pushFeedEvent({
                        id: 'quote_fail_' + acctKey + '_' + Date.now(),
                        type: 'quote',
                        ok: false,
                        uid: acct.uid,
                        name: acct.name,
                        priority: priority,
                        summary: '查股 ' + cmd.ticker + ' 失败：' + qRes.reason,
                        ts: Date.now(),
                    });
                    return;
                }

                const focusRes = focusScheduler.requestFocus({
                    symbol: cmd.ticker,
                    kind: 'quote',
                    showSuperCard: true,
                    priority: priority,
                    ownerUid: acct.uid,
                    ownerName: acct.name,
                    ownerFace: acct.face,
                }, Date.now(), cfg);

                const q = qRes.quote;
                const note = focusRes.activated
                    ? (focusRes.preempted ? ' [高权抢占切屏]' : ' [已锁定主图]')
                    : (' [排队#' + focusRes.queuePos + ']');

                pushFeedEvent({
                    id: 'quote_' + acctKey + '_' + Date.now(),
                    type: 'quote',
                    ok: true,
                    uid: acct.uid,
                    name: acct.name,
                    priority: priority,
                    ticker: cmd.ticker,
                    summary: '查股 ' + cmd.ticker + ' ' + Engine.formatPrice(q.c) + ' (' + Engine.formatPct(q.dp) + ')' + note,
                    ts: Date.now(),
                });
                refreshWatchUniverse();
                scheduleRender();
                return;
            }

            // -------------------------------------------------------------
            // Commands 3-6: 买入 / 卖出 / 做空 / 平空
            // -------------------------------------------------------------
            const session = Engine.getUsMarketSession(Date.now(), cfg.market_session_override);
            if (!cfg.allow_trading_when_closed && session.code === 'closed') {
                pushFeedEvent({
                    id: 'closed_' + acctKey + '_' + Date.now(),
                    type: cmd.kind,
                    ok: false,
                    uid: acct.uid,
                    name: acct.name,
                    priority: priority,
                    summary: '当前美股已闭市，暂未开放非盘中模拟交易',
                    ts: Date.now(),
                });
                return;
            }

            const qRes = await market.ensureQuote(cmd.ticker, { forceFresh: false });
            if (!qRes.ok) {
                pushFeedEvent({
                    id: 'trade_fail_' + acctKey + '_' + Date.now(),
                    type: cmd.kind,
                    ok: false,
                    uid: acct.uid,
                    name: acct.name,
                    priority: priority,
                    summary: cmd.ticker + ' 下单失败：' + qRes.reason,
                    ts: Date.now(),
                });
                return;
            }

            const execRes = Engine.executeTrade(acct, cmd, qRes.quote.c, cfg, Date.now());
            if (!execRes.ok) {
                pushFeedEvent({
                    id: 'trade_err_' + acctKey + '_' + Date.now(),
                    type: cmd.kind,
                    ok: false,
                    uid: acct.uid,
                    name: acct.name,
                    priority: priority,
                    summary: execRes.reason,
                    ts: Date.now(),
                });
                return;
            }

            accountsMap.set(acctKey, execRes.account);
            await Dambo.storage.set('accounts', acctKey, execRes.account);

            focusScheduler.requestFocus({
                symbol: cmd.ticker,
                kind: 'trade',
                showSuperCard: false,
                priority: priority,
                ownerUid: acct.uid,
                ownerName: acct.name,
                ownerFace: acct.face,
            }, Date.now(), cfg);

            const tr = execRes.trade;
            pushFeedEvent({
                id: 'tr_' + acctKey + '_' + Date.now(),
                type: tr.kind,
                ok: true,
                isAllIn: tr.isAllIn,
                uid: acct.uid,
                name: acct.name,
                priority: priority,
                ticker: tr.ticker,
                summary: tr.summary,
                snapshotText: tr.snapshotText,
                ts: Date.now(),
            });

            if (tr.isWhale) {
                const verb = tr.kind === 'short' ? '巨额做空' : (tr.isAllIn ? '全仓梭哈' : '巨鲸大单买入');
                showBanner('whale', acct.name + ' ' + verb + ' ' + tr.ticker + ' ' + Engine.formatUsd(tr.notional) + ' @ ' + Engine.formatPrice(tr.price));
            }

            refreshWatchUniverse();
            scheduleRender();
        } catch (err) {
            console.error('[us-stock-hud] command error:', err);
        } finally {
            busySet.delete(acctKey);
        }
    }

    function handleIncomingChatEvent(item, opts) {
        if (!item || typeof item !== 'object') return;
        learnGuardFromEvent(item);
        const cmd = Engine.parseDanmakuCommand(item.content || '');
        if (!cmd) return;

        const pri = Engine.resolvePlayerPriority(item, guardCacheMap, guardOverrideMap);
        const gate = Engine.checkFanMedalGate(item, cfg, currentRoomId, pri);
        if (!gate.allowed) {
            // 粉丝牌硬性准入：无灯牌/不合规用户的指令静默忽略
            return;
        }
        processPlayerCommand(item, cmd, opts);
    }

    // -------------------------------------------------------------------------
    // 8. Dambo SDK Connection, Config, Storage & Control Sync
    // -------------------------------------------------------------------------
    async function loadConfigAndApply() {
        try {
            const raw = await Dambo.config.get();
            cfg = applyQueryOverrides(raw);
        } catch (_) {
            cfg = applyQueryOverrides({});
        }
        applyVisualConfig();
        market.setConfig(cfg);
        refreshWatchUniverse();
        scheduleRender();
    }

    async function hydrateStorage() {
        const results = await Promise.all([
            Dambo.storage.list('accounts').catch(function () { return {}; }),
            Dambo.storage.list('guards').catch(function () { return {}; }),
            Dambo.storage.list('guard_overrides').catch(function () { return {}; }),
            Dambo.memory.list('quotes').catch(function () { return {}; }),
            Dambo.memory.get('runtime', 'recent_feed').catch(function () { return null; }),
        ]);

        const accts = results[0] || {};
        accountsMap.clear();
        Object.keys(accts).forEach(function (k) {
            if (accts[k] && typeof accts[k] === 'object') {
                accountsMap.set(k, accts[k]);
            }
        });

        const guards = results[1] || {};
        guardCacheMap.clear();
        Object.keys(guards).forEach(function (k) {
            if (guards[k] && typeof guards[k] === 'object') {
                guardCacheMap.set(k, guards[k]);
            }
        });

        const overrides = results[2] || {};
        guardOverrideMap.clear();
        Object.keys(overrides).forEach(function (k) {
            guardOverrideMap.set(k, Number(overrides[k]));
        });

        market.hydrateQuotes(results[3] || {});
        if (Array.isArray(results[4])) {
            timeSales = results[4].slice(0, MAX_FEED_ITEMS);
        }

        refreshWatchUniverse();
        scheduleRender();
    }

    function handleControlCommand(ctrl) {
        if (!ctrl || typeof ctrl !== 'object') return;
        if (ctrl.action === 'sim_danmaku' && ctrl.item) {
            handleIncomingChatEvent(ctrl.item, { bypassCooldown: true });
        } else if (ctrl.action === 'pin_focus' && ctrl.symbol) {
            focusScheduler.setManualFocus(ctrl.symbol, Date.now(), cfg);
            market.ensureQuote(ctrl.symbol, { withMetrics: true }).catch(function () {});
            refreshWatchUniverse();
            scheduleRender();
            scheduleRuntimeSync();
        } else if (ctrl.action === 'price_shock' && ctrl.symbol) {
            const q = market.simulatePriceShock(ctrl.symbol, Number(ctrl.pct || 0));
            if (q) {
                const liqEvents = Engine.checkLiquidationsForSymbol(ctrl.symbol, q.c, accountsMap, cfg, Date.now());
                if (liqEvents.length > 0) {
                    handleLiquidationBatch(liqEvents);
                }
                scheduleRender();
                scheduleRuntimeSync();
            }
        } else if (ctrl.action === 'clear_feed') {
            timeSales = [];
            Dambo.memory.set('runtime', 'recent_feed', []).catch(function () {});
            scheduleRender();
            scheduleRuntimeSync();
        }
    }

    Dambo.config.watch(loadConfigAndApply);

    Dambo.storage.watch('accounts', function (ev) {
        if (!ev) return;
        if (ev.op === 'clear') {
            accountsMap.clear();
            posCardQueue.clear();
            refreshWatchUniverse();
            scheduleRender();
            scheduleRuntimeSync();
            return;
        }
        if (ev.op === 'delete' && Array.isArray(ev.keys)) {
            ev.keys.forEach(function (k) { accountsMap.delete(k); });
            refreshWatchUniverse();
            scheduleRender();
            scheduleRuntimeSync();
            return;
        }
        if (ev.values && typeof ev.values === 'object') {
            Object.keys(ev.values).forEach(function (k) {
                if (ev.values[k]) accountsMap.set(k, ev.values[k]);
            });
            refreshWatchUniverse();
            scheduleRender();
            scheduleRuntimeSync();
            return;
        }
        Dambo.storage.list('accounts').then(function (all) {
            accountsMap.clear();
            Object.keys(all || {}).forEach(function (k) { accountsMap.set(k, all[k]); });
            refreshWatchUniverse();
            scheduleRender();
            scheduleRuntimeSync();
        }).catch(function () {});
    });

    Dambo.storage.watch('guard_overrides', function (ev) {
        if (!ev) return;
        if (ev.op === 'clear') {
            guardOverrideMap.clear();
            return;
        }
        if (ev.op === 'delete' && Array.isArray(ev.keys)) {
            ev.keys.forEach(function (k) { guardOverrideMap.delete(k); });
            return;
        }
        if (ev.values && typeof ev.values === 'object') {
            Object.keys(ev.values).forEach(function (k) {
                guardOverrideMap.set(k, Number(ev.values[k]));
            });
        }
    });

    Dambo.memory.watch('control', function (ev) {
        if (!ev || !ev.values) return;
        if (ev.values.command) {
            handleControlCommand(ev.values.command);
        }
    });

    const client = Dambo.connect({
        channel: 'feed',
        url: Dambo.utils.getQueryParam('ws') || undefined,
        replaySnapshot: false,
        showLike: false,
    });

    client.on('state', function (st) {
        wsStateLabel = st || 'connecting';
        renderDataModeBadge();
    });

    client.on('room_state', function (rs) {
        if (rs && Number(rs.room_id) > 0) {
            currentRoomId = Number(rs.room_id);
        }
    });

    client.on('snapshot', function (snap) {
        if (snap && Number(snap.room_id) > 0) {
            currentRoomId = Number(snap.room_id);
        }
        if (snap && Array.isArray(snap.items)) {
            for (let i = 0; i < snap.items.length; i++) {
                learnGuardFromEvent(snap.items[i]);
            }
        }
        scheduleRender();
    });

    client.on('event', function (item, meta) {
        if (!item || (meta && meta.isSnapshot)) return;
        learnGuardFromEvent(item);
        if (item.kind === 'chat') {
            handleIncomingChatEvent(item, { bypassCooldown: false });
        }
    });

    // -------------------------------------------------------------------------
    // 9. Tick Timers (1Hz Clock/Focus/PosCard + Holding Label Carousel)
    // -------------------------------------------------------------------------
    setInterval(function () {
        const now = Date.now();
        const fTick = focusScheduler.tick(now, getWatchlist(), cfg);
        posCardQueue.tick(now, cfg);
        if (fTick.changed) {
            refreshWatchUniverse();
            scheduleRuntimeSync();
        }
        renderClockAndSession();
        renderWatchlistGrid();
        renderFocusChartAndOverlays();
        renderPosCardOverlay();
    }, 500);

    let lastRotateTickAt = Date.now();
    setInterval(function () {
        const rotMs = Engine.clampNum(cfg.holding_rotate_sec, 1.5, 15, 3.5) * 1000;
        if (Date.now() - lastRotateTickAt >= rotMs) {
            lastRotateTickAt = Date.now();
            holdingRotateTick += 1;
            renderLeaderboard();
        }
    }, 500);

    window.addEventListener('resize', function () {
        scheduleRender();
    });

    Promise.all([loadConfigAndApply(), hydrateStorage()]).then(function () {
        scheduleRender();
        scheduleRuntimeSync();
    });
})();
