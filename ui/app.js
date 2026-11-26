const invoke = window.__TAURI__?.core?.invoke || window.__TAURI__?.invoke;
const listen = window.__TAURI__?.event?.listen;

let currentRoomState = {
    room_id: 0,
    room_uname: "",
    room_title: "",
    is_live: false,
    connected: false,
};

let currentAuthState = {
    is_login: false,
    mid: 0,
    uname: "",
};

let currentLocale = "en";
let i18nMessages = {};
let pluginList = [];
let currentHistoryRooms = [];
let lastRoomOptions = null;
let recordRawEnabled = false;
let activeRoomLabel = "";
let pendingRoomDraft = null;
let skipNextRoomInputMouseUp = false;
let suppressRoomBlurRevert = false;
let isConnecting = false;
let connectingTimeoutId = null;
let isAlwaysOnTop = true;
let toastTimer = null;
let pluginViewMode = "list";
let qrTarget = "__admin__";
let qrPluginPage = "view";
let qrSelectedIp = "";
let lastQrPayload = null;
let qrFetchSeq = 0;

function t(key, vars) {
    let text = i18nMessages[key] || key;
    if (vars && typeof vars === 'object') {
        Object.entries(vars).forEach(([k, v]) => {
            text = text.replaceAll(`{${k}}`, String(v));
        });
    }
    return text;
}

function showToast(text) {
    const el = document.getElementById('toast-banner');
    if (!el || !text) return;
    el.innerText = text;
    el.classList.remove('hidden');
    if (toastTimer !== null) {
        clearTimeout(toastTimer);
    }
    toastTimer = setTimeout(() => {
        el.classList.add('hidden');
        toastTimer = null;
    }, 1800);
}

const PANEL_BASE_MIN_WIDTH = 520;
const PANEL_MAX_WIDTH = 860;
const PANEL_COLLAPSED_HEIGHT = 56;
const INPUT_BAR_LOGICAL_HEIGHT = 52;
const PANEL_EXPANDED_HEIGHT = 420;
const DANMAKU_COOLDOWN_MS = 1200;
const MIN_ROOM_PICKER_WIDTH = 136;
const IS_MACOS = /mac/i.test(navigator.platform || navigator.userAgent || '');
let lastSyncedPanelMinWidth = PANEL_BASE_MIN_WIDTH;
let minWidthSyncRaf = null;
let baseLogicalHeight = null;
let baseLogicalHeightPromise = null;
let heightSyncSeq = 0;
let danmakuBarOpSeq = 0;
let isDanmakuBarOpen = false;
let isSendingDanmaku = false;
let danmakuCooldownUntil = 0;
let danmakuCooldownTimer = null;

function sumOuterWidths(elements) {
    return elements.reduce((sum, el) => {
        return el ? sum + el.getBoundingClientRect().width : sum;
    }, 0);
}

function horizontalBoxChrome(el) {
    if (!el) return 0;
    const cs = window.getComputedStyle(el);
    return (
        parseFloat(cs.paddingLeft || '0') +
        parseFloat(cs.paddingRight || '0') +
        parseFloat(cs.borderLeftWidth || '0') +
        parseFloat(cs.borderRightWidth || '0')
    );
}

function computeRequiredMinPanelWidth() {
    const pod = document.querySelector('.pod-container');
    const podContent = document.querySelector('.pod-content');
    const dragHandle = document.getElementById('drag-handle');
    const accountBadge = document.getElementById('account-badge');
    const roomSwitchBtn = document.getElementById('room-switch-btn');
    const danmakuToggleBtn = document.getElementById('danmaku-toggle-btn');
    const consoleOpenBtn = document.getElementById('console-open-btn');
    const pluginGroup = document.querySelector('.plugin-menu-group');
    const moreGroup = document.querySelector('.more-menu-group');
    const winControls = document.querySelector('.win-controls');
    const resizeHandleRight = document.getElementById('resize-handle-right');
    const dividers = Array.from(document.querySelectorAll('.pod-content > .divider'));
    const roomSelectorGroup = document.querySelector('.room-selector-group');
    if (!pod || !podContent || !accountBadge) return PANEL_BASE_MIN_WIDTH;

    const fixedWidth = sumOuterWidths([
        dragHandle,
        accountBadge,
        ...dividers,
        roomSwitchBtn,
        danmakuToggleBtn,
        consoleOpenBtn,
        pluginGroup,
        moreGroup,
        winControls,
        resizeHandleRight,
    ]);

    const podStyle = window.getComputedStyle(pod);
    const podContentStyle = window.getComputedStyle(podContent);
    const roomGroupStyle = roomSelectorGroup ? window.getComputedStyle(roomSelectorGroup) : null;
    const podGap = parseFloat(podStyle.columnGap || podStyle.gap || '8') || 0;
    const contentGap = parseFloat(podContentStyle.columnGap || podContentStyle.gap || '6') || 0;
    const roomGap = roomGroupStyle
        ? parseFloat(roomGroupStyle.columnGap || roomGroupStyle.gap || '6') || 0
        : 6;
    const podChildCount = pod.children.length;
    const contentChildCount = podContent.children.length;
    const totalGaps =
        Math.max(0, podChildCount - 1) * podGap +
        Math.max(0, contentChildCount - 1) * contentGap +
        roomGap;
    const totalChrome = horizontalBoxChrome(document.body) + horizontalBoxChrome(pod);

    const required = Math.ceil(fixedWidth + totalGaps + totalChrome + MIN_ROOM_PICKER_WIDTH + 6);
    return Math.max(PANEL_BASE_MIN_WIDTH, required);
}

function scheduleSyncPanelMinWidth() {
    if (!invoke) return;
    if (minWidthSyncRaf !== null) {
        cancelAnimationFrame(minWidthSyncRaf);
    }
    minWidthSyncRaf = requestAnimationFrame(() => {
        minWidthSyncRaf = null;
        const required = computeRequiredMinPanelWidth();
        if (required !== lastSyncedPanelMinWidth) {
            lastSyncedPanelMinWidth = required;
            invoke('sync_panel_min_width', { minWidth: required }).catch(console.error);
        }
    });
}

function applyTranslations(payload) {
    if (payload && payload.locale) {
        currentLocale = payload.locale;
    }
    if (payload && payload.messages) {
        i18nMessages = payload.messages;
    }

    document.documentElement.lang = currentLocale;
    document.querySelectorAll('[data-i18n]').forEach((el) => {
        const key = el.getAttribute('data-i18n');
        if (key) {
            el.innerText = t(key);
        }
    });
    document.querySelectorAll('[data-i18n-placeholder]').forEach((el) => {
        const key = el.getAttribute('data-i18n-placeholder');
        if (key) {
            el.placeholder = t(key);
        }
    });
    document.querySelectorAll('[data-i18n-title]').forEach((el) => {
        const key = el.getAttribute('data-i18n-title');
        if (key) {
            el.title = t(key);
        }
    });

    updateAccountDisplay(currentAuthState);
    updateRoomDisplay(currentRoomState, { fromBackendEvent: false });
    renderPlugins(pluginList);
    syncQrViewControls();
    if (lastQrPayload) {
        renderQrPayload(lastQrPayload);
    }
    updateLanguageButtons(currentLocale);
    syncPinUiState();
    if (lastRoomOptions) {
        renderRoomOptions({ ...lastRoomOptions, history_rooms: currentHistoryRooms });
    } else {
        renderHistoryList(currentHistoryRooms);
    }
    scheduleSyncPanelMinWidth();
}

function formatRoomLabel(state) {
    if (!state || state.room_id <= 0) return "";
    const head = state.room_uname ? `${state.room_id}(${state.room_uname})` : `${state.room_id}`;
    return state.room_title ? `${head}: ${state.room_title}` : head;
}

function setConnectingState(connecting) {
    if (connectingTimeoutId !== null) {
        clearTimeout(connectingTimeoutId);
        connectingTimeoutId = null;
    }
    isConnecting = Boolean(connecting);
    if (isConnecting) {
        connectingTimeoutId = setTimeout(() => {
            connectingTimeoutId = null;
            if (isConnecting) {
                isConnecting = false;
                syncRoomActionButton();
            }
        }, 15000);
    }
    syncRoomActionButton();
}

