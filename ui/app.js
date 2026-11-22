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
let isConnecting = false;
let connectingTimeoutId = null;
let isAlwaysOnTop = false;
let toastTimer = null;

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
const MIN_ROOM_PICKER_WIDTH = 136;
const IS_MACOS = /mac/i.test(navigator.platform || navigator.userAgent || '');
let lastSyncedPanelMinWidth = PANEL_BASE_MIN_WIDTH;
let minWidthSyncRaf = null;

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

    const logoutBtn = document.getElementById('more-logout-btn');
    if (logoutBtn) {
        logoutBtn.disabled = !currentAuthState.is_login;
    }
    scheduleSyncPanelMinWidth();
}

function updateRecordRawDisplay(enabled) {
    recordRawEnabled = Boolean(enabled);
    const check = document.getElementById('more-record-raw-check');
    if (check) {
        check.innerText = recordRawEnabled ? '✓' : '';
    }
}

function renderPlugins(plugins) {
    pluginList = Array.isArray(plugins) ? plugins : [];
    const triggerLabel = document.getElementById('plugin-trigger-label');
    if (triggerLabel) {
        triggerLabel.innerText = t('plugin.trigger_label', { count: pluginList.length });
    }

    const listEl = document.getElementById('plugin-cards-list');
    if (!listEl) return;
    listEl.innerHTML = '';

    if (pluginList.length === 0) {
        const empty = document.createElement('div');
        empty.className = 'plugin-empty';
        empty.innerText = t('plugin.empty');
        listEl.appendChild(empty);
        return;
    }

    pluginList.forEach((p) => {
        const isDesktopOpen = Boolean(p.is_desktop_open);
        const card = document.createElement('div');
        card.className = `plugin-card${isDesktopOpen ? ' desktop-open' : ''}`;

        const main = document.createElement('div');
        main.className = 'plugin-card-main';

        const header = document.createElement('div');
        header.className = 'plugin-card-header';

        const title = document.createElement('span');
        title.className = 'plugin-title';
        title.innerText = p.title || p.name;
        header.appendChild(title);

        const badge = document.createElement('span');
        badge.className = `plugin-badge${p.is_builtin ? '' : ' custom'}`;
        badge.innerText = t(p.is_builtin ? 'plugin.badge_builtin' : 'plugin.badge_custom');
        header.appendChild(badge);

        if (p.obs_width && p.obs_height) {
            const size = document.createElement('span');
            size.className = 'plugin-size-badge';
            size.innerText = `${p.obs_width}×${p.obs_height}`;
            header.appendChild(size);
        }

        main.appendChild(header);

        const desc = document.createElement('div');
        desc.className = 'plugin-desc';
        desc.innerText = p.description || t('plugin.no_description');
        desc.title = desc.innerText;
        main.appendChild(desc);

        const urlEl = document.createElement('div');
        urlEl.className = 'plugin-url';
        urlEl.innerText = `${p.route}  ·  ${p.url}`;
        urlEl.title = p.url;
        main.appendChild(urlEl);

        const actions = document.createElement('div');
        actions.className = 'plugin-card-actions';

        const toggleDesktopBtn = document.createElement('button');
        toggleDesktopBtn.type = 'button';
        toggleDesktopBtn.className = `plugin-btn ${isDesktopOpen ? 'danger' : 'primary'}`;
        toggleDesktopBtn.innerText = t(isDesktopOpen ? 'plugin.close_desktop' : 'plugin.open_desktop');
        toggleDesktopBtn.onclick = async () => {
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
            } catch (e) {
                console.error('toggle_plugin_desktop failed:', e);
                showToast(t('toast.error_prefix', { error: String(e) }));
            }
        };

        const copyBtn = document.createElement('button');
        copyBtn.type = 'button';
        copyBtn.className = 'plugin-btn';
        copyBtn.innerText = t('plugin.copy_url');
        copyBtn.onclick = async () => {
            try {
                await navigator.clipboard.writeText(p.url);
                copyBtn.classList.add('copied');
                copyBtn.innerText = t('plugin.copied');
                setTimeout(() => {
                    copyBtn.classList.remove('copied');
                    copyBtn.innerText = t('plugin.copy_url');
                }, 1500);
            } catch (e) {
                console.error('Failed to copy plugin url:', e);
                showToast(t('toast.error_prefix', { error: String(e) }));
            }
        };

        actions.appendChild(toggleDesktopBtn);
        actions.appendChild(copyBtn);

        card.appendChild(main);
        card.appendChild(actions);
        listEl.appendChild(card);
    });
    scheduleSyncPanelMinWidth();
}

let lastSyncedExpanded = null;

