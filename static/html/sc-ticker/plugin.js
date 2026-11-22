(function() {
    const stack = document.getElementById('ticker-stack');
    const MAX_CARDS = 8;
    const seenIds = new Set();

    const timerBag = Dambo.utils.createTimerBag();

    function clearTickerState() {
        timerBag.clearAll();
        seenIds.clear();
        stack.innerHTML = '';
    }

    function formatPrice(item, guardInfo) {
        const priceText = Dambo.utils.formatPrice(item.price);
        if (item.kind === 'guard') {
            const label = guardInfo ? `⚓ ${guardInfo.badgeLabel}` : 'Guard';
            return priceText ? `${label} ${priceText}` : label;
        }
        return `SC ${priceText || '¥0'}`;
    }

    function addTickerCard(item, options) {
        if (!item || (!Dambo.utils.isSuperChat(item.kind) && item.kind !== 'guard')) return;
        if (item.id && seenIds.has(item.id)) return;

        const durationMs = Dambo.utils.superChatDurationMs(item);
        const remainingMs = Dambo.utils.remainingDurationMs(item, durationMs, {
            isSnapshot: Boolean(options?.isSnapshot),
            minMs: 5000,
        });
        if (remainingMs <= 0) return;
        if (item.id) seenIds.add(item.id);

        const isGuard = item.kind === 'guard';
        const guardInfo = isGuard ? Dambo.utils.formatGuard(item) : null;
        const card = document.createElement('div');
        const cny = (item.price || 0) / 1000;
        const isHigh = cny >= 100;
        card.className = `ticker-card ${isGuard ? 'kind-guard' : 'kind-sc'}${isHigh ? ' tier-high' : ''}`;

        const header = document.createElement('div');
        header.className = 'card-header';

        header.appendChild(Dambo.utils.createAvatar(item, 'card-avatar'));

        const meta = document.createElement('div');
        meta.className = 'card-user-meta';

        const uname = document.createElement('span');
        uname.className = 'card-uname';
        uname.innerText = item.uname || 'Viewer';
        meta.appendChild(uname);

        if (item.medal_name) {
            const medal = document.createElement('span');
            medal.className = 'card-medal';
            medal.innerText = `${item.medal_name} ${item.medal_level || ''}`;
            meta.appendChild(medal);
        }

        header.appendChild(meta);

        const price = document.createElement('span');
        price.className = 'card-price';
        price.innerText = formatPrice(item, guardInfo);
        header.appendChild(price);

        card.appendChild(header);

        const body = document.createElement('div');
        body.className = 'card-body';
        if (isGuard && !item.emoticon) {
            body.innerText = guardInfo.summaryText;
        } else {
            Dambo.utils.renderContent(body, item, {
                stickerClass: 'card-sticker',
                emotClass: 'card-emot',
            });
        }
        card.appendChild(body);

        const track = document.createElement('div');
        track.className = 'card-progress-track';
        const bar = document.createElement('div');
        bar.className = 'card-progress-bar';
        bar.style.transition = `transform ${remainingMs}ms linear`;
        track.appendChild(bar);
        card.appendChild(track);

        stack.prepend(card);
        while (stack.children.length > MAX_CARDS) {
            stack.removeChild(stack.lastChild);
        }

        requestAnimationFrame(() => {
            requestAnimationFrame(() => {
                bar.style.transform = 'scaleX(0)';
            });
        });

        timerBag.set(() => {
            card.classList.add('exiting');
            timerBag.set(() => {
                card.remove();
            }, 360);
        }, remainingMs);
    }

    const client = Dambo.connect({
        channel: 'feed',
        replaySnapshot: false,
    });

    client.on('snapshot', (snapshotData) => {
        clearTickerState();
        (snapshotData.items || []).forEach((it) => addTickerCard(it, { isSnapshot: true }));
    });

    client.on('superchat', addTickerCard);
    client.on('guard', addTickerCard);
})();