function syncRoomActionButton() {
    const roomInput = document.getElementById('room-input');
    const roomSwitchBtn = document.getElementById('room-switch-btn');
    if (!roomSwitchBtn) return;

    if (isConnecting) {
        roomSwitchBtn.innerText = t('panel.connecting');
        roomSwitchBtn.disabled = true;
        roomSwitchBtn.classList.add('connecting');
        scheduleSyncPanelMinWidth();
        return;
    }

    roomSwitchBtn.disabled = false;
    roomSwitchBtn.classList.remove('connecting');

    const draft = (roomInput?.value || '').trim();
    if (!currentRoomState.connected) {
        roomSwitchBtn.innerText = t('panel.connect');
    } else if (draft && draft !== activeRoomLabel && draft !== String(currentRoomState.room_id)) {
        roomSwitchBtn.innerText = t('panel.switch');
    } else {
        roomSwitchBtn.innerText = t('panel.disconnect');
    }
    scheduleSyncPanelMinWidth();
}

function updateRoomDisplay(state, { fromBackendEvent = true } = {}) {
    const prevRoomId = currentRoomState.room_id;
    const prevConnected = currentRoomState.connected;
    currentRoomState = state;
    if (state.room_id !== prevRoomId || state.connected !== prevConnected) {
        pendingRoomDraft = null;
    }
    if (fromBackendEvent && isConnecting && (state.connected || state.room_id === 0)) {
        setConnectingState(false);
    }

    const roomDot = document.getElementById('room-dot');
    const roomInput = document.getElementById('room-input');

    roomDot.className = 'status-dot';
    if (!state.connected) {
        roomDot.classList.add('dot-off');
    } else if (!state.is_live) {
        roomDot.classList.add('dot-yellow');
    } else {
        roomDot.classList.add('dot-green');
    }

    activeRoomLabel = formatRoomLabel(state);
    roomInput.title = activeRoomLabel;
    if (document.activeElement !== roomInput && pendingRoomDraft === null) {
        roomInput.value = activeRoomLabel;
    }
    syncRoomActionButton();
    syncDanmakuUiState();
}

function updateAccountDisplay(auth) {
    currentAuthState = auth || { is_login: false, mid: 0, uname: "" };
    const accountDot = document.getElementById('account-dot');
    const accountName = document.getElementById('account-name');

    accountDot.className = 'status-dot';
    if (currentAuthState.is_login) {
        accountDot.classList.add('dot-green');
        accountName.innerText = currentAuthState.uname
            ? `@${currentAuthState.uname}`
            : `@${currentAuthState.mid}`;
    } else {
        accountDot.classList.add('dot-gray');
        accountName.innerText = t('panel.sign_in');
    }

    syncDanmakuUiState();
    scheduleSyncPanelMinWidth();
}

function updateRecordRawDisplay(enabled) {
    recordRawEnabled = Boolean(enabled);
    const check = document.getElementById('more-record-raw-check');
    if (check) {
        check.innerText = recordRawEnabled ? '✓' : '';
    }
}

function createSvgIcon(size, specs) {
    const ns = 'http://www.w3.org/2000/svg';
    const svg = document.createElementNS(ns, 'svg');
    svg.setAttribute('width', String(size));
    svg.setAttribute('height', String(size));
    svg.setAttribute('viewBox', '0 0 24 24');
    svg.setAttribute('fill', 'none');
    svg.setAttribute('stroke', 'currentColor');
    svg.setAttribute('stroke-width', '2');
    svg.setAttribute('stroke-linecap', 'round');
    svg.setAttribute('stroke-linejoin', 'round');
    svg.setAttribute('aria-hidden', 'true');
    specs.forEach(([tag, attrs]) => {
        const node = document.createElementNS(ns, tag);
        Object.entries(attrs).forEach(([k, v]) => node.setAttribute(k, String(v)));
        svg.appendChild(node);
    });
    return svg;
}

function renderPlugins(plugins) {
    pluginList = Array.isArray(plugins) ? plugins : [];
    const triggerLabel = document.getElementById('plugin-trigger-label');
    if (triggerLabel) {
        triggerLabel.innerText = t('plugin.trigger_label', { count: pluginList.length });
    }

    const listEl = document.getElementById('plugin-cards-list');
    if (!listEl) return;
    listEl.replaceChildren();

    if (pluginList.length === 0) {
        const empty = document.createElement('div');
        empty.className = 'plugin-empty';
        empty.innerText = t('plugin.empty');
        listEl.appendChild(empty);
    } else {
        pluginList.forEach((p) => {
            const isDesktopOpen = Boolean(p.is_desktop_open);
            const row = document.createElement('div');
            row.className = `plugin-row${isDesktopOpen ? ' desktop-open' : ''}`;
            const tooltipParts = [p.description || '', p.url || ''].filter(Boolean);
            if (tooltipParts.length > 0) {
                row.title = tooltipParts.join('\n');
            }

            const main = document.createElement('div');
            main.className = 'plugin-row-main';

            const dot = document.createElement('span');
            dot.className = `status-dot ${isDesktopOpen ? 'dot-green' : 'dot-off'}`;
            main.appendChild(dot);

            const title = document.createElement('span');
            title.className = 'plugin-row-title';
            title.innerText = p.title || p.name;
            main.appendChild(title);

            const actions = document.createElement('div');
            actions.className = 'plugin-row-actions';

            const toggleDesktopBtn = document.createElement('button');
            toggleDesktopBtn.type = 'button';
            toggleDesktopBtn.className = `plugin-btn ${isDesktopOpen ? 'danger' : 'primary'}`;
            toggleDesktopBtn.innerText = t(isDesktopOpen ? 'plugin.close_short' : 'plugin.desktop_short');
            toggleDesktopBtn.title = t(isDesktopOpen ? 'plugin.close_desktop' : 'plugin.open_desktop');
            toggleDesktopBtn.onclick = async (e) => {
                e.stopPropagation();
                if (!invoke) return;
                try {
                    if (isDesktopOpen) {
                        await invoke('close_plugin_desktop', { pluginName: p.name });
                        showToast(t('toast.closed_desktop', { name: p.title || p.name }));
                    } else {
                        await invoke('open_plugin_desktop', { pluginName: p.name });
                        showToast(t('toast.opened_desktop', { name: p.title || p.name }));
                    }
                    const fresh = await invoke('get_plugins');
                    renderPlugins(fresh);
                } catch (err) {
                    console.error('toggle_plugin_desktop failed:', err);
                    showToast(t('toast.error_prefix', { error: String(err) }));
                }
            };

            const copyBtn = document.createElement('button');
            copyBtn.type = 'button';
            copyBtn.className = 'plugin-btn';
            copyBtn.innerText = 'OBS';
            copyBtn.title = t('plugin.copy_url');
            copyBtn.onclick = async (e) => {
                e.stopPropagation();
                try {
                    await navigator.clipboard.writeText(p.url);
                    copyBtn.classList.add('copied');
                    copyBtn.innerText = '✓';
                    setTimeout(() => {
                        copyBtn.classList.remove('copied');
                        copyBtn.innerText = 'OBS';
                    }, 1500);
                } catch (err) {
                    console.error('Failed to copy plugin url:', err);
                    showToast(t('toast.error_prefix', { error: String(err) }));
                }
            };

            const manageBtn = document.createElement('button');
            manageBtn.type = 'button';
            manageBtn.className = 'plugin-icon-btn';
            manageBtn.title = t('plugin.open_manage');
            manageBtn.setAttribute('aria-label', t('plugin.open_manage'));
            manageBtn.appendChild(
                createSvgIcon(12, [
                    ['line', { x1: '4', y1: '6', x2: '20', y2: '6' }],
                    ['line', { x1: '4', y1: '12', x2: '20', y2: '12' }],
                    ['line', { x1: '4', y1: '18', x2: '20', y2: '18' }],
                    ['circle', { cx: '9', cy: '6', r: '2', fill: 'currentColor' }],
                    ['circle', { cx: '15', cy: '12', r: '2', fill: 'currentColor' }],
                    ['circle', { cx: '9', cy: '18', r: '2', fill: 'currentColor' }],
                ])
            );
            manageBtn.onclick = async (e) => {
                e.stopPropagation();
                if (!invoke) return;
                try {
                    await invoke('open_plugin_manage', { pluginName: p.name });
                    showToast(t('toast.opened_manage', { name: p.title || p.name }));
                } catch (err) {
                    console.error('open_plugin_manage failed:', err);
                    showToast(t('toast.error_prefix', { error: String(err) }));
                }
            };

            const qrBtn = document.createElement('button');
            qrBtn.type = 'button';
            qrBtn.className = 'plugin-icon-btn';
            qrBtn.title = t('plugin.qr_btn_title');
            qrBtn.setAttribute('aria-label', t('plugin.qr_btn_title'));
            qrBtn.appendChild(
                createSvgIcon(12, [
                    ['rect', { x: '3', y: '3', width: '7', height: '7', rx: '1' }],
                    ['rect', { x: '14', y: '3', width: '7', height: '7', rx: '1' }],
                    ['rect', { x: '3', y: '14', width: '7', height: '7', rx: '1' }],
                    ['path', { d: 'M14 14h3v3h-3zM20 14v3h-3M14 20h6' }],
                ])
            );
            qrBtn.onclick = (e) => {
                e.stopPropagation();
                openPluginQrView(p.name, 'view');
            };

            actions.appendChild(toggleDesktopBtn);
            actions.appendChild(copyBtn);
            actions.appendChild(manageBtn);
            actions.appendChild(qrBtn);

            row.appendChild(main);
            row.appendChild(actions);
            listEl.appendChild(row);
        });
    }

    if (
        pluginViewMode === 'qr' &&
        qrTarget !== '__admin__' &&
        !pluginList.some((p) => p.name === qrTarget)
    ) {
        setPluginViewMode('list');
    } else {
        syncQrViewControls();
    }
    scheduleSyncPanelMinWidth();
}

