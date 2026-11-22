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
    const SHOW_LIKE = Dambo.utils.getQueryBool('show_like', false);

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
        if (currentFilter === 'gift_sc') return item.kind === 'gift' || Dambo.utils.isSuperChat(item.kind) || item.kind === 'guard';
        if (currentFilter === 'medal') return !!item.medal_name;
        return true;
    }

    function appendChatWithEmotes(parentEl, item) {
        Dambo.utils.renderContent(parentEl, item, {
            stickerClass: 'feed-sticker',
            emotClass: 'feed-emot',
        });
    }

    function createFeedRow(item) {
        const row = document.createElement('div');
        row.className = `feed-row item-${item.kind}`;
        row.dataset.kind = item.kind;
        row.dataset.hasMedal = item.medal_name ? 'true' : 'false';

        row.appendChild(Dambo.utils.createAvatar(item, 'feed-avatar'));

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
        if (Dambo.utils.isSuperChat(item.kind)) {
            const priceText = Dambo.utils.formatPrice(item.price) || '¥0';
            text.innerText = `[SC ${priceText}] ${item.content}`;
        } else if (item.kind === 'gift') {
            text.innerText = `[Gift] ${item.content}`;
        } else if (item.kind === 'guard') {
            const guardInfo = Dambo.utils.formatGuard(item);
            text.innerText = `[Guard] ${guardInfo.summaryText}`;
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

        if (Dambo.utils.isSuperChat(item.kind) || item.kind === 'guard') {
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

    const client = Dambo.connect({
        channel: 'feed',
        replaySnapshot: false,
        showLike: SHOW_LIKE,
    });

    client.on('snapshot', (snapshotData) => {
        roomStatusBadge.innerText = Dambo.utils.formatRoomBadge(snapshotData);
        allItems = (snapshotData.items || []).filter(
            (it) => it && (it.kind !== 'like' || SHOW_LIKE)
        );
        reapplyFilter();
    });

    client.on('event', (item) => {
        appendItem(item);
    });

    client.on('room_state', (state) => {
        roomStatusBadge.innerText = Dambo.utils.formatRoomBadge(state);
    });

    client.on('reconnect', () => {
        roomStatusBadge.innerText = 'Disconnected, reconnecting...';
    });
})();