function syncWindowExpanded() {
    const roomMenu = document.getElementById('room-dropdown-menu');
    const pluginMenu = document.getElementById('plugin-dropdown-menu');
    const moreMenu = document.getElementById('more-dropdown-menu');
    const expanded = Boolean(
        (roomMenu && !roomMenu.classList.contains('hidden')) ||
        (pluginMenu && !pluginMenu.classList.contains('hidden')) ||
        (moreMenu && !moreMenu.classList.contains('hidden'))
    );
    if (expanded === lastSyncedExpanded) return;
    lastSyncedExpanded = expanded;
    if (invoke) {
        invoke('set_dropdown_expanded', { expanded }).catch((err) => {
            lastSyncedExpanded = null;
            console.error(err);
        });
    }
}

function setDropdownOpen(open) {
    const menu = document.getElementById('room-dropdown-menu');
    if (!menu) return;
    if (open) {
        document.getElementById('plugin-dropdown-menu')?.classList.add('hidden');
        document.getElementById('plugin-dropdown-btn')?.classList.remove('active');
        document.getElementById('plugin-dropdown-btn')?.setAttribute('aria-expanded', 'false');
        document.getElementById('more-dropdown-menu')?.classList.add('hidden');
        document.getElementById('more-btn')?.classList.remove('active');
        document.getElementById('more-btn')?.setAttribute('aria-expanded', 'false');
        menu.classList.remove('hidden');
    } else {
        menu.classList.add('hidden');
    }
    syncWindowExpanded();
}

function setPluginMenuOpen(open) {
    const pluginMenu = document.getElementById('plugin-dropdown-menu');
    const pluginBtn = document.getElementById('plugin-dropdown-btn');
    if (!pluginMenu) return;
    if (open) {
        document.getElementById('room-dropdown-menu')?.classList.add('hidden');
        document.getElementById('more-dropdown-menu')?.classList.add('hidden');
        document.getElementById('more-btn')?.classList.remove('active');
        document.getElementById('more-btn')?.setAttribute('aria-expanded', 'false');
        pluginMenu.classList.remove('hidden');
        pluginBtn?.classList.add('active');
        pluginBtn?.setAttribute('aria-expanded', 'true');
    } else {
        pluginMenu.classList.add('hidden');
        pluginBtn?.classList.remove('active');
        pluginBtn?.setAttribute('aria-expanded', 'false');
    }
    syncWindowExpanded();
}

function setMoreMenuOpen(open) {
    const moreMenu = document.getElementById('more-dropdown-menu');
    const moreBtn = document.getElementById('more-btn');
    if (!moreMenu) return;
    if (open) {
        document.getElementById('room-dropdown-menu')?.classList.add('hidden');
        document.getElementById('plugin-dropdown-menu')?.classList.add('hidden');
        document.getElementById('plugin-dropdown-btn')?.classList.remove('active');
        document.getElementById('plugin-dropdown-btn')?.setAttribute('aria-expanded', 'false');
        moreMenu.classList.remove('hidden');
        moreBtn?.classList.add('active');
        moreBtn?.setAttribute('aria-expanded', 'true');
    } else {
        moreMenu.classList.add('hidden');
        moreBtn?.classList.remove('active');
        moreBtn?.setAttribute('aria-expanded', 'false');
    }
    syncWindowExpanded();
}