function resolveQrRequest() {
    if (qrTarget !== '__admin__') {
        const matched = pluginList.find((p) => p.name === qrTarget);
        if (matched) {
            const title = matched.title || matched.name;
            if (qrPluginPage === 'manage') {
                return {
                    path: `/${matched.name}/m`,
                    includeToken: true,
                    title,
                };
            }
            return {
                path: `/${matched.name}/`,
                includeToken: false,
                title,
            };
        }
    }
    return {
        path: '/',
        includeToken: true,
        title: t('plugin.qr_target_admin'),
    };
}

function syncQrViewControls() {
    const req = resolveQrRequest();
    const titleEl = document.getElementById('plugin-qr-title');
    if (titleEl) {
        titleEl.innerText = req.title;
        titleEl.title = `${req.title} (${req.path})`;
    }

    const pageRow = document.getElementById('plugin-qr-page-row');
    if (pageRow) {
        pageRow.classList.toggle('hidden', qrTarget === '__admin__');
    }

    document.getElementById('qr-mode-view-btn')?.classList.toggle('active', qrPluginPage !== 'manage');
    document.getElementById('qr-mode-manage-btn')?.classList.toggle('active', qrPluginPage === 'manage');
}

function setPluginViewMode(mode) {
    pluginViewMode = mode === 'qr' ? 'qr' : 'list';
    const isQr = pluginViewMode === 'qr';
    document.getElementById('plugin-list-view')?.classList.toggle('hidden', isQr);
    document.getElementById('plugin-qr-panel')?.classList.toggle('hidden', !isQr);
}

function openPluginQrView(target, page = 'view') {
    qrTarget = target || '__admin__';
    qrPluginPage = page === 'manage' ? 'manage' : 'view';
    syncQrViewControls();
    setPluginViewMode('qr');
    fetchPluginQrUrls();
}

function renderQrPayload(payload) {
    if (!payload) return;
    lastQrPayload = payload;
    syncQrViewControls();

    const modeBadge = document.getElementById('plugin-qr-mode-badge');
    const ipTabsEl = document.getElementById('plugin-qr-ip-tabs');
    const urlTextEl = document.getElementById('plugin-qr-url-text');
    const localNoteEl = document.getElementById('plugin-qr-local-note');
    const qrImgEl = document.getElementById('plugin-qr-img');
    const copyBtn = document.getElementById('plugin-qr-copy-btn');

    const isOpen = payload.security_mode === 'open';
    if (modeBadge) {
        modeBadge.innerText = t(isOpen ? 'lan.mode_open' : 'lan.mode_strict');
        modeBadge.classList.toggle('open', isOpen);
    }

    const urls = Array.isArray(payload.urls) ? payload.urls : [];
    if (payload.local_only || urls.length === 0) {
        const displayUrl = payload.local_url || '';
        if (ipTabsEl) {
            ipTabsEl.replaceChildren();
            ipTabsEl.classList.add('hidden');
        }
        if (urlTextEl) urlTextEl.innerText = displayUrl;
        if (copyBtn) copyBtn.dataset.copyUrl = displayUrl;
        if (localNoteEl) {
            localNoteEl.innerText = t('lan.local_only');
            localNoteEl.classList.remove('hidden');
        }
        if (qrImgEl) qrImgEl.classList.add('hidden');
        return;
    }

    if (!urls.some((u) => u.ip === qrSelectedIp)) {
        qrSelectedIp = urls[0].ip;
    }
    const activeEntry = urls.find((u) => u.ip === qrSelectedIp) || urls[0];

    if (localNoteEl) localNoteEl.classList.add('hidden');
    if (urlTextEl) urlTextEl.innerText = activeEntry.url || '';
    if (copyBtn) copyBtn.dataset.copyUrl = activeEntry.url || '';
    if (qrImgEl) {
        if (activeEntry.qr_svg) {
            qrImgEl.src = 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(activeEntry.qr_svg);
            qrImgEl.alt = t('lan.qr_alt');
            qrImgEl.classList.remove('hidden');
        } else {
            qrImgEl.classList.add('hidden');
        }
    }
    if (ipTabsEl) {
        ipTabsEl.replaceChildren();
        if (urls.length > 1) {
            ipTabsEl.classList.remove('hidden');
            urls.forEach((u) => {
                const btn = document.createElement('button');
                btn.type = 'button';
                btn.className = `plugin-btn lan-ip-btn${u.ip === qrSelectedIp ? ' active' : ''}`;
                btn.innerText = u.ip;
                btn.onclick = (e) => {
                    e.stopPropagation();
                    qrSelectedIp = u.ip;
                    renderQrPayload(lastQrPayload);
                };
                ipTabsEl.appendChild(btn);
            });
        } else {
            ipTabsEl.classList.add('hidden');
        }
    }
}

async function fetchPluginQrUrls() {
    if (!invoke) return;
    const seq = ++qrFetchSeq;
    const { path, includeToken } = resolveQrRequest();
    try {
        const payload = await invoke('get_lan_urls', { path, includeToken });
        if (seq !== qrFetchSeq) return;
        renderQrPayload(payload);
    } catch (e) {
        if (seq !== qrFetchSeq) return;
        console.error('Failed to fetch LAN QR URLs:', e);
        showToast(t('toast.error_prefix', { error: String(e) }));
    }
}

function renderPendingAclBadge(count) {
    const pendingCount = Number.isFinite(Number(count)) ? Math.max(0, Number(count)) : 0;
    const badgeEl = document.getElementById('console-pending-badge');
    if (badgeEl) {
        badgeEl.innerText = String(pendingCount);
        badgeEl.classList.toggle('hidden', pendingCount === 0);
    }
    scheduleSyncPanelMinWidth();
}

async function fetchPendingAclCount() {
    if (!invoke) return;
    try {
        const count = await invoke('get_pending_acl_count');
        renderPendingAclBadge(count);
    } catch (e) {
        console.error('Failed to fetch pending ACL count:', e);
    }
}

function syncPinUiState() {
    const pinBtn = document.getElementById('pin-btn');
    if (!pinBtn) return;
    pinBtn.classList.toggle('active', isAlwaysOnTop);
    pinBtn.setAttribute('aria-pressed', String(isAlwaysOnTop));
    pinBtn.title = t(isAlwaysOnTop ? 'panel.unpin_title' : 'panel.pin_title');
}

function setLangSubmenuOpen(open) {
    const submenu = document.getElementById('more-lang-submenu');
    const trigger = document.getElementById('more-lang-trigger');
    const isOpen = Boolean(open);
    submenu?.classList.toggle('hidden', !isOpen);
    trigger?.classList.toggle('open', isOpen);
    trigger?.setAttribute('aria-expanded', String(isOpen));
}

