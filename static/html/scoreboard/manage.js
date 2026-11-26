(function () {
    'use strict';

    const MAX_KV_KEY_BYTES = 128;

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
    const tabCountScoresEl = document.getElementById('tab-count-scores');
    const tabCountSourcesEl = document.getElementById('tab-count-sources');
    const tabCountConfigEl = document.getElementById('tab-count-config');
    const tabCountAclEl = document.getElementById('tab-count-acl');
    const tabCountTopicsEl = document.getElementById('tab-count-topics');

    const secScoresEl = document.getElementById('sec-scores');
    const secSourcesEl = document.getElementById('sec-sources');
    const secConfigEl = document.getElementById('sec-config');
    const secPushEl = document.getElementById('sec-push');
    const secDataEl = document.getElementById('sec-data');

    const scoresModeHintEl = document.getElementById('scores-mode-hint');
    const simPushBtnEl = document.getElementById('sim-push-btn');
    const toggleIoBtnEl = document.getElementById('toggle-io-btn');
    const resetAllScoresBtnEl = document.getElementById('reset-all-scores-btn');
    const ioPanelEl = document.getElementById('io-panel');
    const exportJsonBtnEl = document.getElementById('export-json-btn');
    const copyJsonBtnEl = document.getElementById('copy-json-btn');
    const importMergeBtnEl = document.getElementById('import-merge-btn');
    const importReplaceBtnEl = document.getElementById('import-replace-btn');
    const ioJsonTextareaEl = document.getElementById('io-json-textarea');
    const manualScoreFormEl = document.getElementById('manual-score-form');
    const mUidEl = document.getElementById('m-uid');
    const mUnameEl = document.getElementById('m-uname');
    const mOpEl = document.getElementById('m-op');
    const mValEl = document.getElementById('m-val');
    const mTagEl = document.getElementById('m-tag');
    const mFaceEl = document.getElementById('m-face');
    const manageRankListEl = document.getElementById('manage-rank-list');

    const presetGame24TopicBtnEl = document.getElementById('preset-game24-topic-btn');
    const presetGame24KvBtnEl = document.getElementById('preset-game24-kv-btn');
    const srcAutoDiscoverCbEl = document.getElementById('src-auto-discover-cb');
    const saveAutoTopicsBtnEl = document.getElementById('save-auto-topics-btn');
    const srcAutoNamesInputEl = document.getElementById('src-auto-names-input');
    const autoDiscoveredListEl = document.getElementById('auto-discovered-list');
    const sourcesListEl = document.getElementById('sources-list');
    const addSourceFormEl = document.getElementById('add-source-form');
    const newSrcPluginEl = document.getElementById('new-src-plugin');
    const newSrcModeEl = document.getElementById('new-src-mode');
    const newSrcTargetEl = document.getElementById('new-src-target');
    const newSrcWeightEl = document.getElementById('new-src-weight');
    const newSrcLabelEl = document.getElementById('new-src-label');
    const newSrcScoreFieldEl = document.getElementById('new-src-score-field');
    const newSrcNameFieldEl = document.getElementById('new-src-name-field');

    const saveAllCfgBtnEl = document.getElementById('save-all-cfg-btn');
    const resetAllCfgBtnEl = document.getElementById('reset-all-cfg-btn');
    const configGroupsWrapEl = document.getElementById('config-groups-wrap');

    const grantPluginSelectEl = document.getElementById('grant-plugin-select');
    const grantResourceSelectEl = document.getElementById('grant-resource-select');
    const quickGrantWriteBtnEl = document.getElementById('quick-grant-write-btn');
    const aclPendingEl = document.getElementById('acl-pending-list');
    const aclGrantedEl = document.getElementById('acl-granted-list');
    const aclOutgoingEl = document.getElementById('acl-outgoing-list');
    const protocolExamplesEl = document.getElementById('protocol-examples');

    const storeTabBtns = Array.from(document.querySelectorAll('[data-store-tab]'));
    const topicListEl = document.getElementById('topic-list');
    const nsSelectEl = document.getElementById('ns-select');
    const nsCustomEl = document.getElementById('ns-custom-input');
    const prefixFilterEl = document.getElementById('prefix-filter-input');
    const refreshDataBtnEl = document.getElementById('refresh-data-btn');
    const clearNsBtnEl = document.getElementById('clear-ns-btn');
    const kvListEl = document.getElementById('kv-list');

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
    let installedPlugins = [];
    let activeMainTab = 'scores';
    let activeBrowserStore = 'persist';
    let activeBrowserNs = '';
    let toastTimer = null;
    let resetConfirmTimer = null;
    let resetConfirmArmed = false;

    let scoreModeBound = '';
    let activeScoreStore = Dambo.storage;
    const selfScoresMap = new Map();
    const topicEventsMap = new Map();
    let kvBaselines = Object.create(null);
    const externalKvMap = new Map();
    const activeTopicSubs = new Map();
    const pendingEventWrites = new Map();

    let offScoreWatch = null;
    let offEventWatch = null;
    let offMetaWatch = null;
    let offCfgWatch = null;
    let offAclWatch = null;
    let offBrowserWatch = null;
    let dirtyConfigReaders = [];

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

    function roundClean(num) {
        if (!Number.isFinite(num)) return 0;
        return Math.round(num * 1000000) / 1000000;
    }

    function safeAvatarFace(rawFace) {
        if (typeof rawFace !== 'string') return '';
        const normalized = Dambo.utils.normalizeUrl(rawFace.trim());
        if (!normalized) return '';
        return /^https?:\/\//i.test(normalized) ? normalized : '';
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

        return {
            playerKey: playerKey,
            uid: uid,
            uname: uname,
            face: safeAvatarFace(obj.face || obj.avatar || ''),
        };
    }

    function formatDisplayScore(score) {
        const decimals = Math.min(4, Math.max(0, Math.round(Number(currentCfg.score_decimals || 0))));
        const fixed = Number(score || 0).toFixed(decimals);
        const body = decimals === 0 ? fixed : fixed.replace(/\.?0+$/, '');
        return String(currentCfg.score_prefix || '') + body + String(currentCfg.score_unit || '');
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

    function renderEmpty(container, text) {
        const div = document.createElement('div');
        div.className = 'muted';
        div.textContent = text;
        container.replaceChildren(div);
    }

    function resolveCanonicalObsUrl(meta) {
        const rel = meta && meta.route ? meta.route : ('/' + Dambo.plugin.name + '/');
        return new URL(rel, window.location.origin).toString();
    }

    function switchMainTab(tabName) {
        const valid = ['scores', 'sources', 'config', 'push', 'data'];
        activeMainTab = valid.indexOf(tabName) !== -1 ? tabName : 'scores';
        mainTabBtns.forEach(function (btn) {
            btn.classList.toggle('active', btn.getAttribute('data-main-tab') === activeMainTab);
        });
        secScoresEl.classList.toggle('hidden', activeMainTab !== 'scores');
        secSourcesEl.classList.toggle('hidden', activeMainTab !== 'sources');
        secConfigEl.classList.toggle('hidden', activeMainTab !== 'config');
        secPushEl.classList.toggle('hidden', activeMainTab !== 'push');
        secDataEl.classList.toggle('hidden', activeMainTab !== 'data');
    }

    // =========================================================================
    // 4. 聚合与跨插件数据源同步（与 OBS 展示端保持一致口径）
    // =========================================================================
    function buildSourceCatalog() {
        const explicitSources = [];
        const disabledIds = new Set();
        const declaredTopicKeys = new Set();

        const rawSources = Array.isArray(currentCfg.sources) ? currentCfg.sources : [];
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

        const autoTopicSources = [];
        if (currentCfg.auto_discover_topics) {
            const wantNames = new Set(
                (Array.isArray(currentCfg.auto_topic_names) ? currentCfg.auto_topic_names : [])
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
        const ruleById = new Map();
        for (let k = 0; k < allActive.length; k++) {
            ruleById.set(allActive[k].id, allActive[k]);
        }

        return {
            explicit: explicitSources,
            autoTopics: autoTopicSources,
            active: allActive,
            ruleById: ruleById,
            disabledIds: disabledIds,
        };
    }

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

    function cancelAllPendingWrites() {
        pendingEventWrites.forEach(function (tid) {
            clearTimeout(tid);
        });
        pendingEventWrites.clear();
    }

    function scheduleManageIdempotentEventWrite(evtKey, record) {
        topicEventsMap.set(evtKey, record);
        if (pendingEventWrites.has(evtKey)) {
            clearTimeout(pendingEventWrites.get(evtKey));
        }
        const targetStore = activeScoreStore;
        // 延迟略高于展示页（110..190ms），当展示页同时开启时由展示页抢先写入，管理页收到 SSE 后自动跳过重复 PUT
        const delayMs = 110 + Math.floor(Math.random() * 80);
        const tid = setTimeout(function () {
            pendingEventWrites.delete(evtKey);
            if (activeScoreStore !== targetStore) return;
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

        if (rule.target === 'round' && typeof payload === 'object' && typeof payload.event === 'string') {
            if (payload.event !== 'solved' || !payload.winner) return [];
            const winnerObj = typeof payload.winner === 'string' ? { uname: payload.winner } : payload.winner;
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

    function handleIncomingTopicMessage(msg, rule) {
        if (!rule) return;
        const ops = normalizeTopicOps(msg, rule);
        if (ops.length === 0) return;

        let changed = false;
        for (let i = 0; i < ops.length; i++) {
            const op = ops[i];
            if (op.op === 'reset_source') {
                const snapKey = sanitizeKvKey('snap:' + rule.id, 'snap');
                selfScoresMap.forEach(function (val, k) {
                    if (k === snapKey || (val && val.source_id === rule.id)) {
                        selfScoresMap.delete(k);
                        activeScoreStore.delete('scores', k).catch(function () {});
                    }
                });
                topicEventsMap.forEach(function (val, k) {
                    if (val && val.source_id === rule.id) {
                        topicEventsMap.delete(k);
                        activeScoreStore.delete('topic_events', k).catch(function () {});
                    }
                });
                renderManageRankList();
                return;
            }
            if (op.op === 'delete_player') {
                const srcKey = sanitizeKvKey('src:' + rule.id + ':' + op.identity.playerKey, 'src');
                if (selfScoresMap.has(srcKey)) {
                    selfScoresMap.delete(srcKey);
                    activeScoreStore.delete('scores', srcKey).catch(function () {});
                }
                topicEventsMap.forEach(function (val, k) {
                    if (val && val.source_id === rule.id && val.player_key === op.identity.playerKey) {
                        topicEventsMap.delete(k);
                        activeScoreStore.delete('topic_events', k).catch(function () {});
                    }
                });
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
                activeScoreStore.set('scores', snapKey, snapVal).catch(function () {});
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
                activeScoreStore.set('scores', srcKey, srcVal).catch(function () {});
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
                scheduleManageIdempotentEventWrite(evtKey, evtRecord);
                changed = true;
            }
        }
        if (changed) renderManageRankList();
    }

    function syncTopicSubscriptions(topicRules) {
        const desired = new Map();
        for (let i = 0; i < topicRules.length; i++) {
            const rule = topicRules[i];
            desired.set(rule.plugin + '\0' + rule.target, rule);
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
                const cur = activeTopicSubs.get(subKey);
                handleIncomingTopicMessage(msg, cur ? cur.rule : rule);
            });
            activeTopicSubs.set(subKey, { off: off, rule: rule });
        });
    }

    function syncKvSources(kvRules) {
        const desired = new Map();
        for (let i = 0; i < kvRules.length; i++) {
            desired.set(kvRules[i].id, kvRules[i]);
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
            if (existing && typeof existing.off === 'function') existing.off();

            const handle = rule.mode === 'persist_kv'
                ? Dambo.storage.of(rule.plugin)
                : Dambo.memory.of(rule.plugin);
            const rec = { spec: rule, entries: new Map(), off: null, errorCode: null };
            externalKvMap.set(kvKey, rec);

            const reload = function () {
                handle.list(rule.target).then(function (obj) {
                    if (externalKvMap.get(kvKey) !== rec) return;
                    rec.errorCode = null;
                    rec.entries.clear();
                    Object.keys(obj || {}).forEach(function (k) { rec.entries.set(k, obj[k]); });
                    renderManageRankList();
                }).catch(function (err) {
                    if (externalKvMap.get(kvKey) !== rec) return;
                    rec.errorCode = (err && err.code) || 'ERROR';
                    rec.entries.clear();
                    renderManageRankList();
                });
            };

            reload();
            rec.off = handle.watch(rule.target, function (ev) {
                if (externalKvMap.get(kvKey) !== rec) return;
                rec.errorCode = null;
                if (!applyMapMutation(rec.entries, ev)) {
                    reload();
                    return;
                }
                renderManageRankList();
            });
        });
    }

    async function ensureScoreStoreBound() {
        const mode = currentCfg.storage_mode === 'memory' ? 'memory' : 'persist';
        scoresModeHintEl.textContent = mode === 'memory'
            ? '当前存储模式：内存态（重启 Dambo 后清零，实时同步所有在线悬浮窗）'
            : '当前存储模式：持久化（重启保留，实时同步所有在线 OBS / 悬浮窗）';

        if (mode === scoreModeBound) return;
        cancelAllPendingWrites();
        if (offScoreWatch) offScoreWatch();
        if (offEventWatch) offEventWatch();
        if (offMetaWatch) offMetaWatch();

        scoreModeBound = mode;
        activeScoreStore = mode === 'memory' ? Dambo.memory : Dambo.storage;

        const bound = activeScoreStore;
        offScoreWatch = bound.watch('scores', function (ev) {
            if (activeScoreStore !== bound) return;
            if (!applyMapMutation(selfScoresMap, ev)) {
                bound.list('scores').then(function (all) {
                    selfScoresMap.clear();
                    Object.keys(all || {}).forEach(function (k) { selfScoresMap.set(k, all[k]); });
                    renderManageRankList();
                }).catch(function () {});
                return;
            }
            renderManageRankList();
        });

        offEventWatch = bound.watch('topic_events', function (ev) {
            if (activeScoreStore !== bound) return;
            if (ev && ev.op === 'clear') {
                cancelAllPendingWrites();
            } else if (ev && Array.isArray(ev.keys)) {
                for (let i = 0; i < ev.keys.length; i++) {
                    if (pendingEventWrites.has(ev.keys[i])) {
                        clearTimeout(pendingEventWrites.get(ev.keys[i]));
                        pendingEventWrites.delete(ev.keys[i]);
                    }
                }
            }
            if (!applyMapMutation(topicEventsMap, ev)) {
                bound.list('topic_events').then(function (all) {
                    topicEventsMap.clear();
                    Object.keys(all || {}).forEach(function (k) { topicEventsMap.set(k, all[k]); });
                    renderManageRankList();
                }).catch(function () {});
                return;
            }
            renderManageRankList();
        });

        offMetaWatch = bound.watch('meta', function (ev) {
            if (activeScoreStore !== bound) return;
            if (ev && ev.op === 'clear') {
                kvBaselines = Object.create(null);
                renderManageRankList();
                return;
            }
            bound.get('meta', 'kv_baselines').then(function (val) {
                kvBaselines = val && typeof val === 'object' ? val : Object.create(null);
                renderManageRankList();
            }).catch(function () {});
        });

        const res = await Promise.all([
            bound.list('scores').catch(function () { return {}; }),
            bound.list('topic_events').catch(function () { return {}; }),
            bound.get('meta', 'kv_baselines').catch(function () { return null; }),
        ]);
        selfScoresMap.clear();
        Object.keys(res[0] || {}).forEach(function (k) { selfScoresMap.set(k, res[0][k]); });
        topicEventsMap.clear();
        Object.keys(res[1] || {}).forEach(function (k) { topicEventsMap.set(k, res[1][k]); });
        kvBaselines = res[2] && typeof res[2] === 'object' ? res[2] : Object.create(null);
    }

    function aggregateForManage() {
        const catalog = buildSourceCatalog();
        const playersMap = new Map();

        function ensureRow(idInfo, tagLabel, updatedAt) {
            let row = playersMap.get(idInfo.playerKey);
            if (!row) {
                row = {
                    playerKey: idInfo.playerKey,
                    uid: idInfo.uid,
                    uname: idInfo.uname,
                    face: idInfo.face,
                    score: 0,
                    directScore: 0,
                    externalScore: 0,
                    directKeys: [],
                    sourceTotals: Object.create(null),
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
            if (tagLabel) row.tags.add(tagLabel);
            if (updatedAt > row.updatedAt) row.updatedAt = updatedAt;
            return row;
        }

        function addContribution(idInfo, weightedScore, tagLabel, updatedAt, isDirect, directKeyName) {
            if (!idInfo || !idInfo.playerKey) return;
            const row = ensureRow(idInfo, tagLabel, updatedAt);
            row.score = roundClean(row.score + weightedScore);
            if (isDirect) {
                row.directScore = roundClean(row.directScore + weightedScore);
                if (directKeyName && row.directKeys.indexOf(directKeyName) === -1) {
                    row.directKeys.push(directKeyName);
                }
            } else {
                row.externalScore = roundClean(row.externalScore + weightedScore);
            }
            const bucketTag = tagLabel || '其他';
            row.sourceTotals[bucketTag] = roundClean((row.sourceTotals[bucketTag] || 0) + weightedScore);
        }

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
                        addContribution(parseCanonicalIdentity(it, '', rule && rule.name_field), sc * weight, it.tag || label, Number(it.updated_at || snapUpdated), false);
                    }
                } else if (items && typeof items === 'object') {
                    Object.keys(items).forEach(function (subK) {
                        const subV = items[subK];
                        if (typeof subV === 'number') {
                            addContribution(parseCanonicalIdentity({}, subK), subV * weight, label, snapUpdated, false);
                        } else if (subV && typeof subV === 'object') {
                            const sc = pickFirstNumber([rule ? subV[rule.score_field] : undefined, subV.score, subV.value]);
                            if (sc === null) return;
                            addContribution(parseCanonicalIdentity(subV, subK, rule && rule.name_field), sc * weight, subV.tag || label, Number(subV.updated_at || snapUpdated), false);
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
                const label = (val && val.tag) || (rule && (rule.label || rule.plugin)) || sourceId || '外接';
                addContribution(parseCanonicalIdentity(typeof val === 'object' ? val : {}, fallbackPk, rule && rule.name_field), rawScore * weight, label, Number((val && val.updated_at) || 0), false);
                return;
            }

            if (typeof val === 'number') {
                addContribution(parseCanonicalIdentity({}, key), val, '手动', 0, true, key);
            } else if (val && typeof val === 'object') {
                const rawScore = pickFirstNumber([val.score, val.value]);
                if (rawScore === null) return;
                addContribution(parseCanonicalIdentity(val, key), rawScore, val.tag || '直接推送', Number(val.updated_at || 0), true, key);
            }
        });

        topicEventsMap.forEach(function (ev) {
            if (!ev || typeof ev !== 'object') return;
            const sid = String(ev.source_id || '').trim();
            if (sid && catalog.disabledIds.has(sid)) return;
            const rule = sid ? catalog.ruleById.get(sid) : null;
            const weight = rule ? rule.weight : 1;
            const delta = Number(ev.delta);
            if (!Number.isFinite(delta) || delta === 0) return;
            addContribution(parseCanonicalIdentity(ev, ev.player_key || ''), delta * weight, ev.tag || (rule && (rule.label || rule.plugin)) || '', Number(ev.updated_at || 0), false);
        });

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
                    }
                }
                addContribution(id, effectiveRaw * weight, rule.label || rule.plugin, updatedAt, false);
            });
        });

        const list = Array.from(playersMap.values());
        const sortDir = currentCfg.sort_order === 'asc' ? 1 : -1;
        const tieBreaker = currentCfg.tie_breaker || 'recent';
        list.sort(function (a, b) {
            if (a.score !== b.score) return (a.score - b.score) * sortDir;
            if (tieBreaker === 'earliest') {
                if (a.updatedAt !== b.updatedAt) return a.updatedAt - b.updatedAt;
            } else if (tieBreaker === 'recent') {
                if (a.updatedAt !== b.updatedAt) return b.updatedAt - a.updatedAt;
            }
            return a.uname.localeCompare(b.uname, 'zh-Hans-CN');
        });

        for (let i = 0; i < list.length; i++) {
            list[i].rank = i + 1;
        }
        return list;
    }

    // =========================================================================
    // 5. Tab 1: 实时榜单展示与手动积分管理
    // =========================================================================
    async function adjustPlayerScore(entry, deltaOrTarget, isSetTarget) {
        try {
            const targetKey = (entry.directKeys && entry.directKeys[0]) || sanitizeKvKey(entry.playerKey, 'p');
            const existing = selfScoresMap.get(targetKey);
            let existingDirectScore = 0;
            let existingTag = '手动';
            if (typeof existing === 'number') {
                existingDirectScore = existing;
            } else if (existing && typeof existing === 'object') {
                existingDirectScore = Number(pickFirstNumber([existing.score, existing.value]) || 0);
                if (existing.tag) existingTag = existing.tag;
            }

            let nextDirectScore;
            if (isSetTarget) {
                // 反推出需要写入直接层的分值，使得 direct + external === deltaOrTarget
                nextDirectScore = roundClean(Number(deltaOrTarget) - Number(entry.externalScore || 0));
            } else {
                nextDirectScore = roundClean(existingDirectScore + Number(deltaOrTarget));
            }

            const payload = {
                player_key: entry.playerKey,
                uid: entry.uid || 0,
                uname: entry.uname,
                face: entry.face || '',
                score: nextDirectScore,
                tag: existingTag,
                updated_at: Date.now(),
            };
            await activeScoreStore.set('scores', targetKey, payload);
            selfScoresMap.set(targetKey, payload);
            renderManageRankList();
            showToast('已更新 ' + entry.uname + ' 的积分');
        } catch (err) {
            handleApiError(err, '调整积分失败');
        }
    }

    async function deleteEntirePlayer(entry) {
        try {
            const delScoreKeys = [];
            selfScoresMap.forEach(function (val, k) {
                if (k === entry.playerKey || (entry.directKeys && entry.directKeys.indexOf(k) !== -1)) {
                    delScoreKeys.push(k);
                    return;
                }
                if (val && typeof val === 'object' && val.player_key === entry.playerKey) {
                    delScoreKeys.push(k);
                }
            });
            for (let i = 0; i < delScoreKeys.length; i++) {
                selfScoresMap.delete(delScoreKeys[i]);
                await activeScoreStore.delete('scores', delScoreKeys[i]);
            }

            const delEventKeys = [];
            topicEventsMap.forEach(function (ev, k) {
                if (ev && ev.player_key === entry.playerKey) {
                    delEventKeys.push(k);
                }
            });
            for (let j = 0; j < delEventKeys.length; j++) {
                topicEventsMap.delete(delEventKeys[j]);
                await activeScoreStore.delete('topic_events', delEventKeys[j]);
            }

            // 为外部只读 KV 源里的该玩家写入基线抵扣，使其立即归零移除
            let baseChanged = false;
            externalKvMap.forEach(function (rec) {
                const rule = rec.spec;
                rec.entries.forEach(function (entryVal, k) {
                    const id = parseCanonicalIdentity(
                        typeof entryVal === 'object' ? entryVal : {},
                        k,
                        rule.name_field
                    );
                    if (id.playerKey !== entry.playerKey) return;
                    const rawScore = typeof entryVal === 'number'
                        ? entryVal
                        : pickFirstNumber([entryVal[rule.score_field], entryVal.score, entryVal.value]);
                    if (rawScore === null) return;
                    if (!kvBaselines[rule.id]) kvBaselines[rule.id] = {};
                    kvBaselines[rule.id][k] = { rawScore: rawScore, updatedAt: Date.now() };
                    baseChanged = true;
                });
            });
            if (baseChanged) {
                await activeScoreStore.set('meta', 'kv_baselines', kvBaselines);
            }

            renderManageRankList();
            showToast('已清除玩家 ' + entry.uname + ' 的全部得分');
        } catch (err) {
            handleApiError(err, '删除玩家失败');
        }
    }

    async function resetAllScores() {
        try {
            cancelAllPendingWrites();
            const nextBaselines = Object.create(null);
            externalKvMap.forEach(function (rec) {
                const rule = rec.spec;
                const bucket = Object.create(null);
                let hasItem = false;
                rec.entries.forEach(function (entryVal, k) {
                    const rawScore = typeof entryVal === 'number'
                        ? entryVal
                        : pickFirstNumber([entryVal && entryVal[rule.score_field], entryVal && entryVal.score, entryVal && entryVal.value]);
                    if (rawScore !== null) {
                        bucket[k] = { rawScore: rawScore, updatedAt: Date.now() };
                        hasItem = true;
                    }
                });
                if (hasItem) nextBaselines[rule.id] = bucket;
            });

            await Promise.all([
                activeScoreStore.clearNamespace('scores'),
                activeScoreStore.clearNamespace('topic_events'),
            ]);
            selfScoresMap.clear();
            topicEventsMap.clear();
            kvBaselines = nextBaselines;

            if (Object.keys(nextBaselines).length > 0) {
                await activeScoreStore.set('meta', 'kv_baselines', nextBaselines);
            } else {
                await activeScoreStore.delete('meta', 'kv_baselines').catch(function () {});
            }

            renderManageRankList();
            showToast('已清空当前模式下的全部积分');
        } catch (err) {
            handleApiError(err, '清空积分失败');
        }
    }

    function renderManageRankList() {
        const list = aggregateForManage();
        tabCountScoresEl.textContent = String(list.length);

        if (list.length === 0) {
            renderEmpty(manageRankListEl, '当前暂无玩家积分，可在上方手动添加、点击“模拟测试加分”，或等待其他插件推分');
            return;
        }

        const frag = document.createDocumentFragment();
        for (let i = 0; i < list.length; i++) {
            const entry = list[i];
            const row = document.createElement('div');
            row.className = 'rank-item-row';

            const left = document.createElement('div');
            left.className = 'rank-item-left';

            const rankEl = document.createElement('span');
            rankEl.className = 'rank-num' + (entry.rank <= 3 ? ' top' + entry.rank : '');
            rankEl.textContent = '#' + entry.rank;
            left.appendChild(rankEl);

            const avatarEl = Dambo.utils.createAvatar(entry.face, 'm-avatar');
            avatarEl.alt = entry.uname;
            left.appendChild(avatarEl);

            const metaCol = document.createElement('div');
            metaCol.style.minWidth = '0';
            const nameRow = document.createElement('div');
            nameRow.className = 'row';
            const nameEl = document.createElement('strong');
            nameEl.textContent = entry.uname;
            nameRow.appendChild(nameEl);
            if (entry.uid > 0) {
                nameRow.appendChild(makeBadge('UID ' + entry.uid));
            }
            metaCol.appendChild(nameRow);

            const srcRow = document.createElement('div');
            srcRow.className = 'row';
            srcRow.style.marginTop = '4px';
            Object.keys(entry.sourceTotals).forEach(function (srcLabel) {
                srcRow.appendChild(makeBadge(srcLabel + ': ' + entry.sourceTotals[srcLabel], 'accent'));
            });
            metaCol.appendChild(srcRow);
            left.appendChild(metaCol);
            row.appendChild(left);

            const right = document.createElement('div');
            right.className = 'row';
            const scoreEl = document.createElement('span');
            scoreEl.className = 'rank-score-big';
            scoreEl.textContent = formatDisplayScore(entry.score);
            right.appendChild(scoreEl);

            right.appendChild(makeBtn('+1', 'small', function () { adjustPlayerScore(entry, 1, false); }));
            right.appendChild(makeBtn('+10', 'small ok', function () { adjustPlayerScore(entry, 10, false); }));
            right.appendChild(makeBtn('-1', 'small', function () { adjustPlayerScore(entry, -1, false); }));

            const setBtn = makeBtn('设分', 'small', function () {
                mUidEl.value = entry.uid > 0 ? String(entry.uid) : '';
                mUnameEl.value = entry.uname;
                mOpEl.value = 'set';
                mValEl.value = String(entry.score);
                mTagEl.value = '手动';
                mFaceEl.value = entry.face || '';
                mValEl.focus();
                mValEl.select();
                showToast('已填入上方表单，请修改目标分数后点击“应用积分调整”');
            });
            right.appendChild(setBtn);

            right.appendChild(makeBtn('清除', 'danger small', function () { deleteEntirePlayer(entry); }));

            row.appendChild(right);
            frag.appendChild(row);
        }
        manageRankListEl.replaceChildren(frag);
    }

    // =========================================================================
    // 6. Tab 2: 多插件数据源接入配置与 ACL 状态检测
    // =========================================================================
    function populatePluginDropdowns() {
        const externalPlugins = installedPlugins.filter(function (p) {
            return p && p.plugin_name !== Dambo.plugin.name;
        });

        newSrcPluginEl.replaceChildren();
        grantPluginSelectEl.replaceChildren();

        if (externalPlugins.length === 0) {
            const opt1 = document.createElement('option');
            opt1.value = '';
            opt1.textContent = '暂无其他已安装插件';
            newSrcPluginEl.appendChild(opt1);

            const opt2 = document.createElement('option');
            opt2.value = '';
            opt2.textContent = '暂无其他已安装插件';
            grantPluginSelectEl.appendChild(opt2);
            return;
        }

        for (let i = 0; i < externalPlugins.length; i++) {
            const p = externalPlugins[i];
            const label = (p.name || p.plugin_name) + ' (' + p.plugin_name + ')';

            const o1 = document.createElement('option');
            o1.value = p.plugin_name;
            o1.textContent = label;
            newSrcPluginEl.appendChild(o1);

            const o2 = document.createElement('option');
            o2.value = p.namedomain;
            o2.textContent = label;
            grantPluginSelectEl.appendChild(o2);
        }
    }

    function updateNewSourcePlaceholder() {
        const pluginName = newSrcPluginEl.value;
        const mode = newSrcModeEl.value;
        const p = installedPlugins.find(function (x) { return x && x.plugin_name === pluginName; });
        if (mode === 'topic') {
            const firstTopic = p && Array.isArray(p.topics) && p.topics[0] ? p.topics[0].name : 'scoreboard';
            if (!newSrcTargetEl.value || newSrcTargetEl.value === 'scores') {
                newSrcTargetEl.value = firstTopic;
            }
        } else {
            if (!newSrcTargetEl.value || newSrcTargetEl.value === 'round' || newSrcTargetEl.value === 'scoreboard') {
                newSrcTargetEl.value = 'scores';
            }
        }
    }

    function resourceForSourceRule(rule) {
        if (rule.mode === 'memory_kv') return 'memory:' + rule.target;
        if (rule.mode === 'persist_kv') return 'persist:' + rule.target;
        return 'topic:' + rule.target;
    }

    function isKnownPublicTopic(pluginIdent, topicName) {
        const p = installedPlugins.find(function (x) {
            return x && (x.plugin_name === pluginIdent || x.plugin_id === pluginIdent);
        });
        if (!p || !Array.isArray(p.topics)) return false;
        return p.topics.some(function (t) { return t && t.name === topicName && Boolean(t.public); });
    }

    async function saveUpdatedSourcesArray(nextSources, toastMsg) {
        try {
            await Dambo.config.set('sources', nextSources);
            showToast(toastMsg || '数据源配置已保存');
            await refreshConfigAndData();
        } catch (err) {
            handleApiError(err, '保存数据源失败');
        }
    }

    async function renderSourcesPanel() {
        const catalog = buildSourceCatalog();
        tabCountSourcesEl.textContent = String(catalog.active.length);

        srcAutoDiscoverCbEl.checked = Boolean(currentCfg.auto_discover_topics);
        srcAutoNamesInputEl.value = (Array.isArray(currentCfg.auto_topic_names) ? currentCfg.auto_topic_names : []).join(', ');

        autoDiscoveredListEl.replaceChildren();
        if (!currentCfg.auto_discover_topics) {
            autoDiscoveredListEl.textContent = '已关闭公开 Topic 自动发现';
        } else if (catalog.autoTopics.length === 0) {
            autoDiscoveredListEl.textContent = '当前未扫描到匹配的公开 Topic（当其他插件发布 public: true 且名称匹配上述列表的 Topic 时会自动接入）';
        } else {
            const prefix = document.createElement('span');
            prefix.textContent = '已自动接入：';
            autoDiscoveredListEl.appendChild(prefix);
            catalog.autoTopics.forEach(function (t) {
                autoDiscoveredListEl.appendChild(makeBadge(t.plugin + ' / ' + t.target, 'ok'));
                autoDiscoveredListEl.appendChild(document.createTextNode(' '));
            });
        }

        const rawSources = Array.isArray(currentCfg.sources) ? currentCfg.sources : [];
        if (rawSources.length === 0) {
            renderEmpty(sourcesListEl, '暂无显式数据源规则，可在上方一键接入 24点 或在下方自定义添加');
            return;
        }

        sourcesListEl.replaceChildren();
        for (let idx = 0; idx < rawSources.length; idx++) {
            const rule = rawSources[idx];
            const box = document.createElement('div');
            box.className = rule.enabled !== false ? 'item-box overridden' : 'item-box';

            const top = document.createElement('div');
            top.className = 'item-top';

            const left = document.createElement('div');
            left.className = 'row';
            const title = document.createElement('strong');
            title.textContent = rule.plugin + ' -> ' + rule.target;
            left.appendChild(title);
            left.appendChild(makeBadge(rule.mode, 'accent'));
            left.appendChild(makeBadge(rule.enabled !== false ? '已启用' : '已停用', rule.enabled !== false ? 'ok' : ''));
            const statusSlot = document.createElement('span');
            statusSlot.className = 'row';
            left.appendChild(statusSlot);
            top.appendChild(left);

            const controls = document.createElement('div');
            controls.className = 'row';

            const enabledCb = document.createElement('input');
            enabledCb.type = 'checkbox';
            enabledCb.checked = rule.enabled !== false;
            enabledCb.title = '启用/停用';
            enabledCb.addEventListener('change', function () {
                const clone = rawSources.map(function (item, j) {
                    return j === idx ? Object.assign({}, item, { enabled: enabledCb.checked }) : item;
                });
                saveUpdatedSourcesArray(clone, enabledCb.checked ? '已启用数据源' : '已停用数据源');
            });
            controls.appendChild(enabledCb);

            const delBtn = makeBtn('删除', 'danger small', function () {
                const clone = rawSources.filter(function (_, j) { return j !== idx; });
                saveUpdatedSourcesArray(clone, '已移除数据源规则');
            });

            top.appendChild(controls);
            box.appendChild(top);

            const grid = document.createElement('div');
            grid.className = 'field-grid';

            const weightLbl = document.createElement('label');
            weightLbl.textContent = '权重倍率 (weight)';
            const weightInp = document.createElement('input');
            weightInp.type = 'number';
            weightInp.step = 'any';
            weightInp.value = String(rule.weight !== undefined ? rule.weight : 1);
            weightLbl.appendChild(weightInp);
            grid.appendChild(weightLbl);

            const tagLbl = document.createElement('label');
            tagLbl.textContent = '展示标签 (label)';
            const tagInp = document.createElement('input');
            tagInp.type = 'text';
            tagInp.maxLength = 32;
            tagInp.value = String(rule.label || '');
            tagLbl.appendChild(tagInp);
            grid.appendChild(tagLbl);

            const sfLbl = document.createElement('label');
            sfLbl.textContent = '分数字段 (score_field)';
            const sfInp = document.createElement('input');
            sfInp.type = 'text';
            sfInp.maxLength = 64;
            sfInp.value = String(rule.score_field || 'score');
            sfLbl.appendChild(sfInp);
            grid.appendChild(sfLbl);

            const nfLbl = document.createElement('label');
            nfLbl.textContent = '昵称字段 (name_field)';
            const nfInp = document.createElement('input');
            nfInp.type = 'text';
            nfInp.maxLength = 64;
            nfInp.value = String(rule.name_field || 'uname');
            nfLbl.appendChild(nfInp);
            grid.appendChild(nfLbl);

            box.appendChild(grid);

            const saveRow = document.createElement('div');
            saveRow.className = 'row';
            saveRow.appendChild(makeBtn('保存规则参数', 'small primary', function () {
                const w = Number(weightInp.value);
                if (!Number.isFinite(w) || w < -1000 || w > 10000) {
                    showToast('权重必须在 -1000 ~ 10000 之间', true);
                    return;
                }
                const clone = rawSources.map(function (item, j) {
                    if (j !== idx) return item;
                    return Object.assign({}, item, {
                        weight: w,
                        label: tagInp.value.trim(),
                        score_field: sfInp.value.trim() || 'score',
                        name_field: nfInp.value.trim() || 'uname',
                    });
                });
                saveUpdatedSourcesArray(clone, '已更新数据源参数');
            }));
            saveRow.appendChild(delBtn);
            box.appendChild(saveRow);
            sourcesListEl.appendChild(box);

            // 异步检查权限状态
            (function (targetRule, slotEl) {
                if (targetRule.mode === 'topic' && isKnownPublicTopic(targetRule.plugin, targetRule.target)) {
                    slotEl.replaceChildren(makeBadge('公开 Topic (免授权)', 'ok'));
                    return;
                }
                const resStr = resourceForSourceRule(targetRule);
                const reqLevel = targetRule.mode === 'topic' ? 'subscribe' : 'read';
                Dambo.acl.check(targetRule.plugin, resStr).then(function (chk) {
                    const eff = chk && chk.effective_level;
                    if (eff && eff !== 'none') {
                        slotEl.replaceChildren(makeBadge('权限就绪: ' + eff, 'ok'));
                        return;
                    }
                    slotEl.replaceChildren(
                        makeBadge('未授权', 'warn'),
                        makeBtn('申请权限', 'small', function () {
                            Dambo.acl.request(targetRule.plugin, {
                                resource: resStr,
                                level: reqLevel,
                                reason: '通用积分榜接入数据源并汇总排名',
                            }).then(function () {
                                showToast('已向 ' + targetRule.plugin + ' 发起读取/订阅权限申请，请到对方管理页批准');
                                loadAclPanel();
                            }).catch(function (err) {
                                handleApiError(err, '发起权限申请失败');
                            });
                        })
                    );
                }).catch(function (err) {
                    const msg = err && err.code === 'PLUGIN_NOT_FOUND' ? '目标插件未安装' : '状态未知';
                    slotEl.replaceChildren(makeBadge(msg));
                });
            })(rule, statusSlot);
        }
    }

    // =========================================================================
    // 7. Tab 3: 分组外观与排名配置表单
    // =========================================================================
    const CONFIG_GROUPS = [
        {
            title: '文案与分值格式',
            keys: ['title', 'subtitle', 'empty_text', 'score_prefix', 'score_unit', 'score_decimals'],
        },
        {
            title: '视觉主题与排版布局',
            keys: ['theme', 'layout', 'accent_color', 'gold_color', 'text_color', 'bg_opacity', 'font_scale', 'border_radius', 'row_gap', 'max_width'],
        },
        {
            title: '展示元素开关与动效',
            keys: ['show_header', 'show_footer', 'show_medals', 'show_avatar', 'show_progress_bar', 'show_delta', 'show_source_tag', 'show_empty_state', 'delta_ttl_ms'],
        },
        {
            title: '排名策略、分页与持久化',
            keys: ['top_n', 'sort_order', 'tie_breaker', 'rank_mode', 'min_score', 'hide_zero_scores', 'auto_paginate', 'page_interval_sec', 'storage_mode', 'publish_leaderboard_topic'],
        },
    ];
    const SOURCE_MANAGED_KEYS = new Set(['auto_discover_topics', 'auto_topic_names', 'sources']);

    function createTypedInput(entry, readerOut, onChange) {
        const t = entry.type;
        const meta = entry.meta || {};
        const wrap = document.createElement('div');
        wrap.className = 'row';

        if (t === 'bool') {
            const cb = document.createElement('input');
            cb.type = 'checkbox';
            cb.checked = Boolean(entry.value);
            cb.addEventListener('change', onChange);
            wrap.appendChild(cb);
            readerOut.get = function () { return cb.checked; };
            return wrap;
        }

        if (t === 'number') {
            const inp = document.createElement('input');
            inp.type = meta.format === 'slider' ? 'range' : 'number';
            if (typeof meta.min === 'number') inp.min = String(meta.min);
            if (typeof meta.max === 'number') inp.max = String(meta.max);
            if (typeof meta.step === 'number') inp.step = String(meta.step);
            else inp.step = meta.format === 'integer' ? '1' : 'any';
            inp.value = String(entry.value);
            wrap.appendChild(inp);

            if (meta.format === 'slider') {
                const readout = document.createElement('span');
                readout.className = 'badge';
                readout.textContent = inp.value;
                inp.addEventListener('input', function () { readout.textContent = inp.value; });
                wrap.appendChild(readout);
            }
            inp.addEventListener('input', onChange);
            inp.addEventListener('change', onChange);
            readerOut.get = function () { return Number(inp.value); };
            return wrap;
        }

        if (t === 'enum') {
            const sel = document.createElement('select');
            const opts = Array.isArray(meta.options) ? meta.options : [];
            for (let i = 0; i < opts.length; i++) {
                const opt = opts[i];
                const o = document.createElement('option');
                if (typeof opt === 'string') {
                    o.value = opt;
                    o.textContent = opt;
                } else {
                    o.value = String(opt.value);
                    o.textContent = opt.label ? (opt.label + ' (' + opt.value + ')') : String(opt.value);
                }
                if (o.value === String(entry.value)) o.selected = true;
                sel.appendChild(o);
            }
            sel.addEventListener('change', onChange);
            wrap.appendChild(sel);
            readerOut.get = function () { return sel.value; };
            return wrap;
        }

        if (t === 'string') {
            const inp = document.createElement('input');
            if (meta.format === 'color') inp.type = 'color';
            else if (meta.format === 'url') inp.type = 'url';
            else inp.type = 'text';
            inp.value = String(entry.value === undefined || entry.value === null ? '' : entry.value);
            if (meta.format !== 'color') inp.style.minWidth = '200px';
            inp.addEventListener('input', onChange);
            inp.addEventListener('change', onChange);
            wrap.appendChild(inp);
            readerOut.get = function () { return inp.value; };
            return wrap;
        }

        const ta = document.createElement('textarea');
        ta.value = JSON.stringify(entry.value, null, 2);
        ta.addEventListener('input', onChange);
        wrap.appendChild(ta);
        readerOut.get = function () { return JSON.parse(ta.value); };
        return wrap;
    }

    function renderConfigPanel(meta) {
        const cfgSchema = (meta && meta.config) || {};
        const allKeys = Object.keys(cfgSchema);
        tabCountConfigEl.textContent = String(allKeys.length);
        dirtyConfigReaders = [];
        configGroupsWrapEl.replaceChildren();

        const placed = new Set();
        const groups = CONFIG_GROUPS.map(function (g) {
            return {
                title: g.title,
                keys: g.keys.filter(function (k) {
                    if (Object.prototype.hasOwnProperty.call(cfgSchema, k)) {
                        placed.add(k);
                        return true;
                    }
                    return false;
                }),
            };
        });

        const leftover = allKeys.filter(function (k) {
            return !placed.has(k) && !SOURCE_MANAGED_KEYS.has(k);
        });
        if (leftover.length > 0) {
            groups.push({ title: '其他配置项', keys: leftover });
        }

        groups.forEach(function (group) {
            if (group.keys.length === 0) return;
            const secBox = document.createElement('div');
            secBox.className = 'cfg-group-box';

            const h3 = document.createElement('h3');
            h3.className = 'cfg-group-title';
            h3.textContent = group.title;
            secBox.appendChild(h3);

            const grid = document.createElement('div');
            grid.className = 'cfg-grid';

            group.keys.forEach(function (key) {
                const entry = cfgSchema[key];
                const metaInfo = entry.meta || {};
                const box = document.createElement('div');
                box.className = entry.is_overridden ? 'item-box overridden' : 'item-box';

                const top = document.createElement('div');
                top.className = 'item-top';
                const left = document.createElement('div');
                left.className = 'row';
                const lbl = document.createElement('strong');
                lbl.textContent = metaInfo.label || key;
                left.appendChild(lbl);
                if (entry.is_overridden) {
                    left.appendChild(makeBadge('已覆写', 'accent'));
                }
                top.appendChild(left);

                const actions = document.createElement('div');
                actions.className = 'row';
                const reader = {};
                const initialSnapshot = JSON.stringify(entry.value);
                const saveBtn = makeBtn('保存', 'small', async function () {
                    try {
                        await Dambo.config.set(key, reader.get());
                        showToast('已保存 ' + (metaInfo.label || key));
                        await refreshConfigAndData();
                    } catch (err) {
                        handleApiError(err, '保存配置失败');
                    }
                });

                const syncDirty = function () {
                    let dirty = true;
                    try {
                        dirty = JSON.stringify(reader.get()) !== initialSnapshot;
                    } catch (_) {
                        dirty = true;
                    }
                    saveBtn.classList.toggle('primary', dirty);
                };

                const inputEl = createTypedInput(entry, reader, syncDirty);
                dirtyConfigReaders.push({ key: key, reader: reader, initial: initialSnapshot });

                actions.appendChild(saveBtn);
                if (entry.is_overridden) {
                    actions.appendChild(makeBtn('默认', 'small', async function () {
                        try {
                            await Dambo.config.reset(key);
                            showToast('已恢复默认 ' + (metaInfo.label || key));
                            await refreshConfigAndData();
                        } catch (err) {
                            handleApiError(err, '恢复默认失败');
                        }
                    }));
                }
                top.appendChild(actions);
                box.appendChild(top);

                if (metaInfo.description) {
                    const desc = document.createElement('div');
                    desc.className = 'muted';
                    desc.textContent = metaInfo.description;
                    box.appendChild(desc);
                }
                box.appendChild(inputEl);
                grid.appendChild(box);
            });

            secBox.appendChild(grid);
            configGroupsWrapEl.appendChild(secBox);
        });

        const jumpBox = document.createElement('div');
        jumpBox.className = 'item-box';
        const jumpTop = document.createElement('div');
        jumpTop.className = 'item-top';
        const jumpText = document.createElement('span');
        jumpText.className = 'muted';
        jumpText.textContent = '数据源规则（sources）与公开 Topic 自动发现设置位于「数据源接入」选项卡。';
        jumpTop.appendChild(jumpText);
        jumpTop.appendChild(makeBtn('前往数据源接入', 'small', function () { switchMainTab('sources'); }));
        jumpBox.appendChild(jumpTop);
        configGroupsWrapEl.appendChild(jumpBox);
    }

    // =========================================================================
    // 8. Tab 4: ACL 权限控制与推分协议示例
    // =========================================================================
    function formatRequesterText(item) {
        const d = item.requester_display;
        if (d && d.installed) {
            const title = d.name || d.plugin_name || item.requester;
            return title + ' (' + item.requester + ')';
        }
        return item.requester + ' [已卸载]';
    }

    function allowedGrantLevels(resource) {
        if (resource.indexOf('topic:') === 0) return ['subscribe'];
        if (resource === 'persist:config') return ['read'];
        return ['read', 'write'];
    }

    function makeLevelSelect(levels, selected) {
        const sel = document.createElement('select');
        levels.forEach(function (lvl) {
            const o = document.createElement('option');
            o.value = lvl;
            o.textContent = lvl;
            if (selected === lvl) o.selected = true;
            sel.appendChild(o);
        });
        return sel;
    }

    async function loadAclPanel() {
        try {
            const data = await Dambo.acl.list();
            const incoming = Array.isArray(data.incoming) ? data.incoming : [];
            const outgoing = Array.isArray(data.outgoing) ? data.outgoing : [];

            const pending = incoming.filter(function (r) { return r.status === 'pending'; });
            const resolved = incoming.filter(function (r) { return r.status !== 'pending'; });

            tabCountAclEl.textContent = String(pending.length);
            tabCountAclEl.classList.toggle('hidden', pending.length === 0);

            if (pending.length === 0) {
                renderEmpty(aclPendingEl, '暂无待审批申请');
            } else {
                aclPendingEl.replaceChildren();
                pending.forEach(function (rec) {
                    const box = document.createElement('div');
                    box.className = 'item-box';
                    const top = document.createElement('div');
                    top.className = 'item-top';

                    const info = document.createElement('div');
                    info.className = 'row';
                    const reqTitle = document.createElement('strong');
                    reqTitle.textContent = formatRequesterText(rec);
                    info.appendChild(reqTitle);
                    info.appendChild(makeBadge(rec.resource, 'accent'));
                    if (rec.requested_level) info.appendChild(makeBadge('申请: ' + rec.requested_level, 'warn'));
                    top.appendChild(info);

                    const controls = document.createElement('div');
                    controls.className = 'row';
                    const sel = makeLevelSelect(allowedGrantLevels(rec.resource), rec.requested_level);
                    controls.appendChild(sel);
                    controls.appendChild(makeBtn('批准', 'ok small', async function () {
                        try {
                            await Dambo.acl.set(rec.requester, { resource: rec.resource, level: sel.value });
                            showToast('已批准 ' + rec.requester + ' -> ' + rec.resource);
                            await loadAclPanel();
                        } catch (err) {
                            handleApiError(err, '审批失败');
                        }
                    }));
                    controls.appendChild(makeBtn('拒绝', 'danger small', async function () {
                        try {
                            await Dambo.acl.set(rec.requester, { resource: rec.resource, level: 'none' });
                            showToast('已拒绝 ' + rec.requester);
                            await loadAclPanel();
                        } catch (err) {
                            handleApiError(err, '拒绝失败');
                        }
                    }));
                    top.appendChild(controls);
                    box.appendChild(top);

                    if (rec.reason) {
                        const rEl = document.createElement('div');
                        rEl.className = 'muted';
                        rEl.textContent = '申请理由: ' + rec.reason;
                        box.appendChild(rEl);
                    }
                    aclPendingEl.appendChild(box);
                });
            }

            if (resolved.length === 0) {
                renderEmpty(aclGrantedEl, '暂无已授权、自动放行或已拒绝规则');
            } else {
                aclGrantedEl.replaceChildren();
                resolved.forEach(function (rec) {
                    const box = document.createElement('div');
                    box.className = 'item-box';
                    const top = document.createElement('div');
                    top.className = 'item-top';

                    const isAuto = rec.status === 'auto_granted';
                    const isDenied = rec.status === 'denied' || rec.level === 'none';

                    const info = document.createElement('div');
                    info.className = 'row';
                    const reqTitle = document.createElement('strong');
                    reqTitle.textContent = formatRequesterText(rec);
                    info.appendChild(reqTitle);
                    info.appendChild(makeBadge(rec.resource, 'accent'));
                    if (isAuto) info.appendChild(makeBadge('自动放行 (' + rec.level + ')', 'warn'));
                    else if (isDenied) info.appendChild(makeBadge('已拒绝', 'danger'));
                    else info.appendChild(makeBadge('已授权 (' + rec.level + ')', 'ok'));
                    top.appendChild(info);

                    const controls = document.createElement('div');
                    controls.className = 'row';
                    const sel = makeLevelSelect(allowedGrantLevels(rec.resource).concat(['none']), rec.level);
                    controls.appendChild(sel);
                    controls.appendChild(makeBtn('更新', 'small', async function () {
                        try {
                            await Dambo.acl.set(rec.requester, { resource: rec.resource, level: sel.value });
                            showToast('已更新授权等级');
                            await loadAclPanel();
                        } catch (err) {
                            handleApiError(err, '更新失败');
                        }
                    }));
                    controls.appendChild(makeBtn('移除', 'danger small', async function () {
                        try {
                            await Dambo.acl.remove(rec.requester, rec.resource);
                            showToast('已移除授权规则');
                            await loadAclPanel();
                        } catch (err) {
                            handleApiError(err, '移除失败');
                        }
                    }));
                    top.appendChild(controls);
                    box.appendChild(top);
                    aclGrantedEl.appendChild(box);
                });
            }

            if (outgoing.length === 0) {
                renderEmpty(aclOutgoingEl, '暂无对外发起的权限申请');
            } else {
                aclOutgoingEl.replaceChildren();
                outgoing.forEach(function (rec) {
                    const box = document.createElement('div');
                    box.className = 'item-box';
                    const top = document.createElement('div');
                    top.className = 'row';
                    const ownerEl = document.createElement('strong');
                    ownerEl.textContent = '目标: ' + rec.owner;
                    top.appendChild(ownerEl);
                    top.appendChild(makeBadge(rec.resource, 'accent'));
                    const badgeCls = rec.status === 'granted' ? 'ok' : (rec.status === 'pending' || rec.status === 'auto_granted' ? 'warn' : 'danger');
                    top.appendChild(makeBadge(rec.status + ' (' + rec.level + ')', badgeCls));
                    box.appendChild(top);
                    aclOutgoingEl.appendChild(box);
                });
            }
        } catch (err) {
            handleApiError(err, '加载 ACL 状态失败');
        }
    }

    function renderProtocolExamples() {
        const examples = [
            {
                title: '方式一（推荐 / 零 ACL 审批）：通过公开 Topic 实时推分',
                desc: '任意其他插件向自身公开 Topic `scoreboard`（或 `score` / `scores`）发布事件，积分榜将自动发现并累加积分。',
                code: [
                    '// 累加玩家 10 分（推荐携带 event_id 防重）',
                    "await Dambo.topics.publish('scoreboard', {",
                    "  uid: 42,",
                    "  uname: '玩家A',",
                    "  face: 'https://...',",
                    "  delta: 10,",
                    "  tag: '竞猜',",
                    "  event_id: 'round_' + Date.now()",
                    '}, { public: true, description: "向积分榜推送加分" });',
                ].join('\n'),
            },
            {
                title: '方式二：直接写入积分榜存储（需先在上方授予 write 权限）',
                desc: '适合由外部插件全权维护某个子来源玩家的绝对分值，覆盖更新实时同步，不影响其他插件分数。',
                code: [
                    '// 先在「推送协议与权限」中授予对方插件 persist:scores 的 write 权限',
                    "await Dambo.storage.of('scoreboard').set('scores', 'src:myplugin:uid:42', {",
                    "  source_id: 'myplugin',",
                    "  uid: 42,",
                    "  uname: '玩家A',",
                    "  score: 120,",
                    "  tag: '抽奖',",
                    '  updated_at: Date.now()',
                    '});',
                ].join('\n'),
            },
            {
                title: '方式三：Topic 批量快照（snapshot）与来源清零（reset）',
                desc: '外部插件也可以定期广播自身完整榜单快照，或发送 reset 指令清空自身来源下的积分。',
                code: [
                    '// 发送快照替换本插件在积分榜上的全部条目',
                    "await Dambo.topics.publish('scoreboard', {",
                    "  action: 'snapshot',",
                    "  label: '答题榜',",
                    "  items: [",
                    "    { uid: 101, uname: '冠军', score: 300 },",
                    "    { uid: 102, uname: '亚军', score: 220 }",
                    '  ]',
                    '}, { public: true });',
                    '',
                    '// 清空本插件贡献的积分',
                    "await Dambo.topics.publish('scoreboard', { action: 'reset' }, { public: true });",
                ].join('\n'),
            },
        ];

        protocolExamplesEl.replaceChildren();
        examples.forEach(function (ex) {
            const box = document.createElement('div');
            box.className = 'item-box';

            const top = document.createElement('div');
            top.className = 'item-top';
            const title = document.createElement('strong');
            title.textContent = ex.title;
            top.appendChild(title);
            top.appendChild(makeBtn('复制代码', 'small', async function () {
                try {
                    await navigator.clipboard.writeText(ex.code);
                    showToast('已复制接入示例代码');
                } catch (_) {
                    showToast('请手动复制代码块中的示例');
                }
            }));
            box.appendChild(top);

            const desc = document.createElement('div');
            desc.className = 'muted';
            desc.textContent = ex.desc;
            box.appendChild(desc);

            const pre = document.createElement('pre');
            pre.className = 'code-block';
            const codeEl = document.createElement('code');
            codeEl.textContent = ex.code;
            pre.appendChild(codeEl);
            box.appendChild(pre);

            protocolExamplesEl.appendChild(box);
        });
    }

    // =========================================================================
    // 9. Tab 5: 存储检查器与自身对外 Topic 管理
    // =========================================================================
    function currentBrowserHandle() {
        return activeBrowserStore === 'memory' ? Dambo.memory : Dambo.storage;
    }

    function rebindBrowserWatch(ns) {
        if (offBrowserWatch) {
            offBrowserWatch();
            offBrowserWatch = null;
        }
        if (!ns) return;
        const prefix = prefixFilterEl.value.trim() || undefined;
        offBrowserWatch = currentBrowserHandle().watch(
            ns,
            function () { loadBrowserEntries(); },
            prefix ? { prefix: prefix } : undefined
        );
    }

    async function loadTopicsPanel() {
        try {
            const list = await Dambo.topics.list('@self');
            tabCountTopicsEl.textContent = String(Array.isArray(list) ? list.length : 0);
            if (!list || list.length === 0) {
                renderEmpty(topicListEl, '当前尚未发布对外 Topic（在「外观与规则」开启「向外广播排行榜 Topic」后自动生成 leaderboard）');
                return;
            }
            topicListEl.replaceChildren();
            list.forEach(function (topic) {
                const box = document.createElement('div');
                box.className = 'item-box';
                const top = document.createElement('div');
                top.className = 'item-top';

                const left = document.createElement('div');
                left.className = 'row';
                const tName = document.createElement('strong');
                tName.textContent = topic.name;
                left.appendChild(tName);
                left.appendChild(makeBadge(topic.public ? 'public (免ACL)' : 'private (需ACL)', topic.public ? 'ok' : 'warn'));
                top.appendChild(left);

                const controls = document.createElement('div');
                controls.className = 'row';
                controls.appendChild(makeBtn(topic.public ? '设为私有' : '设为公开', 'small', async function () {
                    try {
                        await Dambo.topics.configure(topic.name, { public: !topic.public, description: topic.description });
                        showToast('已切换 Topic 可见性');
                        await loadTopicsPanel();
                    } catch (err) {
                        handleApiError(err, '切换失败');
                    }
                }));
                controls.appendChild(makeBtn('删除', 'danger small', async function () {
                    try {
                        await Dambo.topics.remove(topic.name);
                        showToast('已删除 Topic ' + topic.name);
                        await loadTopicsPanel();
                    } catch (err) {
                        handleApiError(err, '删除失败');
                    }
                }));
                top.appendChild(controls);
                box.appendChild(top);

                if (topic.description) {
                    const desc = document.createElement('div');
                    desc.className = 'muted';
                    desc.textContent = topic.description;
                    box.appendChild(desc);
                }
                topicListEl.appendChild(box);
            });
        } catch (err) {
            handleApiError(err, '加载 Topics 失败');
        }
    }

    async function loadBrowserNamespaces() {
        try {
            const namespaces = await currentBrowserHandle().namespaces();
            nsSelectEl.replaceChildren();
            namespaces.forEach(function (ns) {
                const opt = document.createElement('option');
                opt.value = ns;
                opt.textContent = ns;
                nsSelectEl.appendChild(opt);
            });
            const customNs = nsCustomEl.value.trim();
            if (customNs) {
                activeBrowserNs = customNs;
            } else if (namespaces.indexOf(activeBrowserNs) !== -1) {
                nsSelectEl.value = activeBrowserNs;
            } else if (namespaces.length > 0) {
                activeBrowserNs = namespaces[0];
                nsSelectEl.value = activeBrowserNs;
            } else {
                activeBrowserNs = '';
            }
            rebindBrowserWatch(activeBrowserNs);
            await loadBrowserEntries();
        } catch (err) {
            handleApiError(err, '加载命名空间失败');
        }
    }

    async function loadBrowserEntries() {
        const ns = nsCustomEl.value.trim() || nsSelectEl.value || activeBrowserNs;
        activeBrowserNs = ns;
        if (!ns) {
            renderEmpty(kvListEl, '当前存储暂无命名空间');
            return;
        }
        try {
            const prefix = prefixFilterEl.value.trim() || undefined;
            const entries = await currentBrowserHandle().list(ns, { prefix: prefix, withSchema: true });
            const keys = Object.keys(entries || {});
            if (keys.length === 0) {
                renderEmpty(kvListEl, '命名空间 "' + ns + '" 下暂无键值');
                return;
            }
            kvListEl.replaceChildren();
            keys.forEach(function (key) {
                const entry = entries[key];
                const box = document.createElement('div');
                box.className = 'item-box';

                const top = document.createElement('div');
                top.className = 'item-top';
                const left = document.createElement('div');
                left.className = 'row';
                const kCode = document.createElement('code');
                kCode.textContent = key;
                left.appendChild(kCode);
                left.appendChild(makeBadge(entry.type));
                top.appendChild(left);

                top.appendChild(makeBtn('删除', 'danger small', async function () {
                    try {
                        await currentBrowserHandle().delete(ns, key);
                        showToast('已删除 ' + ns + '/' + key);
                        await loadBrowserEntries();
                    } catch (err) {
                        handleApiError(err, '删除失败');
                    }
                }));
                box.appendChild(top);

                const valPre = document.createElement('pre');
                valPre.className = 'code-block';
                valPre.textContent = JSON.stringify(entry.value, null, 2);
                box.appendChild(valPre);
                kvListEl.appendChild(box);
            });
        } catch (err) {
            handleApiError(err, '读取键值失败');
        }
    }

    // =========================================================================
    // 10. 主刷新与事件绑定
    // =========================================================================
    function renderMetaHeader(meta) {
        if (!meta) return;
        currentMeta = meta;
        const displayTitle = meta.name || meta.plugin_name;
        pluginTitleEl.textContent = displayTitle;
        document.title = displayTitle + ' · 管理面板';
        pluginDescEl.textContent = meta.description || pluginDescEl.textContent;
        obsUrlCodeEl.textContent = resolveCanonicalObsUrl(meta);

        pluginBadgesEl.replaceChildren();
        pluginBadgesEl.appendChild(makeBadge('v' + meta.version, 'accent'));
        pluginBadgesEl.appendChild(makeBadge(currentCfg.storage_mode === 'memory' ? '内存模式' : '持久化模式', 'ok'));
        if (meta.obs && (meta.obs.width || meta.obs.height)) {
            pluginBadgesEl.appendChild(makeBadge('OBS ' + (meta.obs.width || '?') + '×' + (meta.obs.height || '?')));
        }
        pluginTechMetaEl.textContent = '目录: ' + meta.plugin_name + ' · 数据域: ' + meta.namedomain + ' · 布局: ' + (currentCfg.layout || 'list') + ' · 主题: ' + (currentCfg.theme || 'glass-dark');
    }

    async function refreshConfigAndData() {
        try {
            const results = await Promise.all([
                Dambo.plugin.meta({ force: true }),
                Dambo.plugins.list().catch(function () { return []; }),
            ]);
            const meta = results[0];
            installedPlugins = Array.isArray(results[1]) ? results[1] : [];

            const cfgSchema = (meta && meta.config) || {};
            const flatCfg = {};
            Object.keys(cfgSchema).forEach(function (k) {
                flatCfg[k] = cfgSchema[k].value;
            });
            currentCfg = flatCfg;

            renderMetaHeader(meta);
            populatePluginDropdowns();
            updateNewSourcePlaceholder();

            await ensureScoreStoreBound();

            const catalog = buildSourceCatalog();
            syncTopicSubscriptions(catalog.active.filter(function (r) { return r.mode === 'topic'; }));
            syncKvSources(catalog.active.filter(function (r) { return r.mode === 'memory_kv' || r.mode === 'persist_kv'; }));

            renderManageRankList();
            renderSourcesPanel();
            renderConfigPanel(meta);
        } catch (err) {
            handleApiError(err, '刷新插件配置失败');
        }
    }

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

    simPushBtnEl.addEventListener('click', async function () {
        try {
            const testNames = ['测试玩家Alpha', '测试玩家Beta', '测试玩家Gamma'];
            const idx = Math.floor(Math.random() * testNames.length);
            const uid = 9001 + idx;
            const uname = testNames[idx];
            const pk = 'uid:' + uid;
            const existing = selfScoresMap.get(pk);
            const curScore = existing && typeof existing === 'object' ? Number(existing.score || 0) : 0;
            const nextVal = {
                player_key: pk,
                uid: uid,
                uname: uname,
                face: '',
                score: curScore + 10,
                tag: '模拟测试',
                updated_at: Date.now(),
            };
            await activeScoreStore.set('scores', pk, nextVal);
            selfScoresMap.set(pk, nextVal);
            renderManageRankList();
            showToast('已模拟为 ' + uname + ' 增加 10 分');
        } catch (err) {
            handleApiError(err, '模拟推送失败');
        }
    });

    toggleIoBtnEl.addEventListener('click', function () {
        ioPanelEl.classList.toggle('hidden');
    });

    exportJsonBtnEl.addEventListener('click', function () {
        const list = aggregateForManage();
        const payload = {
            version: 1,
            storage_mode: currentCfg.storage_mode || 'persist',
            exported_at: new Date().toISOString(),
            items: list.map(function (r) {
                return {
                    rank: r.rank,
                    uid: r.uid,
                    uname: r.uname,
                    score: r.score,
                    face: r.face,
                    tags: Array.from(r.tags || []),
                };
            }),
        };
        ioJsonTextareaEl.value = JSON.stringify(payload, null, 2);
        ioPanelEl.classList.remove('hidden');
        showToast('已导出 ' + list.length + ' 条榜单数据');
    });

    copyJsonBtnEl.addEventListener('click', async function () {
        if (!ioJsonTextareaEl.value.trim()) {
            showToast('请先点击“生成当前榜单 JSON”或输入内容', true);
            return;
        }
        try {
            await navigator.clipboard.writeText(ioJsonTextareaEl.value);
            showToast('已复制 JSON 内容');
        } catch (_) {
            showToast('复制失败，请全选文本框手动复制', true);
        }
    });

    async function runImportJson(replaceAll) {
        let parsed;
        try {
            parsed = JSON.parse(ioJsonTextareaEl.value);
        } catch (_) {
            showToast('JSON 格式不合法，请检查', true);
            return;
        }
        const items = Array.isArray(parsed) ? parsed : (parsed && Array.isArray(parsed.items) ? parsed.items : null);
        if (!items) {
            showToast('JSON 需为数组或包含 items 数组的对象', true);
            return;
        }
        try {
            if (replaceAll) {
                await resetAllScores();
            }
            const batch = {};
            const now = Date.now();
            for (let i = 0; i < items.length; i++) {
                const it = items[i];
                if (!it || typeof it !== 'object') continue;
                const sc = Number(it.score);
                if (!Number.isFinite(sc)) continue;
                const id = parseCanonicalIdentity(it, '');
                const tag = pickFirstText([it.tag, Array.isArray(it.tags) ? it.tags[0] : '', '导入']);
                const entryObj = {
                    player_key: id.playerKey,
                    uid: id.uid,
                    uname: id.uname,
                    face: id.face,
                    score: sc,
                    tag: tag,
                    updated_at: now,
                };
                batch[id.playerKey] = entryObj;
                selfScoresMap.set(id.playerKey, entryObj);
            }
            const keys = Object.keys(batch);
            if (keys.length === 0) {
                showToast('未识别到有效的玩家积分条目', true);
                return;
            }
            for (let offset = 0; offset < keys.length; offset += 200) {
                const sliceMap = {};
                const sliceKeys = keys.slice(offset, offset + 200);
                sliceKeys.forEach(function (k) { sliceMap[k] = batch[k]; });
                await activeScoreStore.mset('scores', sliceMap);
            }
            renderManageRankList();
            showToast('成功导入 ' + keys.length + ' 名玩家积分');
        } catch (err) {
            handleApiError(err, '导入积分失败');
        }
    }

    importMergeBtnEl.addEventListener('click', function () { runImportJson(false); });
    importReplaceBtnEl.addEventListener('click', function () { runImportJson(true); });

    resetAllScoresBtnEl.addEventListener('click', function () {
        if (!resetConfirmArmed) {
            resetConfirmArmed = true;
            resetAllScoresBtnEl.textContent = '再次点击确认清空';
            resetConfirmTimer = setTimeout(function () {
                resetConfirmArmed = false;
                resetAllScoresBtnEl.textContent = '清空全部积分';
            }, 3500);
            return;
        }
        if (resetConfirmTimer) clearTimeout(resetConfirmTimer);
        resetConfirmArmed = false;
        resetAllScoresBtnEl.textContent = '清空全部积分';
        resetAllScores();
    });

    manualScoreFormEl.addEventListener('submit', async function (ev) {
        ev.preventDefault();
        const uname = mUnameEl.value.trim();
        const val = Number(mValEl.value);
        if (!uname || !Number.isFinite(val)) {
            showToast('请填写有效昵称和分值', true);
            return;
        }
        const uidNum = mUidEl.value.trim() ? Math.max(0, Math.floor(Number(mUidEl.value))) : 0;
        const id = parseCanonicalIdentity({
            uid: uidNum,
            uname: uname,
            face: mFaceEl.value.trim(),
        }, '');
        const tag = mTagEl.value.trim() || '手动';
        const op = mOpEl.value;
        const list = aggregateForManage();
        const existingEntry = list.find(function (r) { return r.playerKey === id.playerKey; }) || {
            playerKey: id.playerKey,
            uid: id.uid,
            uname: id.uname,
            face: id.face,
            directScore: 0,
            externalScore: 0,
            directKeys: [],
        };
        if (id.face) existingEntry.face = id.face;

        try {
            if (op === 'set') {
                await adjustPlayerScore(existingEntry, val, true);
            } else {
                const targetKey = (existingEntry.directKeys && existingEntry.directKeys[0]) || id.playerKey;
                const prevDirect = selfScoresMap.get(targetKey);
                const prevNum = prevDirect && typeof prevDirect === 'object'
                    ? Number(pickFirstNumber([prevDirect.score, prevDirect.value]) || 0)
                    : (typeof prevDirect === 'number' ? prevDirect : 0);
                const nextObj = {
                    player_key: id.playerKey,
                    uid: id.uid,
                    uname: id.uname,
                    face: existingEntry.face || id.face || '',
                    score: roundClean(prevNum + val),
                    tag: tag,
                    updated_at: Date.now(),
                };
                await activeScoreStore.set('scores', targetKey, nextObj);
                selfScoresMap.set(targetKey, nextObj);
                renderManageRankList();
                showToast('已为 ' + id.uname + ' 调整 ' + (val >= 0 ? '+' : '') + val);
            }
            mUnameEl.value = '';
            mUidEl.value = '';
            mFaceEl.value = '';
        } catch (err) {
            handleApiError(err, '应用调整失败');
        }
    });

    presetGame24TopicBtnEl.addEventListener('click', function () {
        const cur = Array.isArray(currentCfg.sources) ? currentCfg.sources.slice() : [];
        const existingIdx = cur.findIndex(function (s) {
            return s && s.plugin === 'game24' && s.mode === 'topic' && s.target === 'round';
        });
        if (existingIdx !== -1) {
            cur[existingIdx] = Object.assign({}, cur[existingIdx], { enabled: true });
            saveUpdatedSourcesArray(cur, '已开启 24点 回合流 (game24 / round)');
            return;
        }
        cur.push({
            id: 'game24_round',
            enabled: true,
            plugin: 'game24',
            mode: 'topic',
            target: 'round',
            weight: 10,
            label: '24点',
            score_field: 'score',
            name_field: 'uname',
        });
        saveUpdatedSourcesArray(cur, '已添加 24点 回合流（答对一局默认 +10 分）');
    });

    presetGame24KvBtnEl.addEventListener('click', async function () {
        const cur = Array.isArray(currentCfg.sources) ? currentCfg.sources.slice() : [];
        const existingIdx = cur.findIndex(function (s) {
            return s && s.plugin === 'game24' && s.mode === 'memory_kv' && s.target === 'scores';
        });
        if (existingIdx !== -1) {
            cur[existingIdx] = Object.assign({}, cur[existingIdx], { enabled: true });
        } else {
            cur.push({
                id: 'game24_memory_scores',
                enabled: true,
                plugin: 'game24',
                mode: 'memory_kv',
                target: 'scores',
                weight: 10,
                label: '24点KV',
                score_field: 'score',
                name_field: 'uname',
            });
        }
        await saveUpdatedSourcesArray(cur, '已添加 24点 内存榜数据源');
        try {
            const chk = await Dambo.acl.check('game24', 'memory:scores');
            if (!chk || chk.effective_level === 'none') {
                await Dambo.acl.request('game24', {
                    resource: 'memory:scores',
                    level: 'read',
                    reason: '通用积分榜读取 24点 内存榜单',
                });
                showToast('已向 game24 发送 memory:scores 读取申请，请到 /game24/m 批准');
                loadAclPanel();
            }
        } catch (_) {}
    });

    saveAutoTopicsBtnEl.addEventListener('click', async function () {
        const names = srcAutoNamesInputEl.value
            .split(',')
            .map(function (s) { return s.trim(); })
            .filter(Boolean);
        try {
            await Dambo.config.set({
                auto_discover_topics: srcAutoDiscoverCbEl.checked,
                auto_topic_names: names.length > 0 ? names : ['scoreboard'],
            });
            showToast('已保存自动发现设置');
            await refreshConfigAndData();
        } catch (err) {
            handleApiError(err, '保存自动发现失败');
        }
    });

    newSrcPluginEl.addEventListener('change', updateNewSourcePlaceholder);
    newSrcModeEl.addEventListener('change', updateNewSourcePlaceholder);

    addSourceFormEl.addEventListener('submit', async function (ev) {
        ev.preventDefault();
        const plugin = newSrcPluginEl.value.trim();
        const mode = newSrcModeEl.value;
        const target = newSrcTargetEl.value.trim();
        const weight = Number(newSrcWeightEl.value);
        if (!plugin || !target) {
            showToast('请选择目标插件并填写 Topic 或命名空间', true);
            return;
        }
        if (!Number.isFinite(weight) || weight < -1000 || weight > 10000) {
            showToast('权重必须在 -1000 ~ 10000 之间', true);
            return;
        }
        const rawId = plugin + '_' + mode + '_' + target;
        const cleanId = rawId.replace(/[^A-Za-z0-9_.-]/g, '_').slice(0, 48);
        const cur = Array.isArray(currentCfg.sources) ? currentCfg.sources.slice() : [];
        const uniqueId = cur.some(function (x) { return x && x.id === cleanId; })
            ? (cleanId.slice(0, 38) + '_' + fnv1aHex(String(Date.now())).slice(0, 6))
            : cleanId;

        cur.push({
            id: uniqueId,
            enabled: true,
            plugin: plugin,
            mode: mode,
            target: target,
            weight: weight,
            label: newSrcLabelEl.value.trim(),
            score_field: newSrcScoreFieldEl.value.trim() || 'score',
            name_field: newSrcNameFieldEl.value.trim() || 'uname',
        });
        await saveUpdatedSourcesArray(cur, '已添加数据源规则');
        newSrcLabelEl.value = '';
    });

    saveAllCfgBtnEl.addEventListener('click', async function () {
        const patch = {};
        for (let i = 0; i < dirtyConfigReaders.length; i++) {
            const item = dirtyConfigReaders[i];
            try {
                const val = item.reader.get();
                if (JSON.stringify(val) !== item.initial) {
                    patch[item.key] = val;
                }
            } catch (err) {
                showToast('参数 ' + item.key + ' 格式无效', true);
                return;
            }
        }
        const keys = Object.keys(patch);
        if (keys.length === 0) {
            showToast('没有待保存的修改');
            return;
        }
        try {
            await Dambo.config.set(patch);
            showToast('已批量保存 ' + keys.length + ' 项配置');
            await refreshConfigAndData();
        } catch (err) {
            handleApiError(err, '批量保存失败');
        }
    });

    resetAllCfgBtnEl.addEventListener('click', async function () {
        try {
            await Dambo.config.reset();
            showToast('已恢复全部默认配置');
            await refreshConfigAndData();
        } catch (err) {
            handleApiError(err, '恢复默认失败');
        }
    });

    quickGrantWriteBtnEl.addEventListener('click', async function () {
        const target = grantPluginSelectEl.value;
        const resource = grantResourceSelectEl.value;
        if (!target) {
            showToast('请先选择要授权的插件', true);
            return;
        }
        try {
            await Dambo.acl.set(target, {
                resource: resource,
                level: 'write',
                note: '积分榜快捷授予写入权限',
            });
            showToast('已授予 ' + target + ' 写入 ' + resource + ' 的权限');
            await loadAclPanel();
        } catch (err) {
            handleApiError(err, '授权失败');
        }
    });

    storeTabBtns.forEach(function (btn) {
        btn.addEventListener('click', function () {
            activeBrowserStore = btn.getAttribute('data-store-tab') || 'persist';
            storeTabBtns.forEach(function (b) { b.classList.toggle('active', b === btn); });
            nsCustomEl.value = '';
            loadBrowserNamespaces();
        });
    });

    nsSelectEl.addEventListener('change', function () {
        nsCustomEl.value = '';
        activeBrowserNs = nsSelectEl.value;
        rebindBrowserWatch(activeBrowserNs);
        loadBrowserEntries();
    });

    refreshDataBtnEl.addEventListener('click', function () {
        const c = nsCustomEl.value.trim();
        if (c) activeBrowserNs = c;
        rebindBrowserWatch(activeBrowserNs);
        loadBrowserEntries();
    });

    clearNsBtnEl.addEventListener('click', async function () {
        const ns = nsCustomEl.value.trim() || nsSelectEl.value || activeBrowserNs;
        if (!ns) return;
        try {
            const count = await currentBrowserHandle().clearNamespace(ns);
            showToast('已清空 ' + ns + ' (' + count + ' 条)');
            await loadBrowserNamespaces();
            await refreshConfigAndData();
        } catch (err) {
            handleApiError(err, '清空失败');
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
    // 11. 初始化入口与扫码 #t= 自动解锁
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
        if (!offCfgWatch) {
            offCfgWatch = Dambo.config.watch(function () { refreshConfigAndData(); });
        }
        if (!offAclWatch) {
            offAclWatch = Dambo.acl.onChange(function () {
                loadAclPanel();
                refreshConfigAndData();
            });
        }
        renderProtocolExamples();
        await refreshConfigAndData();
        await Promise.all([
            loadAclPanel(),
            loadTopicsPanel(),
            loadBrowserNamespaces(),
        ]);
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
