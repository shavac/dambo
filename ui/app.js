const invoke = window.__TAURI__?.core?.invoke || window.__TAURI__?.invoke;
const listen = window.__TAURI__?.event?.listen;

let currentRoomState = {
    room_id: 0,
    room_uname: "",
    room_title: "",
    is_live: false,
    connected: false,
};

let serverHost = "localhost";
let serverPort = 3838;
let activeRoomLabel = "";

function formatRoomLabel(state) {
    if (!state || state.room_id <= 0) return "";
    const head = state.room_uname ? `${state.room_id}(${state.room_uname})` : `${state.room_id}`;
    return state.room_title ? `${head}: ${state.room_title}` : head;
}

function updateRoomDisplay(state) {
    currentRoomState = state;
    const roomDot = document.getElementById('room-dot');
    const roomInput = document.getElementById('room-input');
    const roomSwitchBtn = document.getElementById('room-switch-btn');

    roomDot.className = 'status-dot';
    if (!state.connected) {
        roomDot.classList.add('dot-gray');
        roomSwitchBtn.innerText = state.room_id > 0 ? "切换" : "连接";
    } else if (!state.is_live) {
        roomDot.classList.add('dot-yellow');
        roomSwitchBtn.innerText = "断开";
    } else {
        roomDot.classList.add('dot-green');
        roomSwitchBtn.innerText = "断开";
    }

    activeRoomLabel = formatRoomLabel(state);
    roomInput.title = activeRoomLabel;
    if (document.activeElement !== roomInput) {
        roomInput.value = activeRoomLabel;
    }
}

function updateAccountDisplay(auth) {
    const accountDot = document.getElementById('account-dot');
    const accountName = document.getElementById('account-name');

    accountDot.className = 'status-dot';
    if (auth && auth.is_login) {
        accountDot.classList.add('dot-green');
        accountName.innerText = auth.uname ? `@${auth.uname}` : `@${auth.mid}`;
    } else {
        accountDot.classList.add('dot-gray');
        accountName.innerText = "未登录";
    }
}

function populateThemes(themes, activeTheme) {
    const select = document.getElementById('theme-select');
    if (!select) return;
    const list = Array.isArray(themes) && themes.length > 0 ? themes : ['default', 'sc-ticker'];
    select.innerHTML = '';
    list.forEach((name) => {
        const opt = document.createElement('option');
        opt.value = name;
        opt.innerText = name;
        if (name === activeTheme) opt.selected = true;
        select.appendChild(opt);
    });
}

function setDropdownOpen(open) {
    const menu = document.getElementById('room-dropdown-menu');
    if (!menu) return;
    if (open) {
        menu.classList.remove('hidden');
    } else {
        menu.classList.add('hidden');
    }
    if (invoke) {
        invoke('set_dropdown_expanded', { expanded: open }).catch(console.error);
    }
}

async function connectTarget(target) {
    if (!invoke || !target) return;
    try {
        await invoke('switch_room', { target: String(target) });
        setDropdownOpen(false);
    } catch (e) {
        console.error('Failed to switch room:', e);
    }
}

function renderRoomOptions(payload) {
    const myRoomEl = document.getElementById('my-room-item');
    const followingEl = document.getElementById('following-rooms-list');
    const historyEl = document.getElementById('history-rooms-list');

    if (myRoomEl) {
        if (payload?.my_room) {
            const label = `${payload.my_room.room_id}(${payload.my_room.room_uname})${payload.my_room.title ? ': ' + payload.my_room.title : ''}`;
            myRoomEl.className = 'menu-item';
            myRoomEl.innerText = label;
            myRoomEl.title = label;
            myRoomEl.onclick = () => connectTarget(payload.my_room.room_id);
        } else {
            myRoomEl.className = 'menu-item empty-hint';
            myRoomEl.innerText = '未获取到直播间';
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
            empty.innerText = '暂无开播主播';
            followingEl.appendChild(empty);
        } else {
            list.forEach((item) => {
                const label = `${item.room_id}(${item.room_uname})${item.title ? ': ' + item.title : ''}`;
                const row = document.createElement('div');
                row.className = 'menu-item';
                row.innerText = label;
                row.title = label;
                row.onclick = () => connectTarget(item.room_id);
                followingEl.appendChild(row);
            });
        }
    }

    if (historyEl) {
        renderHistoryList(payload?.history_rooms || []);
    }
}

function renderHistoryList(historyRooms) {
    const historyEl = document.getElementById('history-rooms-list');
    if (!historyEl) return;
    historyEl.innerHTML = '';

    if (!historyRooms || historyRooms.length === 0) {
        const empty = document.createElement('div');
        empty.className = 'menu-item empty-hint';
        empty.innerText = '暂无历史';
        historyEl.appendChild(empty);
        return;
    }

    historyRooms.forEach((roomId) => {
        const row = document.createElement('div');
        row.className = 'history-row';

        const label = document.createElement('div');
        label.className = 'menu-item';
        label.innerText = String(roomId);
        label.onclick = () => connectTarget(roomId);

        const removeBtn = document.createElement('button');
        removeBtn.className = 'history-remove-btn';
        removeBtn.title = '移除';
        removeBtn.innerText = '×';
        removeBtn.onclick = async (e) => {
            e.stopPropagation();
            if (!invoke) return;
            try {
                const updated = await invoke('remove_history_room', { roomId });
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
        const init = await invoke('get_initial_state');
        if (!init) return;
        serverHost = init.server_host || 'localhost';
        serverPort = init.server_port || 3838;

        const copyBtn = document.getElementById('copy-obs-btn');
        if (copyBtn) {
            copyBtn.innerText = `📋 OBS (:${serverPort})`;
        }

        updateRoomDisplay(init.room_state);
        updateAccountDisplay(init.auth_state);
        populateThemes(init.themes, init.theme);
        renderHistoryList(init.history_rooms || []);
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
}

document.getElementById('copy-obs-btn')?.addEventListener('click', async () => {
    try {
        await navigator.clipboard.writeText(`http://${serverHost}:${serverPort}`);
        const btn = document.getElementById('copy-obs-btn');
        const orig = btn.innerText;
        btn.innerText = "已复制!";
        setTimeout(() => { btn.innerText = orig; }, 1500);
    } catch (e) {
        console.error("Failed to copy OBS url:", e);
    }
});

document.getElementById('room-switch-btn')?.addEventListener('click', async () => {
    if (!invoke) return;
    const input = document.getElementById('room-input').value.trim();
    if (currentRoomState.connected && (!input || input === activeRoomLabel)) {
        await invoke('disconnect_room').catch(console.error);
    } else if (input) {
        await connectTarget(input);
    }
});

document.getElementById('room-input')?.addEventListener('keydown', async (e) => {
    if (e.key === 'Enter') {
        const input = e.target.value.trim();
        if (input) {
            await connectTarget(input);
            e.target.blur();
        }
    }
});

document.getElementById('room-dropdown-btn')?.addEventListener('click', async () => {
    const menu = document.getElementById('room-dropdown-menu');
    const willOpen = menu.classList.contains('hidden');
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

document.getElementById('theme-select')?.addEventListener('change', (e) => {
    const theme = e.target.value;
    if (invoke) {
        invoke('change_theme', { theme }).catch(console.error);
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

document.addEventListener('click', (e) => {
    const menu = document.getElementById('room-dropdown-menu');
    const group = document.querySelector('.room-selector-group');
    if (menu && !menu.classList.contains('hidden') && group && !group.contains(e.target)) {
        setDropdownOpen(false);
    }
});

document.addEventListener('DOMContentLoaded', hydrateInitialState);