function updateLanguageButtons(locale) {
    const normalized = String(locale || currentLocale || 'en').trim();
    const currentLabelEl = document.getElementById('more-lang-current');
    let activeLabel = '';
    document.querySelectorAll('#more-lang-submenu .more-lang-option').forEach((btn) => {
        const target = btn.getAttribute('data-locale') || '';
        const active = target === normalized;
        btn.classList.toggle('active', active);
        btn.setAttribute('aria-checked', String(active));
        const checkEl = btn.querySelector('.menu-check');
        if (checkEl) {
            checkEl.innerText = active ? '✓' : '';
        }
        if (active) {
            activeLabel = btn.querySelector('.menu-item-label')?.innerText.trim() || normalized;
        }
    });
    if (currentLabelEl && activeLabel) {
        currentLabelEl.innerText = activeLabel;
    }
}

let lastSyncedPanelHeight = null;

function canSendDanmaku() {
    return Boolean(
        currentAuthState.is_login &&
        currentRoomState.connected &&
        currentRoomState.room_id > 0
    );
}

function isAnyMenuOpen() {
    const roomMenu = document.getElementById('room-dropdown-menu');
    const pluginMenu = document.getElementById('plugin-dropdown-menu');
    const moreMenu = document.getElementById('more-dropdown-menu');
    return Boolean(
        (roomMenu && !roomMenu.classList.contains('hidden')) ||
        (pluginMenu && !pluginMenu.classList.contains('hidden')) ||
        (moreMenu && !moreMenu.classList.contains('hidden'))
    );
}

function computePodMinLogicalHeight() {
    const pod = document.querySelector('.pod-container');
    const podHeight = pod ? Math.ceil(pod.getBoundingClientRect().height) : 48;
    const bodyStyle = document.body ? window.getComputedStyle(document.body) : null;
    const padTop = bodyStyle ? parseFloat(bodyStyle.paddingTop) || 0 : 4;
    const padBottom = bodyStyle ? parseFloat(bodyStyle.paddingBottom) || 0 : 4;
    return Math.max(PANEL_COLLAPSED_HEIGHT, Math.ceil(podHeight + padTop + padBottom));
}

function nextAnimationFrame() {
    return new Promise((resolve) => requestAnimationFrame(() => resolve()));
}

async function waitForViewportHeightAtLeast(minLogicalHeight, maxFrames = 6) {
    for (let i = 0; i < maxFrames; i++) {
        await nextAnimationFrame();
        if (window.innerHeight >= minLogicalHeight - 2) {
            return;
        }
    }
}

async function ensureBaseLogicalHeight() {
    if (baseLogicalHeight !== null) return baseLogicalHeight;
    if (baseLogicalHeightPromise) return baseLogicalHeightPromise;

    baseLogicalHeightPromise = (async () => {
        const cssFloor = computePodMinLogicalHeight();
        let measured = PANEL_COLLAPSED_HEIGHT;
        const canMeasureCollapsed = !isAnyMenuOpen() && !isDanmakuBarOpen;
        const cur = window.__TAURI__?.window?.getCurrentWindow?.();
        if (
            canMeasureCollapsed &&
            cur &&
            typeof cur.scaleFactor === 'function' &&
            typeof cur.innerSize === 'function'
        ) {
            try {
                const [factor, inner] = await Promise.all([cur.scaleFactor(), cur.innerSize()]);
                if (
                    canMeasureCollapsed &&
                    !isAnyMenuOpen() &&
                    !isDanmakuBarOpen &&
                    typeof factor === 'number' &&
                    factor > 0 &&
                    inner &&
                    typeof inner.height === 'number' &&
                    Number.isFinite(inner.height) &&
                    inner.height > 0
                ) {
                    const logicalH = Math.round(inner.height / factor);
                    if (logicalH < PANEL_COLLAPSED_HEIGHT + INPUT_BAR_LOGICAL_HEIGHT) {
                        measured = logicalH;
                    }
                }
            } catch (err) {
                console.warn('Failed to measure initial panel base height:', err);
            }
        }
        baseLogicalHeight = Math.max(PANEL_COLLAPSED_HEIGHT, cssFloor, measured);
        return baseLogicalHeight;
    })();

    try {
        return await baseLogicalHeightPromise;
    } finally {
        baseLogicalHeightPromise = null;
    }
}

async function readCurrentLogicalWidth() {
    const cur = window.__TAURI__?.window?.getCurrentWindow?.();
    if (cur && typeof cur.scaleFactor === 'function' && typeof cur.innerSize === 'function') {
        try {
            const [factor, inner] = await Promise.all([cur.scaleFactor(), cur.innerSize()]);
            if (
                typeof factor === 'number' &&
                factor > 0 &&
                inner &&
                typeof inner.width === 'number' &&
                Number.isFinite(inner.width) &&
                inner.width > 0
            ) {
                return Math.round(inner.width / factor);
            }
        } catch (err) {
            console.warn('Failed to read current logical width:', err);
        }
    }
    return Number.isFinite(window.innerWidth) && window.innerWidth > 0
        ? Math.round(window.innerWidth)
        : null;
}

function computeTargetPanelHeight() {
    const baseH = baseLogicalHeight ?? computePodMinLogicalHeight();
    if (isAnyMenuOpen()) {
        return Math.max(PANEL_EXPANDED_HEIGHT, baseH);
    }
    if (isDanmakuBarOpen) {
        return baseH + INPUT_BAR_LOGICAL_HEIGHT;
    }
    return baseH;
}

async function syncWindowHeight() {
    const opId = ++heightSyncSeq;
    const baseH = await ensureBaseLogicalHeight();
    const currentLogicalWidth = await readCurrentLogicalWidth();
    if (opId !== heightSyncSeq) return;

    const targetHeight = computeTargetPanelHeight();
    if (targetHeight === lastSyncedPanelHeight) return;
    lastSyncedPanelHeight = targetHeight;
    if (invoke) {
        try {
            const args = {
                height: targetHeight,
                baseHeight: baseH,
            };
            if (currentLogicalWidth !== null) {
                args.width = currentLogicalWidth;
            }
            await invoke('set_panel_height', args);
        } catch (err) {
            if (opId === heightSyncSeq) {
                lastSyncedPanelHeight = null;
            }
            console.error(err);
        }
    }
}

function syncDanmakuSendButton() {
    const input = document.getElementById('danmaku-input');
    const sendBtn = document.getElementById('danmaku-send-btn');
    if (!sendBtn) return;

    const inCooldown = Date.now() < danmakuCooldownUntil;
    const hasText = Boolean((input?.value || '').trim());
    sendBtn.innerText = isSendingDanmaku ? t('danmaku.sending') : t('danmaku.send');
    sendBtn.disabled = isSendingDanmaku || !canSendDanmaku() || !hasText || inCooldown;
}

function syncDanmakuUiState() {
    const toggleBtn = document.getElementById('danmaku-toggle-btn');
    const eligible = canSendDanmaku();

    if (toggleBtn) {
        toggleBtn.disabled = !eligible;
        toggleBtn.classList.toggle('active', eligible && isDanmakuBarOpen);
        toggleBtn.setAttribute('aria-expanded', String(eligible && isDanmakuBarOpen));

        if (eligible) {
            toggleBtn.title = t('danmaku.toggle_title');
        } else if (!currentAuthState.is_login && (!currentRoomState.connected || currentRoomState.room_id <= 0)) {
            toggleBtn.title = t('danmaku.disabled_both');
        } else if (!currentAuthState.is_login) {
            toggleBtn.title = t('danmaku.disabled_not_logged_in');
        } else {
            toggleBtn.title = t('danmaku.disabled_not_connected');
        }
    }

    if (!eligible && isDanmakuBarOpen) {
        setDanmakuBarOpen(false);
        return;
    }

    syncDanmakuSendButton();
}

function hideAllDropdownMenusOnly() {
    document.getElementById('room-dropdown-menu')?.classList.add('hidden');
    setPluginViewMode('list');
    document.getElementById('plugin-dropdown-menu')?.classList.add('hidden');
    document.getElementById('plugin-dropdown-btn')?.classList.remove('active');
    document.getElementById('plugin-dropdown-btn')?.setAttribute('aria-expanded', 'false');
    const moreMenu = document.getElementById('more-dropdown-menu');
    const moreBtn = document.getElementById('more-btn');
    setLangSubmenuOpen(false);
    moreMenu?.classList.add('hidden');
    moreBtn?.classList.remove('active');
    moreBtn?.setAttribute('aria-expanded', 'false');
}

