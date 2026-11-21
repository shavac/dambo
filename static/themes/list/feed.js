(function() {
    const feedContainer = document.getElementById('feed-container');
    const feedList = document.getElementById('feed-list');
    const scrollBottomBtn = document.getElementById('scroll-bottom-btn');
    const newMsgCountEl = document.getElementById('new-msg-count');
    const stickyHeader = document.getElementById('sticky-header');
    const stickyCards = document.getElementById('sticky-cards');
    const roomStatusBadge = document.getElementById('room-status-badge');

    let currentFilter = 'all';
    let isUserScrolledUp = false;
    let unreadCount = 0;
    let allItems = [];
    const SHOW_LIKE = new URLSearchParams(window.location.search).get('show_like') === '1';

    function isSuperChatKind(kind) {
        return kind === 'superchat' || kind === 'super_chat';
    }

    // Filter switching
    document.querySelectorAll('.filter-btn').forEach(btn => {
        btn.addEventListener('click', () => {
            document.querySelectorAll('.filter-btn').forEach(b => b.classList.remove('active'));
            btn.classList.add('active');
            currentFilter = btn.dataset.filter;
            reapplyFilter();
        });
    });

    // Detect user scroll position
    feedContainer.addEventListener('scroll', () => {
        const threshold = 60;
        const distFromBottom = feedContainer.scrollHeight - feedContainer.scrollTop - feedContainer.clientHeight;
        if (distFromBottom > threshold) {
            isUserScrolledUp = true;
        } else {
            isUserScrolledUp = false;
            unreadCount = 0;
            scrollBottomBtn.classList.add('hidden');
        }
    });

    // Scroll to bottom button
    scrollBottomBtn.addEventListener('click', () => {
        feedContainer.scrollTo({ top: feedContainer.scrollHeight, behavior: 'smooth' });
        isUserScrolledUp = false;
        unreadCount = 0;
        scrollBottomBtn.classList.add('hidden');
    });

    function matchFilter(item) {
        if (currentFilter === 'chat') return item.kind === 'chat';
        if (currentFilter === 'gift_sc') return item.kind === 'gift' || isSuperChatKind(item.kind) || item.kind === 'guard';
        if (currentFilter === 'medal') return !!item.medal_name;
        return true;
    }

    function escapeRegExp(str) {
        return str.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    }

    function appendChatWithEmotes(parentEl, item) {
        if (item.emoticon) {
            const sticker = document.createElement('img');
            sticker.className = 'feed-sticker';
            sticker.src = item.emoticon;
            sticker.alt = item.content || 'Sticker';
            sticker.loading = 'lazy';
            sticker.onerror = () => {
                sticker.replaceWith(document.createTextNode(item.content || ''));
            };
            parentEl.appendChild(sticker);
            return;
        }

        const text = item.content || '';
        const emots = item.emots;
        if (!emots || typeof emots !== 'object') {
            parentEl.innerText = text;
            return;
        }

        const keys = Object.keys(emots).filter((k) => k && emots[k]);
        if (keys.length === 0) {
            parentEl.innerText = text;
            return;
        }

        keys.sort((a, b) => b.length - a.length);
        const pattern = new RegExp(keys.map(escapeRegExp).join('|'), 'g');
        let lastIdx = 0;
        let match;
        while ((match = pattern.exec(text)) !== null) {
            if (match.index > lastIdx) {
                parentEl.appendChild(document.createTextNode(text.slice(lastIdx, match.index)));
            }
            const token = match[0];
            const img = document.createElement('img');
            img.className = 'feed-emot';
            img.src = emots[token];
            img.alt = token;
            img.title = token;
            img.loading = 'lazy';
            img.onerror = () => {
                img.replaceWith(document.createTextNode(token));
            };
            parentEl.appendChild(img);
            lastIdx = pattern.lastIndex;
        }
        if (lastIdx < text.length) {
            parentEl.appendChild(document.createTextNode(text.slice(lastIdx)));
        }
    }

    function formatRoomBadge(state) {
        if (!state || !state.room_id) return 'Disconnected';
        const head = state.room_uname ? `${state.room_id}(${state.room_uname})` : `${state.room_id}`;
        const label = state.room_title ? `${head}: ${state.room_title}` : head;
        if (!state.connected) return `${label} · Disconnected`;
        return state.is_live ? label : `${label} · Offline`;
    }

    function createFeedRow(item) {
        const row = document.createElement('div');
        row.className = `feed-row item-${item.kind}`;
        row.dataset.kind = item.kind;
        row.dataset.hasMedal = item.medal_name ? 'true' : 'false';

        const avatar = document.createElement('img');
        avatar.className = 'feed-avatar';
        avatar.src = item.face || 'data:image/svg+xml;utf8,<svg xmlns="http://www.w3.org/2000/svg" width="22" height="22" viewBox="0 0 24 24" fill="%2364748b"><circle cx="12" cy="8" r="4"/><path d="M4 20c0-4 4-6 8-6s8 2 8 6"/></svg>';
        avatar.loading = 'lazy';
        row.appendChild(avatar);

        const body = document.createElement('div');
        body.className = 'feed-body';

        const meta = document.createElement('div');
        meta.className = 'feed-meta';

        if (item.medal_name) {
            const medal = document.createElement('span');
            medal.className = 'feed-medal';
            medal.innerText = `${item.medal_name} ${item.medal_level || ''}`;
            meta.appendChild(medal);
        }

        const name = document.createElement('span');
        name.className = 'feed-name';
        name.innerText = item.uname || 'Viewer';
        meta.appendChild(name);

        body.appendChild(meta);

        const text = document.createElement('div');
        text.className = 'feed-text';
        if (isSuperChatKind(item.kind)) {
            text.innerText = `[SC ￥${(item.price || 0) / 1000}] ${item.content}`;
        } else if (item.kind === 'gift') {
            text.innerText = `[Gift] ${item.content}`;
        } else if (item.kind === 'guard') {
            text.innerText = `[Guard] ${item.content}`;
        } else {
            appendChatWithEmotes(text, item);
        }
        body.appendChild(text);

        row.appendChild(body);
        return row;
    }

    function addStickySC(item) {
        stickyHeader.classList.remove('hidden');
        const card = document.createElement('div');
        card.className = 'sc-card';

        const uname = document.createElement('div');
        uname.className = 'uname';
        uname.innerText = `${item.uname} (￥${(item.price || 0) / 1000})`;
        card.appendChild(uname);

        const content = document.createElement('div');
        content.className = 'content';
        content.innerText = item.content;
        card.appendChild(content);

        const prog = document.createElement('div');
        prog.className = 'sc-progress';
        card.appendChild(prog);

        stickyCards.appendChild(card);

        // Auto remove after 60s
        setTimeout(() => {
            card.remove();
            if (stickyCards.children.length === 0) {
                stickyHeader.classList.add('hidden');
            }
        }, 60000);
    }

    function reapplyFilter() {
        feedList.innerHTML = '';
        allItems.filter(matchFilter).forEach(item => {
            feedList.appendChild(createFeedRow(item));
        });
        if (!isUserScrolledUp) {
            feedContainer.scrollTop = feedContainer.scrollHeight;
        }
    }

    function appendItem(item) {
        if (!item) return;
        if (item.kind === 'like' && !SHOW_LIKE) return;
        if (item.kind === 'room_info' || item.kind === 'session_end' || item.kind === 'combo_end') return;

        allItems.push(item);
        if (allItems.length > 500) allItems.shift();

        if (isSuperChatKind(item.kind) || item.kind === 'guard') {
            addStickySC(item);
        }

        if (matchFilter(item)) {
            const row = createFeedRow(item);
            feedList.appendChild(row);

            if (isUserScrolledUp) {
                unreadCount++;
                newMsgCountEl.innerText = unreadCount;
                scrollBottomBtn.classList.remove('hidden');
            } else {
                feedContainer.scrollTop = feedContainer.scrollHeight;
            }
        }
    }

    function connect() {
        const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
        const wsUrl = `${protocol}//${window.location.host}/ws`;

        const ws = new WebSocket(wsUrl);

        ws.onmessage = (event) => {
            try {
                const msg = JSON.parse(event.data);
                if (msg.type === 'Snapshot' && msg.data) {
                    roomStatusBadge.innerText = formatRoomBadge(msg.data);
                    allItems = (msg.data.items || []).filter(
                        (it) => it && (it.kind !== 'like' || SHOW_LIKE)
                    );
                    reapplyFilter();
                } else if (msg.type === 'Event' && msg.data) {
                    appendItem(msg.data);
                } else if (msg.type === 'RoomState' && msg.data) {
                    roomStatusBadge.innerText = formatRoomBadge(msg.data);
                }
            } catch (e) {
                console.error("Failed to parse event:", e);
            }
        };

        ws.onclose = () => {
            roomStatusBadge.innerText = "Disconnected, reconnecting...";
            setTimeout(connect, 2000);
        };
    }

    connect();
})();