function closeAllMenus() {
    document.getElementById('room-dropdown-menu')?.classList.add('hidden');
    document.getElementById('plugin-dropdown-menu')?.classList.add('hidden');
    document.getElementById('plugin-dropdown-btn')?.classList.remove('active');
    document.getElementById('plugin-dropdown-btn')?.setAttribute('aria-expanded', 'false');
    const moreMenu = document.getElementById('more-dropdown-menu');
    const moreBtn = document.getElementById('more-btn');
    moreMenu?.classList.add('hidden');
    moreBtn?.classList.remove('active');
    moreBtn?.setAttribute('aria-expanded', 'false');
    syncWindowExpanded();
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
        roomInput.blur();
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
        followingEl.innerHTML = '';
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
    historyEl.innerHTML = '';

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
    let pendingWidth = null;
    let rafId = null;

    const flushWidth = () => {
        rafId = null;
        if (activePointerResize) {
            activePointerResize.rafId = null;
        }
        if (pendingWidth === null || pendingWidth === lastSentWidth) return;
        const targetWidth = pendingWidth;
        lastSentWidth = targetWidth;
        invoke('set_panel_width', { width: targetWidth }).catch((err) => {
            console.error('Failed to resize panel width:', err);
        });
    };

    const onMove = (ev) => {
        if (ev.pointerId !== pointerId) return;
        const deltaX = ev.screenX - startScreenX;
        const maxWidth = Math.max(PANEL_MAX_WIDTH, lastSyncedPanelMinWidth);
        const clamped = Math.max(
            lastSyncedPanelMinWidth,
            Math.min(maxWidth, Math.round(startWidth + deltaX))
        );
        pendingWidth = clamped;
        if (rafId === null) {
            rafId = requestAnimationFrame(flushWidth);
            if (activePointerResize) {
                activePointerResize.rafId = rafId;
            }
        }
    };

    const onEnd = (ev) => {
        if (ev && ev.pointerId !== undefined && ev.pointerId !== pointerId) return;
        if (pendingWidth !== null && pendingWidth !== lastSentWidth) {
            flushWidth();
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

    if (invoke) {
        invoke('end_panel_move').catch(() => {});
    }
}

function startPointerPanelMove(target, startEvent) {
    if (!target || !invoke) return;
    stopPointerPanelMove();

    const pointerId = startEvent.pointerId;
    const startScreenX = startEvent.screenX;
    const startScreenY = startEvent.screenY;
    let lastSentDx = 0;
    let lastSentDy = 0;
    let pendingDx = 0;
    let pendingDy = 0;
    let rafId = null;

    invoke('begin_panel_move').catch((err) => {
        console.error('Failed to begin pointer panel move:', err);
    });

    const flushMove = () => {
        rafId = null;
        if (activePointerMove) {
            activePointerMove.rafId = null;
        }
        if (pendingDx === lastSentDx && pendingDy === lastSentDy) return;
        const dx = pendingDx;
        const dy = pendingDy;
        lastSentDx = dx;
        lastSentDy = dy;
        invoke('move_panel_by', { dx, dy }).catch((err) => {
            console.error('Failed to move panel:', err);
        });
    };

    const onMove = (ev) => {
        if (ev.pointerId !== pointerId) return;
        pendingDx = Math.round(ev.screenX - startScreenX);
        pendingDy = Math.round(ev.screenY - startScreenY);
        if (rafId === null) {
            rafId = requestAnimationFrame(flushMove);
            if (activePointerMove) {
                activePointerMove.rafId = rafId;
            }
        }
    };

    const onEnd = (ev) => {
        if (ev && ev.pointerId !== undefined && ev.pointerId !== pointerId) return;
        if (pendingDx !== lastSentDx || pendingDy !== lastSentDy) {
            flushMove();
        }
        stopPointerPanelMove();
    };

    activePointerMove = {
        target,
        pointerId,
        onMove,
        onEnd,
        rafId: null,
    };

    try {
        target.setPointerCapture(pointerId);
    } catch (_) {
        // Window listeners continue tracking if capture fails
    }

    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onEnd);
    window.addEventListener('pointercancel', onEnd);
    target.addEventListener('lostpointercapture', onEnd);
}

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

roomInputEl?.addEventListener('input', () => {
    pendingRoomDraft = roomInputEl.value;
    syncRoomActionButton();
});

roomInputEl?.addEventListener('keydown', async (e) => {
    if (e.key === 'Enter') {
        e.preventDefault();
        const input = roomInputEl.value.trim();
        setDropdownOpen(false);
        if (input && !isConnecting) {
            roomInputEl.blur();
            await connectTarget(input);
        }
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

document.getElementById('room-switch-btn')?.addEventListener('click', async () => {
    if (!invoke || isConnecting) return;
    const input = (roomInputEl?.value || '').trim();
    setDropdownOpen(false);
    if (
        currentRoomState.connected &&
        (!input || input === activeRoomLabel || input === String(currentRoomState.room_id))
    ) {
        pendingRoomDraft = null;
        setConnectingState(false);
        await invoke('disconnect_room').catch(console.error);
    } else if (input) {
        await connectTarget(input);
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

document.getElementById('plugin-open-dir-btn')?.addEventListener('click', () => {
    if (invoke) {
        invoke('open_plugins_dir').catch(console.error);
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

document.getElementById('more-pin-btn')?.addEventListener('click', async () => {
    if (!invoke) return;
    try {
        const nextState = await invoke('set_always_on_top', { alwaysOnTop: !isAlwaysOnTop });
        isAlwaysOnTop = Boolean(nextState);
        const check = document.getElementById('more-pin-check');
        if (check) {
            check.innerText = isAlwaysOnTop ? '✓' : '';
        }
    } catch (e) {
        console.error('Failed to toggle always on top:', e);
    } finally {
        setMoreMenuOpen(false);
    }
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

document.getElementById('more-relogin-btn')?.addEventListener('click', () => {
    setMoreMenuOpen(false);
    if (invoke) invoke('open_qr_login').catch(console.error);
});

document.getElementById('more-logout-btn')?.addEventListener('click', () => {
    setMoreMenuOpen(false);
    if (invoke) invoke('logout').catch(console.error);
});

document.addEventListener('click', (e) => {
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

document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
        closeAllMenus();
    }
});

window.addEventListener('blur', () => {
    stopPointerEastResize();
    stopPointerPanelMove();
    closeAllMenus();
});

document.addEventListener('DOMContentLoaded', hydrateInitialState);