async function setDanmakuBarOpen(open) {
    const bar = document.getElementById('danmaku-bar');
    const input = document.getElementById('danmaku-input');
    if (!bar) return;

    const opSeq = ++danmakuBarOpSeq;

    if (open) {
        if (!canSendDanmaku()) return;
        hideAllDropdownMenusOnly();
        isDanmakuBarOpen = true;
        syncDanmakuUiState();

        const baseH = await ensureBaseLogicalHeight();
        if (opSeq !== danmakuBarOpSeq || !isDanmakuBarOpen || !canSendDanmaku()) return;

        await syncWindowHeight();
        if (opSeq !== danmakuBarOpSeq || !isDanmakuBarOpen || !canSendDanmaku()) return;

        await waitForViewportHeightAtLeast(baseH + INPUT_BAR_LOGICAL_HEIGHT);
        if (opSeq !== danmakuBarOpSeq || !isDanmakuBarOpen || !canSendDanmaku()) return;

        bar.classList.remove('hidden');
        await nextAnimationFrame();
        if (opSeq !== danmakuBarOpSeq || !isDanmakuBarOpen || !canSendDanmaku()) {
            bar.classList.add('hidden');
            return;
        }

        document.documentElement.scrollTop = 0;
        if (document.body) document.body.scrollTop = 0;
        input?.focus({ preventScroll: true });
        document.documentElement.scrollTop = 0;
        if (document.body) document.body.scrollTop = 0;
    } else {
        bar.classList.add('hidden');
        input?.blur();
        isDanmakuBarOpen = false;
        document.documentElement.scrollTop = 0;
        if (document.body) document.body.scrollTop = 0;
        syncDanmakuUiState();

        await nextAnimationFrame();
        if (opSeq !== danmakuBarOpSeq) return;

        await syncWindowHeight();
    }
}

function openSingleDropdownMenu(target) {
    danmakuBarOpSeq++;
    if (isDanmakuBarOpen) {
        document.getElementById('danmaku-bar')?.classList.add('hidden');
        isDanmakuBarOpen = false;
        syncDanmakuUiState();
    }
    hideAllDropdownMenusOnly();
    if (target === 'room') {
        document.getElementById('room-dropdown-menu')?.classList.remove('hidden');
    } else if (target === 'plugin') {
        document.getElementById('plugin-dropdown-menu')?.classList.remove('hidden');
        document.getElementById('plugin-dropdown-btn')?.classList.add('active');
        document.getElementById('plugin-dropdown-btn')?.setAttribute('aria-expanded', 'true');
    } else if (target === 'more') {
        document.getElementById('more-dropdown-menu')?.classList.remove('hidden');
        document.getElementById('more-btn')?.classList.add('active');
        document.getElementById('more-btn')?.setAttribute('aria-expanded', 'true');
    }
}

function setDropdownOpen(open) {
    if (open) {
        openSingleDropdownMenu('room');
    } else {
        document.getElementById('room-dropdown-menu')?.classList.add('hidden');
    }
    syncWindowHeight();
}

function setPluginMenuOpen(open) {
    if (open) {
        openSingleDropdownMenu('plugin');
    } else {
        setPluginViewMode('list');
        document.getElementById('plugin-dropdown-menu')?.classList.add('hidden');
        document.getElementById('plugin-dropdown-btn')?.classList.remove('active');
        document.getElementById('plugin-dropdown-btn')?.setAttribute('aria-expanded', 'false');
    }
    syncWindowHeight();
}

function setMoreMenuOpen(open) {
    if (open) {
        openSingleDropdownMenu('more');
    } else {
        setLangSubmenuOpen(false);
        document.getElementById('more-dropdown-menu')?.classList.add('hidden');
        document.getElementById('more-btn')?.classList.remove('active');
        document.getElementById('more-btn')?.setAttribute('aria-expanded', 'false');
    }
    syncWindowHeight();
}

function closeAllMenus() {
    hideAllDropdownMenusOnly();
    syncWindowHeight();
}

async function connectTarget(target) {
    if (!invoke || !target || isConnecting) return;
    pendingRoomDraft = null;
    setConnectingState(true);
    try {
        const resolvedId = await invoke('switch_room', { target: String(target) });
        setDropdownOpen(false);
        if (currentRoomState.connected && currentRoomState.room_id === Number(resolvedId)) {
            setConnectingState(false);
        }
    } catch (e) {
        setConnectingState(false);
        console.error('Failed to switch room:', e);
        showToast(t('toast.error_prefix', { error: String(e) }));
    }
}

async function selectAndConnectRoom(roomId) {
    if (!roomId) return;
    const roomInput = document.getElementById('room-input');
    const idStr = String(roomId);
    if (roomInput) {
        roomInput.value = idStr;
        roomInput.title = idStr;
        blurRoomInputWithoutRevert();
    }
    setDropdownOpen(false);
    await connectTarget(idStr);
}

function renderRoomOptions(payload) {
    if (payload) {
        lastRoomOptions = payload;
    }
    const myRoomEl = document.getElementById('my-room-item');
    const followingEl = document.getElementById('following-rooms-list');
    const historyEl = document.getElementById('history-rooms-list');

    if (myRoomEl) {
        if (payload?.my_room) {
            const label = `${payload.my_room.room_id}(${payload.my_room.room_uname})${payload.my_room.title ? ': ' + payload.my_room.title : ''}`;
            myRoomEl.className = 'menu-item';
            myRoomEl.innerText = label;
            myRoomEl.title = label;
            myRoomEl.onclick = (e) => {
                e.stopPropagation();
                selectAndConnectRoom(payload.my_room.room_id);
            };
        } else {
            myRoomEl.className = 'menu-item empty-hint';
            myRoomEl.innerText = t('panel.my_room_empty');
            myRoomEl.title = '';
            myRoomEl.onclick = null;
        }
    }

    if (followingEl) {
        followingEl.replaceChildren();
        const list = payload?.following_live || [];
        if (list.length === 0) {
            const empty = document.createElement('div');
            empty.className = 'menu-item empty-hint';
            empty.innerText = t('panel.following_empty');
            followingEl.appendChild(empty);
        } else {
            list.forEach((item) => {
                const label = `${item.room_id}(${item.room_uname})${item.title ? ': ' + item.title : ''}`;
                const row = document.createElement('div');
                row.className = 'menu-item';
                row.innerText = label;
                row.title = label;
                row.onclick = (e) => {
                    e.stopPropagation();
                    selectAndConnectRoom(item.room_id);
                };
                followingEl.appendChild(row);
            });
        }
    }

    if (historyEl) {
        renderHistoryList(payload?.history_rooms || []);
    }
}

function renderHistoryList(historyRooms) {
    currentHistoryRooms = Array.isArray(historyRooms) ? historyRooms : [];
    const historyEl = document.getElementById('history-rooms-list');
    if (!historyEl) return;
    historyEl.replaceChildren();

    if (currentHistoryRooms.length === 0) {
        const empty = document.createElement('div');
        empty.className = 'menu-item empty-hint';
        empty.innerText = t('panel.history_empty');
        historyEl.appendChild(empty);
        return;
    }

    currentHistoryRooms.forEach((entry) => {
        const roomId = typeof entry === 'number' ? entry : entry?.room_id;
        if (!roomId) return;
        const uname = typeof entry === 'object' ? (entry.uname || '') : '';
        const title = typeof entry === 'object' ? (entry.title || '') : '';
        const head = uname ? `${roomId}(${uname})` : `${roomId}`;
        const text = title ? `${head}: ${title}` : head;

        const row = document.createElement('div');
        row.className = 'history-row';

        const label = document.createElement('div');
        label.className = 'menu-item';
        label.innerText = text;
        label.title = text;
        label.onclick = (e) => {
            e.stopPropagation();
            selectAndConnectRoom(roomId);
        };

        const removeBtn = document.createElement('button');
        removeBtn.className = 'history-remove-btn';
        removeBtn.title = t('panel.history_remove');
        removeBtn.innerText = '×';
        removeBtn.onclick = async (e) => {
            e.stopPropagation();
            if (!invoke) return;
            try {
                const updated = await invoke('remove_history_room', { roomId });
                if (lastRoomOptions) {
                    lastRoomOptions.history_rooms = updated;
                }
                renderHistoryList(updated);
            } catch (err) {
                console.error('Failed to remove history room:', err);
            }
        };

        row.appendChild(label);
        row.appendChild(removeBtn);
        historyEl.appendChild(row);
    });
}

