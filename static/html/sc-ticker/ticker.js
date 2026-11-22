(function() {
    const stack = document.getElementById('ticker-stack');
    const MAX_CARDS = 8;
    const seenIds = new Set();

    function formatPrice(item) {
        const priceText = Dambo.utils.formatPrice(item.price);
        if (item.kind === 'guard') {
            return priceText ? `Guard ${priceText}` : 'Guard';
        }
        return `SC ${priceText || '¥0'}`;
    }

    function addTickerCard(item) {
        if (!item || (!Dambo.utils.isSuperChat(item.kind) && item.kind !== 'guard')) return;
        if (item.id && seenIds.has(item.id)) return;
        if (item.id) seenIds.add(item.id);

        const durationMs = Dambo.utils.superChatDurationMs(item);
        const createdAt = item.timestamp || Date.now();
        const remainingMs = Math.max(5000, createdAt + durationMs - Date.now());
        if (createdAt + durationMs <= Date.now()) return;

        const card = document.createElement('div');
        const cny = (item.price || 0) / 1000;
        const isHigh = cny >= 100;
        card.className = `ticker-card ${item.kind === 'guard' ? 'kind-guard' : 'kind-sc'}${isHigh ? ' tier-high' : ''}`;

        const header = document.createElement('div');
        header.className = 'card-header';

        const avatar = document.createElement('img');
        avatar.className = 'card-avatar';
        avatar.src = item.face || Dambo.utils.DEFAULT_AVATAR_SVG;
        avatar.onerror = () => {
            if (avatar.src !== Dambo.utils.DEFAULT_AVATAR_SVG) {
                avatar.src = Dambo.utils.DEFAULT_AVATAR_SVG;
            }
        };
        header.appendChild(avatar);

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
        price.innerText = formatPrice(item);
        header.appendChild(price);

        card.appendChild(header);

        const body = document.createElement('div');
        body.className = 'card-body';
        body.innerText = item.content || (item.kind === 'guard' ? 'Subscribed to Guard' : '');
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

        setTimeout(() => {
            card.classList.add('exiting');
            setTimeout(() => card.remove(), 360);
        }, remainingMs);
    }

    const client = Dambo.connect({
        channel: 'feed',
        replaySnapshot: false,
    });

    client.on('snapshot', (snapshotData) => {
        stack.innerHTML = '';
        (snapshotData.items || []).forEach(addTickerCard);
    });

    client.on('superchat', addTickerCard);
    client.on('guard', addTickerCard);
})();
