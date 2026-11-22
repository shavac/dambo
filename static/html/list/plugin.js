(function() {
    const feedContainer = document.getElementById('feed-container');
    const feedList = document.getElementById('feed-list');
    const scrollBottomBtn = document.getElementById('scroll-bottom-btn');
    const newMsgCountEl = document.getElementById('new-msg-count');
    const stickyHeader = document.getElementById('sticky-header');
    const stickyCards = document.getElementById('sticky-cards');
    const roomStatusBadge = document.getElementById('room-status-badge');

    let currentFilter = 'all';
    let unreadCount = 0;
    let allItems = [];
    const SHOW_LIKE = Dambo.utils.getQueryBool('show_like', false);
    const timerBag = Dambo.utils.createTimerBag();
    const autoScroll = Dambo.utils.createAutoScroll(feedContainer, {
        thresholdPx: 60,
        onStateChange: (following) => {
            if (following) {
                unreadCount = 0;
                scrollBottomBtn.classList.add('hidden');
            }
        },
    });

    // Filter switching
    document.querySelectorAll('.filter-btn').forEach(btn => {
        btn.addEventListener('click', () => {
            document.querySelectorAll('.filter-btn').forEach(b => b.classList.remove('active'));
            btn.classList.add('active');
            currentFilter = btn.dataset.filter;
            reapplyFilter();
        });
    });

    // Scroll to bottom button
    scrollBottomBtn.addEventListener('click', () => {
        unreadCount = 0;
        scrollBottomBtn.classList.add('hidden');
        autoScroll.schedule({ force: true, smooth: true });
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

    function formatGiftRowText(item) {
        const meta = Dambo.utils.formatGift(item);
        const count = item._comboCount || meta.comboNum || meta.num || 1;
        const name = item._giftName || meta.giftName;
        const priceSeeds = item._comboPrice !== undefined ? item._comboPrice : meta.priceSeeds;
        const priceText = meta.coinType === 'gold' ? Dambo.utils.formatPrice(priceSeeds) : '';
        return priceText ? `[Gift] ${name} × ${count} (${priceText})` : `[Gift] ${name} × ${count}`;
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
            text.innerText = formatGiftRowText(item);
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
        timerBag.set(() => {
            if (card.isConnected) {
                card.remove();
            }
            if (stickyCards.children.length === 0) {
                stickyHeader.classList.add('hidden');
            }
        }, 60000);
    }

    function pushItemToHistory(rec) {
        allItems.push(rec);
        if (allItems.length > 500) allItems.shift();
    }

    function notifyNewFeedRow() {
        if (!autoScroll.isFollowing()) {
            unreadCount++;
            newMsgCountEl.innerText = unreadCount;
            scrollBottomBtn.classList.remove('hidden');
        } else {
            autoScroll.schedule();
        }
    }

    const giftCombos = Dambo.utils.createGiftComboTracker({
        windowMs: 4000,
        onCreate: (entry, item, opts) => {
            const rec = {
                ...item,
                _giftName: entry.giftName,
                _comboCount: entry.totalCount,
                _comboPrice: entry.totalPriceSeeds,
            };
            pushItemToHistory(rec);

            let row = null;
            let textEl = null;
            if (!opts?.isSnapshot && matchFilter(rec)) {
                row = createFeedRow(rec);
                textEl = row.querySelector('.feed-text');
                feedList.appendChild(row);
                notifyNewFeedRow();
            }
            entry.data = { record: rec, row, textEl };
        },
        onUpdate: (entry, _delta, _item, opts) => {
            if (!entry.data) return;
            entry.data.record._comboCount = entry.totalCount;
            entry.data.record._comboPrice = entry.totalPriceSeeds;

            if (!opts?.isSnapshot && entry.data.textEl && entry.data.row?.isConnected) {
                entry.data.textEl.innerText = formatGiftRowText(entry.data.record);
                autoScroll.schedule();
            }
        },
    });

    function reapplyFilter() {
        feedList.innerHTML = '';
        allItems.filter(matchFilter).forEach(item => {
            feedList.appendChild(createFeedRow(item));
        });
        if (autoScroll.isFollowing()) {
            autoScroll.schedule();
        }
    }

    function appendItem(item, options) {
        if (!item) return;
        if (item.kind === 'like' && !SHOW_LIKE) return;
        if (item.kind === 'room_info' || item.kind === 'session_end' || item.kind === 'combo_end') return;

        const isSnapshot = Boolean(options?.isSnapshot);

        if (!isSnapshot && (Dambo.utils.isSuperChat(item.kind) || item.kind === 'guard')) {
            addStickySC(item);
        }

        if (item.kind === 'gift') {
            giftCombos.push(item, { isSnapshot });
            return;
        }

        pushItemToHistory(item);

        if (!isSnapshot && matchFilter(item)) {
            const row = createFeedRow(item);
            feedList.appendChild(row);
            notifyNewFeedRow();
        }
    }

    const client = Dambo.connect({
        channel: 'feed',
        replaySnapshot: false,
        showLike: SHOW_LIKE,
    });

    client.on('snapshot', (snapshotData) => {
        roomStatusBadge.innerText = Dambo.utils.formatRoomBadge(snapshotData);
        timerBag.clearAll();
        giftCombos.clear();
        stickyCards.innerHTML = '';
        stickyHeader.classList.add('hidden');
        allItems = [];
        unreadCount = 0;
        scrollBottomBtn.classList.add('hidden');
        autoScroll.reset();

        (snapshotData.items || []).forEach((it) => appendItem(it, { isSnapshot: true }));
        giftCombos.pruneExpiredSnapshots();
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