async function hydrateInitialState() {
    if (!invoke) return;
    try {
        await ensureBaseLogicalHeight();
        const [i18nPayload, init, plugins] = await Promise.all([
            invoke('get_i18n'),
            invoke('get_initial_state'),
            invoke('get_plugins'),
        ]);
        if (i18nPayload) {
            applyTranslations(i18nPayload);
        }
        if (init) {
            if (!currentRoomState.connected || init.room_state?.connected) {
                updateRoomDisplay(init.room_state, { fromBackendEvent: false });
            }
            const stillConnecting = Boolean(init.connecting) && !currentRoomState.connected;
            setConnectingState(stillConnecting);
            updateAccountDisplay(init.auth_state);
            renderHistoryList(init.history_rooms || []);
            updateRecordRawDisplay(init.record_raw);
        }
        if (Array.isArray(plugins)) {
            renderPlugins(plugins);
        }
        syncPinUiState();
        updateLanguageButtons(currentLocale);
        await fetchPendingAclCount();
        scheduleSyncPanelMinWidth();
    } catch (e) {
        console.error('Failed to hydrate initial UI state:', e);
    }
}

// Bind Tauri native IPC events (NO /ws subscription in dambo-ui!)
if (listen) {
    listen('room_state_changed', (event) => {
        if (event.payload) {
            updateRoomDisplay(event.payload);
        }
    });

    listen('auth_state_changed', (event) => {
        if (event.payload) {
            updateAccountDisplay(event.payload);
        }
    });

    listen('app_state_changed', (event) => {
        const persisted = event.payload;
        if (!persisted) return;
        if (Array.isArray(persisted.room_history)) {
            if (lastRoomOptions) {
                lastRoomOptions.history_rooms = persisted.room_history;
            }
            renderHistoryList(persisted.room_history);
        }
        if (typeof persisted.record_raw === 'boolean') {
            updateRecordRawDisplay(persisted.record_raw);
        }
    });

    listen('plugins-updated', (event) => {
        if (Array.isArray(event.payload)) {
            renderPlugins(event.payload);
        }
    });

    listen('acl-updated', () => {
        fetchPendingAclCount();
    });

    listen('i18n-updated', (event) => {
        if (event.payload) {
            applyTranslations(event.payload);
        }
    });
}

function startPanelDragging() {
    const cur = window.__TAURI__?.window?.getCurrentWindow?.();
    if (cur && typeof cur.startDragging === 'function') {
        return cur.startDragging().catch(() =>
            window.__TAURI_INTERNALS__?.invoke?.('plugin:window|start_dragging')
        );
    }
    return window.__TAURI_INTERNALS__?.invoke?.('plugin:window|start_dragging');
}

function startPanelEastResize() {
    const cur = window.__TAURI__?.window?.getCurrentWindow?.();
    if (cur && typeof cur.startResizeDragging === 'function') {
        return cur.startResizeDragging('East').catch(() =>
            window.__TAURI_INTERNALS__?.invoke?.('plugin:window|start_resize_dragging', {
                value: 'East',
            })
        );
    }
    return window.__TAURI_INTERNALS__?.invoke?.('plugin:window|start_resize_dragging', {
        value: 'East',
    });
}

let activePointerResize = null;

function stopPointerEastResize() {
    if (!activePointerResize) return;
    const drag = activePointerResize;
    activePointerResize = null;

    if (drag.rafId !== null) {
        cancelAnimationFrame(drag.rafId);
    }
    window.removeEventListener('pointermove', drag.onMove);
    window.removeEventListener('pointerup', drag.onEnd);
    window.removeEventListener('pointercancel', drag.onEnd);
    drag.handle.removeEventListener('lostpointercapture', drag.onEnd);
    drag.handle.classList.remove('dragging');
    document.documentElement.style.cursor = '';

    try {
        if (drag.handle.hasPointerCapture?.(drag.pointerId)) {
            drag.handle.releasePointerCapture(drag.pointerId);
        }
    } catch (_) {
        // Ignore if capture is already released
    }
}

function startPointerEastResize(handle, startEvent) {
    if (!handle || !invoke) return;
    stopPointerEastResize();

    const pointerId = startEvent.pointerId;
    const startScreenX = startEvent.screenX;
    const startWidth = window.innerWidth;
    let lastSentWidth = Math.round(startWidth);
    let pendingWidth = lastSentWidth;
    let dirty = false;
    let inFlight = false;
    let rafId = null;

    const pumpWidth = async () => {
        if (inFlight || !dirty) return;
        inFlight = true;
        try {
            while (dirty) {
                dirty = false;
                const targetWidth = pendingWidth;
                if (targetWidth === lastSentWidth) continue;
                lastSentWidth = targetWidth;
                try {
                    await invoke('set_panel_width', { width: targetWidth });
                } catch (err) {
                    console.error('Failed to resize panel width:', err);
                }
            }
        } finally {
            inFlight = false;
            if (dirty) {
                pumpWidth();
            }
        }
    };

    const flushWidth = () => {
        rafId = null;
        if (activePointerResize) {
            activePointerResize.rafId = null;
        }
        pumpWidth();
    };

    const onMove = (ev) => {
        if (ev.pointerId !== pointerId) return;
        const deltaX = ev.screenX - startScreenX;
        const maxWidth = Math.max(PANEL_MAX_WIDTH, lastSyncedPanelMinWidth);
        const clamped = Math.max(
            lastSyncedPanelMinWidth,
            Math.min(maxWidth, Math.round(startWidth + deltaX))
        );
        if (clamped !== pendingWidth) {
            pendingWidth = clamped;
            dirty = true;
        }
        if (dirty && rafId === null && !inFlight) {
            rafId = requestAnimationFrame(flushWidth);
            if (activePointerResize) {
                activePointerResize.rafId = rafId;
            }
        }
    };

    const onEnd = (ev) => {
        if (ev && ev.pointerId !== undefined && ev.pointerId !== pointerId) return;
        if (dirty) {
            pumpWidth();
        }
        stopPointerEastResize();
    };

    activePointerResize = {
        handle,
        pointerId,
        onMove,
        onEnd,
        rafId: null,
    };

    handle.classList.add('dragging');
    document.documentElement.style.cursor = 'ew-resize';
    try {
        handle.setPointerCapture(pointerId);
    } catch (_) {
        // Window listeners continue tracking if capture fails
    }

    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onEnd);
    window.addEventListener('pointercancel', onEnd);
    handle.addEventListener('lostpointercapture', onEnd);
}

const resizeHandleEl = document.getElementById('resize-handle-right');
resizeHandleEl?.addEventListener('pointerdown', (e) => {
    if (e.pointerType === 'mouse' && e.button !== 0) return;
    e.stopPropagation();
    e.preventDefault();
    if (IS_MACOS || e.pointerType !== 'mouse') {
        startPointerEastResize(resizeHandleEl, e);
        return;
    }
    Promise.resolve(startPanelEastResize()).catch(() => {
        startPointerEastResize(resizeHandleEl, e);
    });
});

let activePointerMove = null;

function stopPointerPanelMove() {
    if (!activePointerMove) return;
    const drag = activePointerMove;
    activePointerMove = null;

    if (drag.rafId !== null) {
        cancelAnimationFrame(drag.rafId);
    }
    window.removeEventListener('pointermove', drag.onMove);
    window.removeEventListener('pointerup', drag.onEnd);
    window.removeEventListener('pointercancel', drag.onEnd);
    drag.target.removeEventListener('lostpointercapture', drag.onEnd);

    try {
        if (drag.target.hasPointerCapture?.(drag.pointerId)) {
            drag.target.releasePointerCapture(drag.pointerId);
        }
    } catch (_) {
        // Ignore if capture is already released
    }

    drag.endWhenIdle();
}

