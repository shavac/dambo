(function () {
    'use strict';

    // =========================================================================
    // 1. 常量与默认配置回退
    // =========================================================================
    const MAX_KV_KEY_BYTES = 128;
    const PLUGIN_REFRESH_MS = 15000;
    const LEADERBOARD_PUBLISH_DEBOUNCE_MS = 350;
    const VALID_THEMES = ['glass-dark', 'cyber-neon', 'warm-gold', 'minimal-light', 'transparent-hud'];
    const VALID_LAYOUTS = ['list', 'podium', 'compact', 'ticker'];

    const FALLBACK_CONFIG = {
        title: '实时积分榜',
        subtitle: '多插件联动排名',
        empty_text: '等待积分推送…',
        score_prefix: '',
        score_unit: '分',
        score_decimals: 0,
        theme: 'glass-dark',
        layout: 'list',
        accent_color: '#38bdf8',
        gold_color: '#f59e0b',
        text_color: '#f8fafc',
        bg_opacity: 0.86,
        font_scale: 1,
        border_radius: 16,
        row_gap: 8,
        max_width: 0,
        show_header: true,
        show_footer: true,
        show_medals: true,
        show_avatar: true,
        show_progress_bar: true,
        show_delta: true,
        show_source_tag: true,
        show_empty_state: true,
        delta_ttl_ms: 2800,
        top_n: 8,
        sort_order: 'desc',
        tie_breaker: 'recent',
        rank_mode: 'sequential',
        min_score: 0,
        hide_zero_scores: true,
        auto_paginate: false,
        page_interval_sec: 8,
        storage_mode: 'persist',
        auto_discover_topics: true,
        auto_topic_names: ['scoreboard', 'score', 'scores'],
        sources: [
            {
                id: 'game24_round',
                enabled: true,
                plugin: 'game24',
                mode: 'topic',
                target: 'round',
                weight: 10,
                label: '24点',
                score_field: 'score',
                name_field: 'uname',
            },
        ],
        publish_leaderboard_topic: false,
    };

    // =========================================================================
    // 2. DOM 引用
    // =========================================================================
    const appShellEl = document.getElementById('app-shell');
    const boardEl = document.getElementById('board');
    const headerEl = document.getElementById('board-header');
    const titleEl = document.getElementById('board-title');
    const subtitleEl = document.getElementById('board-subtitle');
    const pagePillEl = document.getElementById('page-pill');
    const sourcePillEl = document.getElementById('source-pill');
    const podiumStageEl = document.getElementById('podium-stage');
    const rankListEl = document.getElementById('rank-list');
    const emptyStateEl = document.getElementById('empty-state');
    const emptyTextEl = document.getElementById('empty-text');
    const footerEl = document.getElementById('board-footer');
    const footerStatsEl = document.getElementById('footer-stats');
    const footerStoreEl = document.getElementById('footer-store');

    if (Dambo.utils.getQueryBool('safe_top', true) === false) {
        appShellEl.classList.add('no-safe-top');
    }

    // =========================================================================
    // 3. 运行时状态
    // =========================================================================
    let cfg = Object.assign({}, FALLBACK_CONFIG);
    let installedPlugins = [];
    let currentStoreMode = '';
    let activeStore = Dambo.storage;

    /** @type {Map<string, any>} */
    const selfScoresMap = new Map();
    /** @type {Map<string, any>} */
    const topicEventsMap = new Map();
    /** @type {Record<string, Record<string, { rawScore: number, updatedAt: number }>>} */
    let kvBaselines = Object.create(null);

    /** @type {Map<string, { spec: any, entries: Map<string, any>, off: Function | null, errorCode: string | null }>} */
    const externalKvMap = new Map();
    /** @type {Map<string, { off: Function, rule: any, sourceId: string }>} */
    const activeTopicSubs = new Map();

    const timers = Dambo.utils.createTimerBag();
    const pendingEventWrites = new Map();
    const pendingKvBaselineCleanups = new Set();

    let hasHydrated = false;
    /** @type {Map<string, number>} */
    const prevDisplayScores = new Map();
    /** @type {Map<string, { diff: number, expiresAt: number }>} */
    const activeDeltas = new Map();

    let currentPage = 0;
    let totalPages = 1;
    let pageTimerId = null;
    let publishDebounceId = null;
    let lastPublishedSig = '';
    let offStoreScores = null;
    let offStoreEvents = null;
    let offStoreMeta = null;

    // =========================================================================
    // 4. 通用安全与工具函数
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
            s = (prefixFallback || 'k') + '_' + fnv1aHex(String(raw || 'empty'));
        }
        if (utf8ByteLength(s) <= MAX_KV_KEY_BYTES) {
            return s;
        }
        const suffix = '_' + fnv1aHex(s);
        let cut = s.slice(0, 64);
        while (cut.length > 1 && utf8ByteLength(cut + suffix) > MAX_KV_KEY_BYTES) {
            cut = cut.slice(0, -1);
        }
        return cut + suffix;
    }

    function parseHexRgb(hex, fallbackHex) {
        const validate = function (v) {
            if (typeof v !== 'string') return null;
            const trimmed = v.trim();
            if (/^#[0-9a-fA-F]{6}$/.test(trimmed)) {
                return {
                    hex: trimmed,
                    rgb: [
                        parseInt(trimmed.slice(1, 3), 16),
                        parseInt(trimmed.slice(3, 5), 16),
                        parseInt(trimmed.slice(5, 7), 16),
                    ].join(', '),
                };
            }
            if (/^#[0-9a-fA-F]{3}$/.test(trimmed)) {
                return {
                    hex: trimmed,
                    rgb: [
                        parseInt(trimmed[1] + trimmed[1], 16),
                        parseInt(trimmed[2] + trimmed[2], 16),
                        parseInt(trimmed[3] + trimmed[3], 16),
                    ].join(', '),
                };
            }
            return null;
        };
        return validate(hex) || validate(fallbackHex);
    }

    function clampNum(val, min, max, fallback) {
        const n = Number(val);
        if (!Number.isFinite(n)) return fallback;
        return Math.min(max, Math.max(min, n));
    }

    function roundClean(num) {
        if (!Number.isFinite(num)) return 0;
        return Math.round(num * 1000000) / 1000000;
    }

    function safeAvatarFace(rawFace) {
        if (typeof rawFace !== 'string') return '';
        const normalized = Dambo.utils.normalizeUrl(rawFace.trim());
        if (!normalized) return '';
        if (/^https?:\/\//i.test(normalized)) {
            return normalized;
        }
        return '';
    }

    function formatScoreValue(score) {
        const decimals = Math.round(clampNum(cfg.score_decimals, 0, 4, 0));
        const fixed = Number(score || 0).toFixed(decimals);
        const body = decimals === 0 ? fixed : fixed.replace(/\.?0+$/, '');
        return String(cfg.score_prefix || '') + body + String(cfg.score_unit || '');
    }

    function formatDeltaBadge(diff) {
        const decimals = Math.round(clampNum(cfg.score_decimals, 0, 4, 0));
        const fixed = Math.abs(diff).toFixed(decimals);
        const body = decimals === 0 ? fixed : fixed.replace(/\.?0+$/, '');
        return (diff > 0 ? '+' : '-') + body;
    }

    function pickFirstNumber(candidates) {
        for (let i = 0; i < candidates.length; i++) {
            const v = candidates[i];
            if (v !== undefined && v !== null && v !== '') {
                const n = Number(v);
                if (Number.isFinite(n)) return n;
            }
        }
        return null;
    }

    function pickFirstText(candidates) {
        for (let i = 0; i < candidates.length; i++) {
            const v = candidates[i];
            if (typeof v === 'string' && v.trim()) {
                return v.trim();
            }
        }
        return '';
    }

    function parseCanonicalIdentity(rawObj, fallbackKey, nameField) {
        const obj = rawObj && typeof rawObj === 'object' ? rawObj : {};
        const uidNum = Number(obj.uid);
        const uid = Number.isFinite(uidNum) && uidNum > 0 ? Math.floor(uidNum) : 0;
        const unameCandidates = [];
        if (nameField && typeof obj[nameField] === 'string') {
            unameCandidates.push(obj[nameField]);
        }
        unameCandidates.push(obj.uname, obj.name, obj.username, obj.nickname, obj.player);
        let uname = pickFirstText(unameCandidates);
        if (!uname && typeof fallbackKey === 'string') {
            if (fallbackKey.indexOf('name:') === 0) {
                uname = fallbackKey.slice(5);
            } else if (fallbackKey.indexOf('uid:') === 0) {
                uname = 'UID ' + fallbackKey.slice(4);
            } else {
                uname = fallbackKey;
            }
        }
        if (!uname) uname = uid > 0 ? 'UID ' + uid : '匿名玩家';

        let playerKey = '';
        if (typeof obj.player_key === 'string' && obj.player_key.trim()) {
            playerKey = sanitizeKvKey(obj.player_key.trim(), 'pk');
        } else if (uid > 0) {
            playerKey = 'uid:' + uid;
        } else {
            playerKey = sanitizeKvKey('name:' + uname, 'name');
        }

        const face = safeAvatarFace(obj.face || obj.avatar || '');
        return {
            playerKey: playerKey,
            uid: uid,
            uname: uname,
            face: face,
        };
    }

    // =========================================================================
    // 5. 配置解析、URL Query 覆写与主题样式应用
    // =========================================================================
    function applyQueryOverrides(raw) {
        const next = Object.assign({}, FALLBACK_CONFIG, raw || {});

        const qTitle = Dambo.utils.getQueryParam('title');
        if (qTitle !== null) next.title = qTitle;

        const qSub = Dambo.utils.getQueryParam('subtitle');
        if (qSub !== null) next.subtitle = qSub;

        const qTheme = Dambo.utils.getQueryParam('theme');
        if (qTheme && VALID_THEMES.indexOf(qTheme) !== -1) next.theme = qTheme;

        const qLayout = Dambo.utils.getQueryParam('layout');
        if (qLayout && VALID_LAYOUTS.indexOf(qLayout) !== -1) next.layout = qLayout;

        const qAccent = Dambo.utils.getQueryParam('accent');
        if (qAccent && parseHexRgb(qAccent, null)) next.accent_color = qAccent;

        const qTopN = Dambo.utils.getQueryParam('top_n');
        if (qTopN !== null) next.top_n = clampNum(qTopN, 1, 50, next.top_n);

        const qOpacity = Dambo.utils.getQueryParam('bg_opacity');
        if (qOpacity !== null) next.bg_opacity = clampNum(qOpacity, 0, 1, next.bg_opacity);

        const qScale = Dambo.utils.getQueryParam('font_scale');
        if (qScale !== null) next.font_scale = clampNum(qScale, 0.7, 1.6, next.font_scale);

        const qStore = Dambo.utils.getQueryParam('storage_mode');
        if (qStore === 'persist' || qStore === 'memory') next.storage_mode = qStore;

        const qSort = Dambo.utils.getQueryParam('sort_order');
        if (qSort === 'desc' || qSort === 'asc') next.sort_order = qSort;

        const qHeader = Dambo.utils.getQueryParam('show_header');
        if (qHeader !== null) next.show_header = Dambo.utils.getQueryBool('show_header', next.show_header);

        const qFooter = Dambo.utils.getQueryParam('show_footer');
        if (qFooter !== null) next.show_footer = Dambo.utils.getQueryBool('show_footer', next.show_footer);

        const qAvatar = Dambo.utils.getQueryParam('show_avatar');
        if (qAvatar !== null) next.show_avatar = Dambo.utils.getQueryBool('show_avatar', next.show_avatar);

        const qEmpty = Dambo.utils.getQueryParam('show_empty');
        if (qEmpty !== null) next.show_empty_state = Dambo.utils.getQueryBool('show_empty', next.show_empty_state);

        if (!Array.isArray(next.auto_topic_names)) {
            next.auto_topic_names = FALLBACK_CONFIG.auto_topic_names.slice();
        }
        if (!Array.isArray(next.sources)) {
            next.sources = FALLBACK_CONFIG.sources.slice();
        }
        return next;
    }

    function applyVisualStyles() {
        const theme = VALID_THEMES.indexOf(cfg.theme) !== -1 ? cfg.theme : 'glass-dark';
        const layout = VALID_LAYOUTS.indexOf(cfg.layout) !== -1 ? cfg.layout : 'list';
        boardEl.className = 'board theme-' + theme + ' layout-' + layout;

        const accent = parseHexRgb(cfg.accent_color, '#38bdf8');
        const gold = parseHexRgb(cfg.gold_color, '#f59e0b');
        const textC = parseHexRgb(cfg.text_color, '#f8fafc');

        boardEl.style.setProperty('--sb-accent', accent.hex);
        boardEl.style.setProperty('--sb-accent-rgb', accent.rgb);
        boardEl.style.setProperty('--sb-gold', gold.hex);
        boardEl.style.setProperty('--sb-gold-rgb', gold.rgb);
        boardEl.style.setProperty('--sb-text', textC.hex);
        boardEl.style.setProperty('--sb-text-rgb', textC.rgb);
        boardEl.style.setProperty('--sb-bg-opacity', String(clampNum(cfg.bg_opacity, 0, 1, 0.86)));
        boardEl.style.setProperty('--sb-font-scale', String(clampNum(cfg.font_scale, 0.7, 1.6, 1)));
        boardEl.style.setProperty('--sb-radius', String(clampNum(cfg.border_radius, 0, 28, 16)) + 'px');
        boardEl.style.setProperty('--sb-gap', String(clampNum(cfg.row_gap, 2, 20, 8)) + 'px');

        const maxW = clampNum(cfg.max_width, 0, 1920, 0);
        boardEl.style.maxWidth = maxW > 0 ? (maxW + 'px') : '';

        titleEl.textContent = cfg.title || '';
        subtitleEl.textContent = cfg.subtitle || '';
        subtitleEl.classList.toggle('hidden', !String(cfg.subtitle || '').trim());
        headerEl.classList.toggle('hidden', !cfg.show_header);
        footerEl.classList.toggle('hidden', !cfg.show_footer);
        emptyTextEl.textContent = cfg.empty_text || '等待积分推送…';
        footerStoreEl.textContent = cfg.storage_mode === 'memory' ? '内存态 (重启清零)' : '持久化 (重启保留)';
    }

    // =========================================================================
    // 6. 解析与汇总当前生效的数据源配置
    // =========================================================================
    function buildSourceCatalog() {
        /** @type {Array<any>} */
        const explicitSources = [];
        const disabledIds = new Set();
        const declaredTopicKeys = new Set();

        const rawSources = Array.isArray(cfg.sources) ? cfg.sources : [];
        for (let i = 0; i < rawSources.length; i++) {
            const item = rawSources[i];
            if (!item || typeof item !== 'object') continue;
            const plugin = String(item.plugin || '').trim();
            const mode = String(item.mode || 'topic').trim();
            const target = String(item.target || '').trim();
            if (!plugin || !target) continue;

            const sid = String(item.id || (mode + ':' + plugin + ':' + target)).trim();
            const rule = {
                id: sid,
                enabled: item.enabled !== false,
                plugin: plugin,
                mode: mode,
                target: target,
                weight: Number.isFinite(Number(item.weight)) ? Number(item.weight) : 1,
                label: String(item.label || '').trim(),
                score_field: String(item.score_field || 'score').trim() || 'score',
                name_field: String(item.name_field || 'uname').trim() || 'uname',
            };
            if (rule.enabled) {
                explicitSources.push(rule);
                if (rule.mode === 'topic') {
                    declaredTopicKeys.add(rule.plugin + '\0' + rule.target);
                }
            } else {
                disabledIds.add(sid);
            }
        }

        /** @type {Array<any>} */
        const autoTopicSources = [];
        if (cfg.auto_discover_topics) {
            const wantNames = new Set(
                (Array.isArray(cfg.auto_topic_names) ? cfg.auto_topic_names : [])
                    .map(function (s) { return String(s || '').trim(); })
                    .filter(Boolean)
            );
            for (let pIdx = 0; pIdx < installedPlugins.length; pIdx++) {
                const p = installedPlugins[pIdx];
                if (!p || p.plugin_name === Dambo.plugin.name || p.plugin_id === Dambo.plugin.name) {
                    continue;
                }
                const topics = Array.isArray(p.topics) ? p.topics : [];
                for (let tIdx = 0; tIdx < topics.length; tIdx++) {
                    const t = topics[tIdx];
                    if (!t || !t.public || !wantNames.has(t.name)) continue;
                    const byNameKey = p.plugin_name + '\0' + t.name;
                    const byIdKey = p.plugin_id + '\0' + t.name;
                    if (declaredTopicKeys.has(byNameKey) || declaredTopicKeys.has(byIdKey)) {
                        continue;
                    }
                    declaredTopicKeys.add(byNameKey);
                    autoTopicSources.push({
                        id: 'auto:' + p.plugin_name + ':' + t.name,
                        enabled: true,
                        plugin: p.plugin_name,
                        mode: 'topic',
                        target: t.name,
                        weight: 1,
                        label: p.name || p.plugin_name,
                        score_field: 'score',
                        name_field: 'uname',
                        isAuto: true,
                    });
                }
            }
        }

        const allActive = explicitSources.concat(autoTopicSources);
        /** @type {Map<string, any>} */
        const ruleById = new Map();
        for (let k = 0; k < allActive.length; k++) {
            ruleById.set(allActive[k].id, allActive[k]);
        }

        return {
            active: allActive,
            ruleById: ruleById,
            disabledIds: disabledIds,
        };
    }

    // =========================================================================
    // 7. 自身存储加载与监听（scores / topic_events / meta）
    // =========================================================================
    function applyMapMutation(map, ev) {
        if (!ev) return false;
        if (ev.op === 'clear') {
            map.clear();
            return true;
        }
        if (ev.op === 'delete') {
            const delKeys = Array.isArray(ev.keys) ? ev.keys : [];
            for (let i = 0; i < delKeys.length; i++) {
                map.delete(delKeys[i]);
            }
            return true;
        }
        if (ev.values && typeof ev.values === 'object') {
            const ks = Object.keys(ev.values);
            for (let j = 0; j < ks.length; j++) {
                map.set(ks[j], ev.values[ks[j]]);
            }
            return true;
        }
        return false;
    }

    function cancelPendingWritesForSource(sourceId) {
        const prefix = sanitizeKvKey('evt_' + sourceId + '_', 'evt');
        Array.from(pendingEventWrites.entries()).forEach(function (pair) {
            const k = pair[0];
            const timerId = pair[1];
            if (k.indexOf(prefix) === 0 || (sourceId && k.indexOf(sourceId) !== -1)) {
                timers.clear(timerId);
                pendingEventWrites.delete(k);
            }
        });
    }

    function cancelAllPendingEventWrites() {
        Array.from(pendingEventWrites.values()).forEach(function (timerId) {
            timers.clear(timerId);
        });
        pendingEventWrites.clear();
    }

    async function loadSelfNamespaces() {
        const store = activeStore;
        const results = await Promise.all([
            store.list('scores').catch(function () { return {}; }),
            store.list('topic_events').catch(function () { return {}; }),
            store.get('meta', 'kv_baselines').catch(function () { return null; }),
        ]);

        if (activeStore !== store) return;

        selfScoresMap.clear();
        const scObj = results[0] || {};
        Object.keys(scObj).forEach(function (k) {
            selfScoresMap.set(k, scObj[k]);
        });

        topicEventsMap.clear();
        const evObj = results[1] || {};
        Object.keys(evObj).forEach(function (k) {
            topicEventsMap.set(k, evObj[k]);
        });

        kvBaselines = results[2] && typeof results[2] === 'object' ? results[2] : Object.create(null);
        hasHydrated = false;
        renderLeaderboard();
        hasHydrated = true;
    }

    function bindSelfStoreWatches() {
        if (offStoreScores) {
            offStoreScores();
            offStoreScores = null;
        }
        if (offStoreEvents) {
            offStoreEvents();
            offStoreEvents = null;
        }
        if (offStoreMeta) {
            offStoreMeta();
            offStoreMeta = null;
        }

        const boundStore = activeStore;
        offStoreScores = boundStore.watch('scores', function (ev) {
            if (activeStore !== boundStore) return;
            if (!applyMapMutation(selfScoresMap, ev)) {
                boundStore.list('scores').then(function (all) {
                    if (activeStore !== boundStore) return;
                    selfScoresMap.clear();
                    Object.keys(all || {}).forEach(function (k) { selfScoresMap.set(k, all[k]); });
                    renderLeaderboard();
                }).catch(function () {});
                return;
            }
            renderLeaderboard();
        });

        offStoreEvents = boundStore.watch('topic_events', function (ev) {
            if (activeStore !== boundStore) return;
            if (ev && ev.op === 'clear') {
                cancelAllPendingEventWrites();
            } else if (ev && Array.isArray(ev.keys)) {
                // 其他标签页或自身已完成写入时取消待执行的幂等补写，避免重复 PUT
                for (let i = 0; i < ev.keys.length; i++) {
                    const k = ev.keys[i];
                    if (pendingEventWrites.has(k)) {
                        timers.clear(pendingEventWrites.get(k));
                        pendingEventWrites.delete(k);
                    }
                }
            }
            if (!applyMapMutation(topicEventsMap, ev)) {
                boundStore.list('topic_events').then(function (all) {
                    if (activeStore !== boundStore) return;
                    topicEventsMap.clear();
                    Object.keys(all || {}).forEach(function (k) { topicEventsMap.set(k, all[k]); });
                    renderLeaderboard();
                }).catch(function () {});
                return;
            }
            renderLeaderboard();
        });

        offStoreMeta = boundStore.watch('meta', function (ev) {
            if (activeStore !== boundStore) return;
            if (ev && ev.op === 'clear') {
                kvBaselines = Object.create(null);
                renderLeaderboard();
                return;
            }
            if (ev && ev.op === 'delete' && Array.isArray(ev.keys) && ev.keys.indexOf('kv_baselines') !== -1) {
                kvBaselines = Object.create(null);
                renderLeaderboard();
                return;
            }
            if (ev && ev.values && Object.prototype.hasOwnProperty.call(ev.values, 'kv_baselines')) {
                const nextBase = ev.values.kv_baselines;
                kvBaselines = nextBase && typeof nextBase === 'object' ? nextBase : Object.create(null);
                renderLeaderboard();
                return;
            }
            boundStore.get('meta', 'kv_baselines').then(function (val) {
                if (activeStore !== boundStore) return;
                kvBaselines = val && typeof val === 'object' ? val : Object.create(null);
                renderLeaderboard();
            }).catch(function () {});
        });
    }

    async function ensureStoreModeSynced() {
        const mode = cfg.storage_mode === 'memory' ? 'memory' : 'persist';
        if (mode === currentStoreMode) return;
        cancelAllPendingEventWrites();
        currentStoreMode = mode;
        activeStore = mode === 'memory' ? Dambo.memory : Dambo.storage;
        bindSelfStoreWatches();
        await loadSelfNamespaces();
    }

    // =========================================================================
    // 8. 外部 Topic 消费与幂等持久化引擎
    // =========================================================================
    function scheduleIdempotentEventWrite(evtKey, record) {
        topicEventsMap.set(evtKey, record);
        if (pendingEventWrites.has(evtKey)) {
            timers.clear(pendingEventWrites.get(evtKey));
            pendingEventWrites.delete(evtKey);
        }
        const targetStore = activeStore;
        const delayMs = 25 + Math.floor(Math.random() * 65);
        const tid = timers.set(function () {
            pendingEventWrites.delete(evtKey);
            if (activeStore !== targetStore) return;
            targetStore.set('topic_events', evtKey, record).catch(function () {});
        }, delayMs);
        pendingEventWrites.set(evtKey, tid);
    }

    function normalizeSingleTopicRecord(rawItem, payloadRoot, rule, msg) {
        if (!rawItem || typeof rawItem !== 'object') return null;

        const actionRaw = String(rawItem.action || payloadRoot.action || '').trim().toLowerCase();
        const playerObj = rawItem.player && typeof rawItem.player === 'object' ? rawItem.player : rawItem;
        const id = parseCanonicalIdentity(playerObj, '', rule.name_field);
        const tag = pickFirstText([rawItem.tag, playerObj.tag, payloadRoot.tag, rule.label, rule.plugin]);

        if (actionRaw === 'delete' || actionRaw === 'remove') {
            return { op: 'delete_player', identity: id };
        }

        const customFieldVal = rule.score_field ? rawItem[rule.score_field] : undefined;
        let numericVal = null;
        let op = 'add_delta';

        if (actionRaw === 'set') {
            op = 'set_player';
            numericVal = pickFirstNumber([customFieldVal, rawItem.score, rawItem.value, rawItem.points, rawItem.delta]);
        } else if (actionRaw === 'sub') {
            op = 'add_delta';
            const absVal = pickFirstNumber([rawItem.delta, rawItem.sub, customFieldVal, rawItem.points, rawItem.value, rawItem.score]);
            numericVal = absVal === null ? null : -Math.abs(absVal);
        } else if (rawItem.delta !== undefined || rawItem.add !== undefined) {
            op = 'add_delta';
            numericVal = pickFirstNumber([rawItem.delta, rawItem.add]);
        } else if (actionRaw === 'add' || String(rawItem.mode || '').toLowerCase() === 'add') {
            op = 'add_delta';
            numericVal = pickFirstNumber([customFieldVal, rawItem.points, rawItem.value, rawItem.score, 1]);
        } else if (rule.score_field && rule.score_field !== 'score' && customFieldVal !== undefined) {
            op = 'add_delta';
            numericVal = pickFirstNumber([customFieldVal]);
        } else if (rawItem.points !== undefined) {
            op = 'add_delta';
            numericVal = pickFirstNumber([rawItem.points]);
        } else if (rawItem.score !== undefined || rawItem.value !== undefined) {
            op = 'set_player';
            numericVal = pickFirstNumber([rawItem.score, rawItem.value]);
        }

        if (numericVal === null || !Number.isFinite(numericVal)) {
            return null;
        }

        return {
            op: op,
            identity: id,
            value: numericVal,
            tag: tag,
            eventId: pickFirstText([
                rawItem.event_id !== undefined ? String(rawItem.event_id) : '',
                rawItem.id !== undefined ? String(rawItem.id) : '',
                payloadRoot.event_id !== undefined ? String(payloadRoot.event_id) : '',
                String(msg.ts || Date.now()),
            ]),
            ts: Number(rawItem.ts || rawItem.updated_at || msg.ts || Date.now()),
        };
    }

    function normalizeTopicOps(msg, rule) {
        const payload = msg && msg.payload;
        if (payload === undefined || payload === null) return [];

        // 1. 兼容内置 game24 公开 topic `round`
        if (rule.target === 'round' && typeof payload === 'object' && typeof payload.event === 'string') {
            if (payload.event !== 'solved' || !payload.winner) {
                return [];
            }
            const winnerObj = typeof payload.winner === 'string'
                ? { uname: payload.winner }
                : payload.winner;
            const id = parseCanonicalIdentity(winnerObj, '', rule.name_field);
            const rawPoints = pickFirstNumber([
                rule.score_field ? payload[rule.score_field] : undefined,
                rule.score_field && winnerObj ? winnerObj[rule.score_field] : undefined,
                1,
            ]);
            return [
                {
                    op: 'add_delta',
                    identity: id,
                    value: rawPoints,
                    tag: rule.label || '24点',
                    eventId: 'round_' + String(payload.round !== undefined ? payload.round : 0) + '_' + String(msg.ts || Date.now()),
                    ts: Number(msg.ts || Date.now()),
                },
            ];
        }

        // 2. 数组批量事件
        if (Array.isArray(payload)) {
            const out = [];
            for (let i = 0; i < payload.length; i++) {
                const norm = normalizeSingleTopicRecord(payload[i], {}, rule, msg);
                if (norm) {
                    norm.eventId = norm.eventId + '#' + i;
                    out.push(norm);
                }
            }
            return out;
        }

        if (typeof payload !== 'object') return [];

        const action = String(payload.action || '').trim().toLowerCase();
        if (action === 'reset' || action === 'clear') {
            return [{ op: 'reset_source' }];
        }

        if (action === 'snapshot' || action === 'replace') {
            return [
                {
                    op: 'snapshot',
                    items: payload.items || {},
                    label: pickFirstText([payload.label, rule.label, rule.plugin]),
                    ts: Number(payload.ts || msg.ts || Date.now()),
                },
            ];
        }

        if (action === 'batch' || (Array.isArray(payload.items) && !action)) {
            const list = Array.isArray(payload.items) ? payload.items : [];
            const batchOut = [];
            for (let j = 0; j < list.length; j++) {
                const itemNorm = normalizeSingleTopicRecord(list[j], payload, rule, msg);
                if (itemNorm) {
                    itemNorm.eventId = itemNorm.eventId + '#' + j;
                    batchOut.push(itemNorm);
                }
            }
            return batchOut;
        }

        const single = normalizeSingleTopicRecord(payload, payload, rule, msg);
        return single ? [single] : [];
    }

    async function clearSourceData(sourceId) {
        cancelPendingWritesForSource(sourceId);
        const store = activeStore;

        const scoreKeysToDelete = [];
        const snapKey = sanitizeKvKey('snap:' + sourceId, 'snap');
        selfScoresMap.forEach(function (val, k) {
            if (k === snapKey || (val && typeof val === 'object' && val.source_id === sourceId)) {
                scoreKeysToDelete.push(k);
            }
        });
        for (let i = 0; i < scoreKeysToDelete.length; i++) {
            selfScoresMap.delete(scoreKeysToDelete[i]);
            store.delete('scores', scoreKeysToDelete[i]).catch(function () {});
        }

        const evtKeysToDelete = [];
        topicEventsMap.forEach(function (val, k) {
            if (val && typeof val === 'object' && val.source_id === sourceId) {
                evtKeysToDelete.push(k);
            }
        });
        for (let j = 0; j < evtKeysToDelete.length; j++) {
            topicEventsMap.delete(evtKeysToDelete[j]);
            store.delete('topic_events', evtKeysToDelete[j]).catch(function () {});
        }
        renderLeaderboard();
    }

    function deletePlayerInSource(sourceId, playerKey) {
        const store = activeStore;
        const srcKey = sanitizeKvKey('src:' + sourceId + ':' + playerKey, 'src');
        if (selfScoresMap.has(srcKey)) {
            selfScoresMap.delete(srcKey);
            store.delete('scores', srcKey).catch(function () {});
        }
        const delEvts = [];
        topicEventsMap.forEach(function (val, k) {
            if (val && val.source_id === sourceId && val.player_key === playerKey) {
                delEvts.push(k);
            }
        });
        for (let i = 0; i < delEvts.length; i++) {
            topicEventsMap.delete(delEvts[i]);
            store.delete('topic_events', delEvts[i]).catch(function () {});
        }
    }

    function handleIncomingTopicMessage(msg, rule) {
        if (!rule) return;
        const ops = normalizeTopicOps(msg, rule);
        if (ops.length === 0) return;

        let changed = false;
        for (let i = 0; i < ops.length; i++) {
            const op = ops[i];
            if (op.op === 'reset_source') {
                clearSourceData(rule.id);
                return;
            }
            if (op.op === 'delete_player') {
                deletePlayerInSource(rule.id, op.identity.playerKey);
                changed = true;
                continue;
            }
            if (op.op === 'snapshot') {
                const snapKey = sanitizeKvKey('snap:' + rule.id, 'snap');
                const snapVal = {
                    source_id: rule.id,
                    label: op.label || rule.label || rule.plugin,
                    items: op.items,
                    updated_at: op.ts,
                };
                selfScoresMap.set(snapKey, snapVal);
                activeStore.set('scores', snapKey, snapVal).catch(function () {});
                changed = true;
                continue;
            }
            if (op.op === 'set_player') {
                const srcKey = sanitizeKvKey('src:' + rule.id + ':' + op.identity.playerKey, 'src');
                const srcVal = {
                    source_id: rule.id,
                    player_key: op.identity.playerKey,
                    uid: op.identity.uid,
                    uname: op.identity.uname,
                    face: op.identity.face,
                    score: op.value,
                    tag: op.tag,
                    updated_at: op.ts,
                };
                selfScoresMap.set(srcKey, srcVal);
                activeStore.set('scores', srcKey, srcVal).catch(function () {});
                changed = true;
                continue;
            }
            if (op.op === 'add_delta') {
                const evtKey = sanitizeKvKey(
                    'evt_' + rule.id + '_' + op.eventId + '_' + op.identity.playerKey,
                    'evt'
                );
                const evtRecord = {
                    source_id: rule.id,
                    player_key: op.identity.playerKey,
                    uid: op.identity.uid,
                    uname: op.identity.uname,
                    face: op.identity.face,
                    delta: op.value,
                    tag: op.tag,
                    updated_at: op.ts,
                };
                scheduleIdempotentEventWrite(evtKey, evtRecord);
                changed = true;
            }
        }

        if (changed) {
            renderLeaderboard();
        }
    }

    function syncTopicSubscriptions(topicRules) {
        const desired = new Map();
        for (let i = 0; i < topicRules.length; i++) {
            const rule = topicRules[i];
            const subKey = rule.plugin + '\0' + rule.target;
            desired.set(subKey, rule);
        }

        Array.from(activeTopicSubs.keys()).forEach(function (k) {
            if (!desired.has(k)) {
                const entry = activeTopicSubs.get(k);
                if (entry && typeof entry.off === 'function') entry.off();
                activeTopicSubs.delete(k);
            }
        });

        desired.forEach(function (rule, subKey) {
            if (activeTopicSubs.has(subKey)) {
                activeTopicSubs.get(subKey).rule = rule;
                return;
            }
            const off = Dambo.topics.subscribe(rule.plugin, rule.target, function (msg) {
                const current = activeTopicSubs.get(subKey);
                handleIncomingTopicMessage(msg, current ? current.rule : rule);
            });
            activeTopicSubs.set(subKey, {
                off: off,
                rule: rule,
                sourceId: rule.id,
            });
        });
    }

    // =========================================================================
    // 9. 外部 KV 存储源读取与实时监听（memory_kv / persist_kv）
    // =========================================================================
    function syncKvSources(kvRules) {
        const desired = new Map();
        for (let i = 0; i < kvRules.length; i++) {
            const r = kvRules[i];
            const kvKey = r.id;
            desired.set(kvKey, r);
        }

        Array.from(externalKvMap.keys()).forEach(function (k) {
            if (!desired.has(k)) {
                const old = externalKvMap.get(k);
                if (old && typeof old.off === 'function') old.off();
                externalKvMap.delete(k);
            }
        });

        desired.forEach(function (rule, kvKey) {
            const existing = externalKvMap.get(kvKey);
            const needsRebind = !existing ||
                existing.spec.mode !== rule.mode ||
                existing.spec.plugin !== rule.plugin ||
                existing.spec.target !== rule.target;

            if (!needsRebind) {
                existing.spec = rule;
                return;
            }

            if (existing && typeof existing.off === 'function') {
                existing.off();
            }

            const handle = rule.mode === 'persist_kv'
                ? Dambo.storage.of(rule.plugin)
                : Dambo.memory.of(rule.plugin);

            const rec = {
                spec: rule,
                entries: new Map(),
                off: null,
                errorCode: null,
            };
            externalKvMap.set(kvKey, rec);

            const refreshList = function () {
                handle.list(rule.target).then(function (obj) {
                    if (externalKvMap.get(kvKey) !== rec) return;
                    rec.errorCode = null;
                    rec.entries.clear();
                    Object.keys(obj || {}).forEach(function (k) {
                        rec.entries.set(k, obj[k]);
                    });
                    renderLeaderboard();
                }).catch(function (err) {
                    if (externalKvMap.get(kvKey) !== rec) return;
                    rec.errorCode = (err && err.code) || 'ERROR';
                    rec.entries.clear();
                    renderLeaderboard();
                });
            };

            refreshList();
            rec.off = handle.watch(rule.target, function (ev) {
                if (externalKvMap.get(kvKey) !== rec) return;
                rec.errorCode = null;
                if (!applyMapMutation(rec.entries, ev)) {
                    refreshList();
                    return;
                }
                renderLeaderboard();
            });
        });
    }

    function refreshExternalKvAfterAclChange() {
        externalKvMap.forEach(function (rec) {
            if (rec.errorCode === 'ACL_DENIED') {
                const r = rec.spec;
                const handle = r.mode === 'persist_kv'
                    ? Dambo.storage.of(r.plugin)
                    : Dambo.memory.of(r.plugin);
                handle.list(r.target).then(function (obj) {
                    rec.errorCode = null;
                    rec.entries.clear();
                    Object.keys(obj || {}).forEach(function (k) {
                        rec.entries.set(k, obj[k]);
                    });
                    renderLeaderboard();
                }).catch(function () {});
            }
        });
    }

    // =========================================================================
    // 10. 积分聚合、排名计算与增量 Diff 动效
    // =========================================================================
    function upsertPlayer(playersMap, idInfo, weightedScore, tagLabel, updatedAt) {
        if (!idInfo || !idInfo.playerKey) return;
        let row = playersMap.get(idInfo.playerKey);
        if (!row) {
            row = {
                playerKey: idInfo.playerKey,
                uid: idInfo.uid,
                uname: idInfo.uname,
                face: idInfo.face,
                score: 0,
                tags: new Set(),
                updatedAt: 0,
            };
            playersMap.set(idInfo.playerKey, row);
        }
        if (idInfo.uid > 0 && row.uid <= 0) row.uid = idInfo.uid;
        if (idInfo.uname && (!row.uname || row.uname.indexOf('UID ') === 0 || (idInfo.uname.indexOf('UID ') !== 0 && updatedAt >= row.updatedAt))) {
            row.uname = idInfo.uname;
        }
        if (idInfo.face && (!row.face || updatedAt >= row.updatedAt)) {
            row.face = idInfo.face;
        }
        row.score = roundClean(row.score + weightedScore);
        if (tagLabel) row.tags.add(tagLabel);
        if (updatedAt > row.updatedAt) row.updatedAt = updatedAt;
    }

    function aggregatePlayers() {
        const catalog = buildSourceCatalog();
        /** @type {Map<string, any>} */
        const playersMap = new Map();

        // Layer 1: 自身 scores 命名空间（直接写入 / src:<id>:<pk> / snap:<id>）
        selfScoresMap.forEach(function (val, key) {
            if (val === undefined || val === null) return;

            if (key.indexOf('snap:') === 0 && val && typeof val === 'object') {
                const sid = String(val.source_id || key.slice(5)).trim();
                if (catalog.disabledIds.has(sid)) return;
                const rule = catalog.ruleById.get(sid);
                const weight = rule ? rule.weight : 1;
                const label = val.label || (rule && rule.label) || sid;
                const snapUpdated = Number(val.updated_at || 0);
                const items = val.items;
                if (Array.isArray(items)) {
                    for (let i = 0; i < items.length; i++) {
                        const it = items[i];
                        if (!it || typeof it !== 'object') continue;
                        const sc = pickFirstNumber([rule ? it[rule.score_field] : undefined, it.score, it.value]);
                        if (sc === null) continue;
                        const id = parseCanonicalIdentity(it, '', rule && rule.name_field);
                        upsertPlayer(playersMap, id, sc * weight, it.tag || label, Number(it.updated_at || snapUpdated));
                    }
                } else if (items && typeof items === 'object') {
                    Object.keys(items).forEach(function (subK) {
                        const subV = items[subK];
                        if (typeof subV === 'number') {
                            upsertPlayer(playersMap, parseCanonicalIdentity({}, subK), subV * weight, label, snapUpdated);
                        } else if (subV && typeof subV === 'object') {
                            const sc = pickFirstNumber([rule ? subV[rule.score_field] : undefined, subV.score, subV.value]);
                            if (sc === null) return;
                            upsertPlayer(
                                playersMap,
                                parseCanonicalIdentity(subV, subK, rule && rule.name_field),
                                sc * weight,
                                subV.tag || label,
                                Number(subV.updated_at || snapUpdated)
                            );
                        }
                    });
                }
                return;
            }

            if (key.indexOf('src:') === 0) {
                let sourceId = '';
                let fallbackPk = '';
                if (val && typeof val === 'object' && val.source_id) {
                    sourceId = String(val.source_id);
                    fallbackPk = String(val.player_key || '');
                } else {
                    const rest = key.slice(4);
                    const sep = rest.indexOf(':');
                    if (sep > 0) {
                        sourceId = rest.slice(0, sep);
                        fallbackPk = rest.slice(sep + 1);
                    }
                }
                if (sourceId && catalog.disabledIds.has(sourceId)) return;
                const rule = sourceId ? catalog.ruleById.get(sourceId) : null;
                const weight = rule ? rule.weight : 1;
                const rawScore = typeof val === 'number'
                    ? val
                    : pickFirstNumber([rule ? val[rule.score_field] : undefined, val.score, val.value]);
                if (rawScore === null) return;
                const id = parseCanonicalIdentity(typeof val === 'object' ? val : {}, fallbackPk, rule && rule.name_field);
                const label = (val && val.tag) || (rule && (rule.label || rule.plugin)) || sourceId || '外接';
                upsertPlayer(playersMap, id, rawScore * weight, label, Number((val && val.updated_at) || 0));
                return;
            }

            // 手动 / 直接写入的键
            if (typeof val === 'number') {
                upsertPlayer(playersMap, parseCanonicalIdentity({}, key), val, '手动', 0);
            } else if (val && typeof val === 'object') {
                const rawScore = pickFirstNumber([val.score, val.value]);
                if (rawScore === null) return;
                upsertPlayer(
                    playersMap,
                    parseCanonicalIdentity(val, key),
                    rawScore,
                    val.tag || '直接推送',
                    Number(val.updated_at || 0)
                );
            }
        });

        // Layer 2: Topic 增量事件流（topic_events）
        topicEventsMap.forEach(function (ev) {
            if (!ev || typeof ev !== 'object') return;
            const sid = String(ev.source_id || '').trim();
            if (sid && catalog.disabledIds.has(sid)) return;
            const rule = sid ? catalog.ruleById.get(sid) : null;
            const weight = rule ? rule.weight : 1;
            const delta = Number(ev.delta);
            if (!Number.isFinite(delta) || delta === 0) return;
            const id = parseCanonicalIdentity(ev, ev.player_key || '');
            const tag = ev.tag || (rule && (rule.label || rule.plugin)) || '';
            upsertPlayer(playersMap, id, delta * weight, tag, Number(ev.updated_at || 0));
        });

        // Layer 3: 只读外部 KV 存储监听源（带清零基线抵扣）
        let baselineStaleCleaned = false;
        externalKvMap.forEach(function (rec) {
            const rule = rec.spec;
            if (!rule || rule.enabled === false) return;
            const weight = Number.isFinite(rule.weight) ? rule.weight : 1;
            const baseBucket = kvBaselines[rule.id];

            rec.entries.forEach(function (entryVal, k) {
                let rawScore = null;
                let id = null;
                let updatedAt = 0;

                if (typeof entryVal === 'number') {
                    rawScore = entryVal;
                    id = parseCanonicalIdentity({}, k);
                } else if (entryVal && typeof entryVal === 'object') {
                    rawScore = pickFirstNumber([entryVal[rule.score_field], entryVal.score, entryVal.value]);
                    if (rawScore === null) return;
                    id = parseCanonicalIdentity(entryVal, k, rule.name_field);
                    updatedAt = Number(entryVal.updated_at || 0);
                } else {
                    return;
                }

                let effectiveRaw = rawScore;
                if (baseBucket && baseBucket[k] && Number.isFinite(Number(baseBucket[k].rawScore))) {
                    const base = Number(baseBucket[k].rawScore);
                    if (rawScore >= base) {
                        effectiveRaw = rawScore - base;
                        if (effectiveRaw === 0) return;
                    } else {
                        // 源插件已自行清零（如 game24 重开），自动移除旧基线
                        delete baseBucket[k];
                        baselineStaleCleaned = true;
                    }
                }

                upsertPlayer(
                    playersMap,
                    id,
                    effectiveRaw * weight,
                    rule.label || rule.plugin,
                    updatedAt
                );
            });
        });

        if (baselineStaleCleaned && !pendingKvBaselineCleanups.has(currentStoreMode)) {
            pendingKvBaselineCleanups.add(currentStoreMode);
            const saveStore = activeStore;
            timers.set(function () {
                pendingKvBaselineCleanups.delete(currentStoreMode);
                saveStore.set('meta', 'kv_baselines', kvBaselines).catch(function () {});
            }, 300);
        }

        // 过滤、排序与名次赋值
        const minScore = Number.isFinite(Number(cfg.min_score)) ? Number(cfg.min_score) : -Infinity;
        const hideZero = Boolean(cfg.hide_zero_scores);
        const list = [];
        playersMap.forEach(function (row) {
            if (row.score < minScore) return;
            if (hideZero && row.score <= 0) return;
            list.push(row);
        });

        const sortDir = cfg.sort_order === 'asc' ? 1 : -1;
        const tieBreaker = cfg.tie_breaker || 'recent';
        list.sort(function (a, b) {
            if (a.score !== b.score) {
                return (a.score - b.score) * sortDir;
            }
            if (tieBreaker === 'earliest') {
                if (a.updatedAt !== b.updatedAt) return a.updatedAt - b.updatedAt;
            } else if (tieBreaker === 'recent') {
                if (a.updatedAt !== b.updatedAt) return b.updatedAt - a.updatedAt;
            }
            return a.uname.localeCompare(b.uname, 'zh-Hans-CN');
        });

        const rankMode = cfg.rank_mode || 'sequential';
        let lastScore = null;
        let lastRank = 0;
        let distinctRank = 0;
        for (let i = 0; i < list.length; i++) {
            const item = list[i];
            if (rankMode === 'competition') {
                if (i === 0 || item.score !== lastScore) {
                    lastRank = i + 1;
                    lastScore = item.score;
                }
                item.rank = lastRank;
            } else if (rankMode === 'dense') {
                if (i === 0 || item.score !== lastScore) {
                    distinctRank += 1;
                    lastRank = distinctRank;
                    lastScore = item.score;
                }
                item.rank = lastRank;
            } else {
                item.rank = i + 1;
            }
        }

        return {
            ranked: list,
            activeSourceCount: catalog.active.length,
        };
    }

    function updateDeltaAnimations(ranked) {
        const now = Date.now();
        const ttlMs = clampNum(cfg.delta_ttl_ms, 800, 10000, 2800);

        if (hasHydrated) {
            let needExpiryCleanup = false;
            for (let i = 0; i < ranked.length; i++) {
                const row = ranked[i];
                const prev = prevDisplayScores.has(row.playerKey) ? prevDisplayScores.get(row.playerKey) : 0;
                const diff = roundClean(row.score - prev);
                if (diff !== 0) {
                    activeDeltas.set(row.playerKey, {
                        diff: diff,
                        expiresAt: now + ttlMs,
                    });
                    needExpiryCleanup = true;
                }
            }
            if (needExpiryCleanup) {
                timers.set(function () {
                    const checkNow = Date.now();
                    let pruned = false;
                    activeDeltas.forEach(function (val, k) {
                        if (val.expiresAt <= checkNow) {
                            activeDeltas.delete(k);
                            pruned = true;
                        }
                    });
                    if (pruned) renderLeaderboard();
                }, ttlMs + 40);
            }
        }

        prevDisplayScores.clear();
        for (let j = 0; j < ranked.length; j++) {
            prevDisplayScores.set(ranked[j].playerKey, ranked[j].score);
        }
    }

    // =========================================================================
    // 11. DOM 渲染（全程仅用安全 DOM 节点与 textContent）
    // =========================================================================
    function formatRankLabel(rank) {
        if (cfg.show_medals) {
            if (rank === 1) return '🥇';
            if (rank === 2) return '🥈';
            if (rank === 3) return '🥉';
        }
        return '#' + rank;
    }

    function renderPodium(topThree) {
        podiumStageEl.replaceChildren();
        if (topThree.length === 0) {
            podiumStageEl.classList.add('hidden');
            return;
        }
        podiumStageEl.classList.remove('hidden');
        // 视觉顺序：3 人时 [2, 1, 3]，2 人时 [1, 2]，1 人时 [1]
        const order = topThree.length >= 3
            ? [topThree[1], topThree[0], topThree[2]]
            : topThree.slice();

        for (let i = 0; i < order.length; i++) {
            const entry = order[i];
            const card = document.createElement('div');
            card.className = 'podium-card rank-' + Math.min(3, entry.rank);

            const rankEl = document.createElement('div');
            rankEl.className = 'podium-rank';
            rankEl.textContent = formatRankLabel(entry.rank);
            card.appendChild(rankEl);

            if (cfg.show_avatar) {
                const av = Dambo.utils.createAvatar(entry.face, 'podium-avatar');
                av.alt = entry.uname;
                card.appendChild(av);
            }

            const nameEl = document.createElement('div');
            nameEl.className = 'podium-name';
            nameEl.textContent = entry.uname;
            card.appendChild(nameEl);

            const scoreEl = document.createElement('div');
            scoreEl.className = 'podium-score';
            scoreEl.textContent = formatScoreValue(entry.score);
            card.appendChild(scoreEl);

            podiumStageEl.appendChild(card);
        }
    }

    function createRankRow(entry, progressPct) {
        const now = Date.now();
        const deltaInfo = activeDeltas.get(entry.playerKey);
        const hasActiveDelta = deltaInfo && deltaInfo.expiresAt > now;

        const li = document.createElement('li');
        li.className = 'rank-row' +
            (entry.rank <= 3 ? ' rank-' + entry.rank : '') +
            (hasActiveDelta ? ' is-bumped' : '');

        if (cfg.show_progress_bar && progressPct > 0) {
            const bar = document.createElement('div');
            bar.className = 'row-progress';
            bar.style.width = progressPct.toFixed(1) + '%';
            li.appendChild(bar);
        }

        const rankSpan = document.createElement('span');
        rankSpan.className = 'row-rank';
        rankSpan.textContent = formatRankLabel(entry.rank);
        li.appendChild(rankSpan);

        if (cfg.show_avatar) {
            const avatarImg = Dambo.utils.createAvatar(entry.face, 'row-avatar');
            avatarImg.alt = entry.uname;
            li.appendChild(avatarImg);
        }

        const info = document.createElement('div');
        info.className = 'row-info';

        const nameLine = document.createElement('div');
        nameLine.className = 'row-name-line';
        const nameEl = document.createElement('span');
        nameEl.className = 'row-name';
        nameEl.textContent = entry.uname;
        nameLine.appendChild(nameEl);
        info.appendChild(nameLine);

        if (cfg.show_source_tag && entry.tags && entry.tags.size > 0) {
            const tagsWrap = document.createElement('div');
            tagsWrap.className = 'row-tags';
            let count = 0;
            entry.tags.forEach(function (t) {
                if (!t || count >= 3) return;
                const tagSpan = document.createElement('span');
                tagSpan.className = 'row-tag';
                tagSpan.textContent = t;
                tagsWrap.appendChild(tagSpan);
                count += 1;
            });
            if (count > 0) info.appendChild(tagsWrap);
        }
        li.appendChild(info);

        const scoreWrap = document.createElement('div');
        scoreWrap.className = 'row-score-wrap';

        if (cfg.show_delta && hasActiveDelta) {
            const badge = document.createElement('span');
            badge.className = 'delta-badge ' + (deltaInfo.diff > 0 ? 'up' : 'down');
            badge.textContent = formatDeltaBadge(deltaInfo.diff);
            scoreWrap.appendChild(badge);
        }

        const scoreEl = document.createElement('span');
        scoreEl.className = 'row-score';
        scoreEl.textContent = formatScoreValue(entry.score);
        scoreWrap.appendChild(scoreEl);

        li.appendChild(scoreWrap);
        return li;
    }

    function scheduleLeaderboardBroadcast(ranked) {
        if (!cfg.publish_leaderboard_topic) return;
        const payload = {
            title: cfg.title,
            storage_mode: cfg.storage_mode,
            total: ranked.length,
            top: ranked.slice(0, 20).map(function (r) {
                return {
                    rank: r.rank,
                    uid: r.uid,
                    uname: r.uname,
                    score: r.score,
                    tags: Array.from(r.tags || []),
                };
            }),
        };
        const sig = JSON.stringify(payload);
        if (sig === lastPublishedSig) return;
        if (publishDebounceId !== null) clearTimeout(publishDebounceId);
        publishDebounceId = setTimeout(function () {
            publishDebounceId = null;
            lastPublishedSig = sig;
            Dambo.topics.publish('leaderboard', payload, {
                public: true,
                description: '通用积分榜实时排名快照',
            }).catch(function () {});
        }, LEADERBOARD_PUBLISH_DEBOUNCE_MS);
    }

    function syncPaginationTimer() {
        if (pageTimerId !== null) {
            clearInterval(pageTimerId);
            pageTimerId = null;
        }
        if (!cfg.auto_paginate || totalPages <= 1) {
            currentPage = 0;
            return;
        }
        const sec = clampNum(cfg.page_interval_sec, 3, 60, 8);
        pageTimerId = setInterval(function () {
            if (totalPages > 1) {
                currentPage = (currentPage + 1) % totalPages;
                renderLeaderboard();
            }
        }, sec * 1000);
    }

    function renderLeaderboard() {
        const result = aggregatePlayers();
        const ranked = result.ranked;
        updateDeltaAnimations(ranked);

        sourcePillEl.textContent = result.activeSourceCount + ' 源';
        footerStatsEl.textContent = '共 ' + ranked.length + ' 位上榜';

        if (ranked.length === 0) {
            totalPages = 1;
            currentPage = 0;
            pagePillEl.classList.add('hidden');
            podiumStageEl.replaceChildren();
            podiumStageEl.classList.add('hidden');
            rankListEl.replaceChildren();
            rankListEl.classList.add('hidden');
            if (cfg.show_empty_state) {
                boardEl.classList.remove('is-auto-hidden');
                emptyStateEl.classList.remove('hidden');
            } else {
                emptyStateEl.classList.add('hidden');
                boardEl.classList.add('is-auto-hidden');
            }
            scheduleLeaderboardBroadcast(ranked);
            return;
        }

        boardEl.classList.remove('is-auto-hidden');
        emptyStateEl.classList.add('hidden');
        rankListEl.classList.remove('hidden');

        const perPage = Math.round(clampNum(cfg.top_n, 1, 50, 8));
        const nextTotalPages = cfg.auto_paginate ? Math.max(1, Math.ceil(ranked.length / perPage)) : 1;
        const pageCountChanged = nextTotalPages !== totalPages;
        totalPages = nextTotalPages;
        if (currentPage >= totalPages) currentPage = 0;
        if (pageCountChanged) syncPaginationTimer();

        if (totalPages > 1) {
            pagePillEl.textContent = (currentPage + 1) + '/' + totalPages;
            pagePillEl.classList.remove('hidden');
        } else {
            pagePillEl.classList.add('hidden');
        }

        const start = currentPage * perPage;
        const visible = ranked.slice(start, start + perPage);

        let maxAbsScore = 0;
        for (let i = 0; i < ranked.length; i++) {
            const absVal = Math.abs(ranked[i].score);
            if (absVal > maxAbsScore) maxAbsScore = absVal;
        }

        let listSlice = visible;
        if (cfg.layout === 'podium' && currentPage === 0) {
            renderPodium(visible.slice(0, 3));
            listSlice = visible.slice(3);
        } else {
            podiumStageEl.replaceChildren();
            podiumStageEl.classList.add('hidden');
        }

        const frag = document.createDocumentFragment();
        for (let idx = 0; idx < listSlice.length; idx++) {
            const item = listSlice[idx];
            const pct = maxAbsScore > 0 && item.score > 0
                ? Math.min(100, (item.score / maxAbsScore) * 100)
                : 0;
            frag.appendChild(createRankRow(item, pct));
        }
        rankListEl.replaceChildren(frag);
        rankListEl.classList.toggle('hidden', listSlice.length === 0);

        scheduleLeaderboardBroadcast(ranked);
    }

    // =========================================================================
    // 12. 初始化与生命周期同步
    // =========================================================================
    async function syncSourcesAndPlugins() {
        try {
            installedPlugins = await Dambo.plugins.list();
        } catch (_) {
            installedPlugins = [];
        }
        const catalog = buildSourceCatalog();
        const topicRules = catalog.active.filter(function (r) { return r.mode === 'topic'; });
        const kvRules = catalog.active.filter(function (r) { return r.mode === 'memory_kv' || r.mode === 'persist_kv'; });
        syncTopicSubscriptions(topicRules);
        syncKvSources(kvRules);
        renderLeaderboard();
    }

    async function loadConfigAndApply() {
        try {
            const raw = await Dambo.config.get();
            cfg = applyQueryOverrides(raw);
        } catch (_) {
            cfg = applyQueryOverrides(FALLBACK_CONFIG);
        }
        applyVisualStyles();
        await ensureStoreModeSynced();
        syncPaginationTimer();
        await syncSourcesAndPlugins();
    }

    Dambo.config.watch(function () {
        loadConfigAndApply();
    });

    Dambo.acl.onChange(function () {
        refreshExternalKvAfterAclChange();
    });

    setInterval(function () {
        if (cfg.auto_discover_topics) {
            syncSourcesAndPlugins();
        }
    }, PLUGIN_REFRESH_MS);

    loadConfigAndApply();
})();