function startPointerPanelMove(target, startEvent) {
    if (!target || !invoke) return;
    stopPointerPanelMove();

    const pointerId = startEvent.pointerId;
    const startScreenX = startEvent.screenX;
    const startScreenY = startEvent.screenY;
    let latestDx = 0;
    let latestDy = 0;
    let lastSentDx = 0;
    let lastSentDy = 0;
    let dirty = false;
    let inFlight = false;
    let finished = false;
    let endSent = false;
    let rafId = null;

    try {
        target.setPointerCapture(pointerId);
    } catch (_) {
        // Window listeners continue tracking if capture fails
    }

    const beginPromise = Promise.resolve(invoke('begin_panel_move')).catch((err) => {
        console.error('Failed to begin pointer panel move:', err);
    });

    const finalizeBackendMove = () => {
        if (endSent) return;
        endSent = true;
        invoke('end_panel_move').catch(() => {});
    };

    const pumpMove = async () => {
        if (inFlight || !dirty) return;
        inFlight = true;
        try {
            await beginPromise;
            while (dirty) {
                dirty = false;
                const dx = latestDx;
                const dy = latestDy;
                if (dx === lastSentDx && dy === lastSentDy) continue;
                lastSentDx = dx;
                lastSentDy = dy;
                try {
                    await invoke('move_panel_by', { dx, dy });
                } catch (err) {
                    console.error('Failed to move panel:', err);
                }
            }
        } finally {
            inFlight = false;
            if (dirty) {
                pumpMove();
            } else if (finished) {
                finalizeBackendMove();
            }
        }
    };

    const flushMove = () => {
        rafId = null;
        if (activePointerMove) {
            activePointerMove.rafId = null;
        }
        pumpMove();
    };

    const onMove = (ev) => {
        if (ev.pointerId !== pointerId) return;
        const dx = ev.screenX - startScreenX;
        const dy = ev.screenY - startScreenY;
        if (dx === latestDx && dy === latestDy) return;
        latestDx = dx;
        latestDy = dy;
        dirty = true;
        if (rafId === null && !inFlight) {
            rafId = requestAnimationFrame(flushMove);
            if (activePointerMove) {
                activePointerMove.rafId = rafId;
            }
        }
    };

    const onEnd = (ev) => {
        if (ev && ev.pointerId !== undefined && ev.pointerId !== pointerId) return;
        finished = true;
        if (dirty) {
            pumpMove();
        }
        stopPointerPanelMove();
    };

    activePointerMove = {
        target,
        pointerId,
        onMove,
        onEnd,
        rafId: null,
        endWhenIdle: () => {
            finished = true;
            if (!inFlight && !dirty) {
                finalizeBackendMove();
            }
        },
    };

    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onEnd);
    window.addEventListener('pointercancel', onEnd);
    target.addEventListener('lostpointercapture', onEnd);
}

const dragHandleEl = document.getElementById('drag-handle');
dragHandleEl?.addEventListener('pointerdown', (e) => {
    if (e.pointerType === 'mouse' && e.button !== 0) return;
    e.stopPropagation();
    e.preventDefault();
    if (e.pointerType !== 'mouse') {
        startPointerPanelMove(dragHandleEl, e);
        return;
    }
    Promise.resolve(startPanelDragging()).catch((err) => {
        console.error('Failed to start panel drag:', err);
    });
});

const podContainerEl = document.querySelector('.pod-container');
podContainerEl?.addEventListener('pointerdown', (e) => {
    if (e.pointerType === 'mouse' && e.button !== 0) return;
    if (
        e.target instanceof Element &&
        e.target.closest(
            'button, input, textarea, select, a, .room-pill, .resize-handle-right, .dropdown-menu, .plugin-dropdown-menu, .more-dropdown-menu'
        )
    ) {
        return;
    }
    if (e.pointerType !== 'mouse') {
        e.preventDefault();
        startPointerPanelMove(podContainerEl, e);
        return;
    }
    Promise.resolve(startPanelDragging()).catch((err) => {
        console.error('Failed to start panel drag:', err);
    });
});

const roomInputEl = document.getElementById('room-input');
const roomSwitchBtnEl = document.getElementById('room-switch-btn');

function restoreRoomInputToActive() {
    if (!roomInputEl) return;
    pendingRoomDraft = null;
    roomInputEl.value = activeRoomLabel;
    roomInputEl.title = activeRoomLabel;
    syncRoomActionButton();
}

function blurRoomInputWithoutRevert() {
    if (!roomInputEl) return;
    suppressRoomBlurRevert = true;
    try {
        roomInputEl.blur();
    } finally {
        suppressRoomBlurRevert = false;
    }
}

roomInputEl?.addEventListener('focus', () => {
    skipNextRoomInputMouseUp = true;
    roomInputEl.select();
});

roomInputEl?.addEventListener('mouseup', (e) => {
    if (skipNextRoomInputMouseUp) {
        skipNextRoomInputMouseUp = false;
        e.preventDefault();
    }
});

roomInputEl?.addEventListener('blur', () => {
    skipNextRoomInputMouseUp = false;
    if (!suppressRoomBlurRevert && !isConnecting) {
        restoreRoomInputToActive();
    }
});

roomInputEl?.addEventListener('input', () => {
    pendingRoomDraft = roomInputEl.value;
    syncRoomActionButton();
});

roomInputEl?.addEventListener('keydown', async (e) => {
    skipNextRoomInputMouseUp = false;
    if (e.key === 'Enter' && !e.isComposing && e.keyCode !== 229) {
        e.preventDefault();
        const input = roomInputEl.value.trim();
        setDropdownOpen(false);
        if (input && !isConnecting) {
            blurRoomInputWithoutRevert();
            await connectTarget(input);
        }
    } else if (e.key === 'Escape') {
        e.preventDefault();
        roomInputEl.blur();
    }
});

document.getElementById('room-dropdown-btn')?.addEventListener('click', async () => {
    const menu = document.getElementById('room-dropdown-menu');
    const willOpen = menu ? menu.classList.contains('hidden') : false;
    setDropdownOpen(willOpen);
    if (willOpen && invoke) {
        try {
            const options = await invoke('fetch_room_options');
            renderRoomOptions(options);
        } catch (e) {
            console.error('Failed to fetch room options:', e);
        }
    }
});

roomSwitchBtnEl?.addEventListener('pointerdown', (e) => {
    e.preventDefault();
});
roomSwitchBtnEl?.addEventListener('mousedown', (e) => {
    e.preventDefault();
});

roomSwitchBtnEl?.addEventListener('click', async () => {
    if (!invoke || isConnecting) return;
    const input = (roomInputEl?.value || '').trim();
    setDropdownOpen(false);
    if (
        currentRoomState.connected &&
        (!input || input === activeRoomLabel || input === String(currentRoomState.room_id))
    ) {
        pendingRoomDraft = null;
        setConnectingState(false);
        roomInputEl?.blur();
        await invoke('disconnect_room').catch(console.error);
    } else if (input) {
        blurRoomInputWithoutRevert();
        await connectTarget(input);
    }
});

document.getElementById('console-open-btn')?.addEventListener('click', async () => {
    if (!invoke) return;
    closeAllMenus();
    try {
        await invoke('open_admin_console', { view: null });
        showToast(t('toast.opened_admin'));
    } catch (e) {
        console.error('open_admin_console failed:', e);
        showToast(t('toast.error_prefix', { error: String(e) }));
    }
});

document.getElementById('plugin-dropdown-btn')?.addEventListener('click', async () => {
    const menu = document.getElementById('plugin-dropdown-menu');
    const willOpen = menu ? menu.classList.contains('hidden') : false;
    setPluginMenuOpen(willOpen);
    if (willOpen && invoke) {
        try {
            const plugins = await invoke('get_plugins');
            renderPlugins(plugins);
        } catch (e) {
            console.error('Failed to fetch plugins:', e);
        }
    }
});

document.getElementById('plugin-admin-qr-btn')?.addEventListener('click', (e) => {
    e.stopPropagation();
    openPluginQrView('__admin__', 'view');
});

document.getElementById('plugin-qr-back-btn')?.addEventListener('click', (e) => {
    e.stopPropagation();
    setPluginViewMode('list');
});

document.querySelectorAll('#plugin-qr-page-row .qr-page-btn').forEach((btn) => {
    btn.addEventListener('click', (e) => {
        e.stopPropagation();
        const page = btn.getAttribute('data-qr-page') === 'manage' ? 'manage' : 'view';
        if (qrPluginPage === page) return;
        qrPluginPage = page;
        syncQrViewControls();
        fetchPluginQrUrls();
    });
});

document.getElementById('plugin-qr-copy-btn')?.addEventListener('click', async (e) => {
    e.stopPropagation();
    const copyBtn = document.getElementById('plugin-qr-copy-btn');
    const url = copyBtn?.dataset.copyUrl || document.getElementById('plugin-qr-url-text')?.innerText || '';
    if (!url) return;
    try {
        await navigator.clipboard.writeText(url);
        if (copyBtn) {
            copyBtn.classList.add('copied');
            copyBtn.innerText = t('lan.copied');
            setTimeout(() => {
                copyBtn.classList.remove('copied');
                copyBtn.innerText = t('lan.copy_url');
            }, 1500);
        }
    } catch (err) {
        console.error('Failed to copy QR url:', err);
        showToast(t('toast.error_prefix', { error: String(err) }));
    }
});

document.getElementById('plugin-open-dir-btn')?.addEventListener('click', () => {
    if (invoke) {
        invoke('open_plugins_dir').catch(console.error);
    }
});

document.getElementById('pin-btn')?.addEventListener('click', async () => {
    if (!invoke) return;
    try {
        const nextState = await invoke('set_always_on_top', { alwaysOnTop: !isAlwaysOnTop });
        isAlwaysOnTop = Boolean(nextState);
        syncPinUiState();
    } catch (e) {
        console.error('Failed to toggle always on top:', e);
        showToast(t('toast.error_prefix', { error: String(e) }));
    }
});

document.getElementById('min-btn')?.addEventListener('click', () => {
    if (invoke) invoke('minimize_window').catch(console.error);
});
document.getElementById('close-btn')?.addEventListener('click', () => {
    if (invoke) invoke('close_window').catch(console.error);
});

document.getElementById('account-badge')?.addEventListener('click', () => {
    if (invoke) invoke('open_qr_login').catch(console.error);
});

document.getElementById('more-btn')?.addEventListener('click', () => {
    const moreMenu = document.getElementById('more-dropdown-menu');
    const willOpen = moreMenu ? moreMenu.classList.contains('hidden') : false;
    setMoreMenuOpen(willOpen);
});

document.getElementById('more-record-raw-btn')?.addEventListener('click', async () => {
    if (!invoke) return;
    try {
        const nextState = await invoke('set_record_raw', { enabled: !recordRawEnabled });
        updateRecordRawDisplay(nextState);
        showToast(t(nextState ? 'toast.record_raw_on' : 'toast.record_raw_off'));
    } catch (e) {
        console.error('Failed to toggle record_raw:', e);
        showToast(t('toast.error_prefix', { error: String(e) }));
    } finally {
        setMoreMenuOpen(false);
    }
});

document.getElementById('more-open-config-btn')?.addEventListener('click', () => {
    setMoreMenuOpen(false);
    if (invoke) invoke('open_config_file').catch(console.error);
});

document.getElementById('more-open-rec-btn')?.addEventListener('click', () => {
    setMoreMenuOpen(false);
    if (invoke) invoke('open_folder', { kind: 'record' }).catch(console.error);
});

document.getElementById('more-open-log-btn')?.addEventListener('click', () => {
    setMoreMenuOpen(false);
    if (invoke) invoke('open_folder', { kind: 'log' }).catch(console.error);
});

const langSubmenuGroup = document.getElementById('more-lang-submenu-group');
langSubmenuGroup?.addEventListener('mouseenter', () => {
    setLangSubmenuOpen(true);
});
langSubmenuGroup?.addEventListener('mouseleave', () => {
    setLangSubmenuOpen(false);
});
document.querySelectorAll('#more-dropdown-menu > .more-menu-item').forEach((item) => {
    item.addEventListener('mouseenter', () => {
        setLangSubmenuOpen(false);
    });
});
document.getElementById('more-lang-trigger')?.addEventListener('click', (e) => {
    e.stopPropagation();
    const submenu = document.getElementById('more-lang-submenu');
    const isHidden = submenu ? submenu.classList.contains('hidden') : true;
    setLangSubmenuOpen(isHidden);
});

document.querySelectorAll('#more-lang-submenu .more-lang-option').forEach((btn) => {
    btn.addEventListener('click', async (e) => {
        e.stopPropagation();
        const locale = btn.getAttribute('data-locale');
        if (!locale || !invoke) return;
        try {
            const payload = await invoke('set_locale', { locale });
            if (payload) {
                applyTranslations(payload);
            } else {
                updateLanguageButtons(locale);
            }
            const label = btn.querySelector('.menu-item-label')?.innerText.trim() || locale;
            showToast(t('toast.locale_updated', { locale: label }));
        } catch (err) {
            console.error('Failed to switch locale:', err);
            showToast(t('toast.error_prefix', { error: String(err) }));
        } finally {
            setMoreMenuOpen(false);
        }
    });
});

document.addEventListener('click', (e) => {
    if (e.target instanceof Node && !e.target.isConnected) {
        return;
    }

    const roomMenu = document.getElementById('room-dropdown-menu');
    const roomGroup = document.querySelector('.room-selector-group');
    if (roomMenu && !roomMenu.classList.contains('hidden') && roomGroup && !roomGroup.contains(e.target)) {
        setDropdownOpen(false);
    }

    const pluginMenu = document.getElementById('plugin-dropdown-menu');
    const pluginGroup = document.querySelector('.plugin-menu-group');
    if (pluginMenu && !pluginMenu.classList.contains('hidden') && pluginGroup && !pluginGroup.contains(e.target)) {
        setPluginMenuOpen(false);
    }

    const moreMenu = document.getElementById('more-dropdown-menu');
    const moreGroup = document.querySelector('.more-menu-group');
    if (moreMenu && !moreMenu.classList.contains('hidden') && moreGroup && !moreGroup.contains(e.target)) {
        setMoreMenuOpen(false);
    }
});

document.addEventListener('pointerdown', (e) => {
    if (!isDanmakuBarOpen) return;
    const target = e.target instanceof Element ? e.target : null;
    if (!target) return;
    if (target.closest('#danmaku-bar')) return;
    if (target.closest('#danmaku-toggle-btn')) return;
    if (target.closest('#drag-handle, #resize-handle-right')) return;
    if (target.closest('button, input, textarea, select, a, .room-pill, .menu-item, .plugin-row')) {
        setDanmakuBarOpen(false);
    }
}, true);

async function submitDanmaku() {
    const input = document.getElementById('danmaku-input');
    if (!invoke || !input || isSendingDanmaku) return;
    if (!canSendDanmaku()) {
        syncDanmakuUiState();
        return;
    }
    if (Date.now() < danmakuCooldownUntil) return;

    const msg = input.value.trim();
    if (!msg) return;

    isSendingDanmaku = true;
    syncDanmakuSendButton();
    try {
        await invoke('send_danmaku', { msg });
        input.value = '';
        danmakuCooldownUntil = Date.now() + DANMAKU_COOLDOWN_MS;
        if (danmakuCooldownTimer !== null) {
            clearTimeout(danmakuCooldownTimer);
        }
        danmakuCooldownTimer = setTimeout(() => {
            danmakuCooldownTimer = null;
            syncDanmakuSendButton();
        }, DANMAKU_COOLDOWN_MS);
    } catch (err) {
        console.error('Failed to send danmaku:', err);
        showToast(String(err));
    } finally {
        isSendingDanmaku = false;
        syncDanmakuSendButton();
        if (isDanmakuBarOpen) {
            input.focus({ preventScroll: true });
        }
    }
}

document.getElementById('danmaku-toggle-btn')?.addEventListener('click', () => {
    if (!canSendDanmaku()) {
        syncDanmakuUiState();
        return;
    }
    setDanmakuBarOpen(!isDanmakuBarOpen);
});

const danmakuInputEl = document.getElementById('danmaku-input');
danmakuInputEl?.addEventListener('input', syncDanmakuSendButton);
danmakuInputEl?.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.isComposing && e.keyCode !== 229) {
        e.preventDefault();
        submitDanmaku();
    }
});

const danmakuSendBtnEl = document.getElementById('danmaku-send-btn');
danmakuSendBtnEl?.addEventListener('pointerdown', (e) => {
    e.preventDefault();
});
danmakuSendBtnEl?.addEventListener('click', () => {
    submitDanmaku();
});

document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
        closeAllMenus();
        if (isDanmakuBarOpen) {
            setDanmakuBarOpen(false);
        }
    }
});

window.addEventListener('blur', () => {
    stopPointerEastResize();
    stopPointerPanelMove();
    closeAllMenus();
    if (document.activeElement === roomInputEl) {
        roomInputEl.blur();
    }
});

document.addEventListener('DOMContentLoaded', hydrateInitialState);
