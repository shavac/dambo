(function() {
    const stack = document.getElementById('ticker-stack');
    const MAX_CARDS = 8;
    const seenIds = new Set();

    function durationForItem(item) {
        if (item.kind === 'guard') return 90000;
        const cny = (item.price || 0) / 1000;
        if (cny >= 1000) return 300000;
        if (cny >= 500) return 180000;
        if (cny >= 100) return 120000;
        return 60000;
    }

    function formatPrice(item) {
        const cny = (item.price || 0) / 1000;
        if (item.kind === 'guard') {
            return cny > 0 ? `大航海 ¥${cny.toFixed(0)}` : '大航海';
        }
        return `SC ¥${cny % 1 === 0 ? cny.toFixed(0) : cny.toFixed(1)}`;
    }

    function addTickerCard(item) {
        if (!item || (item.kind !== 'super_chat' && item.kind !== 'guard')) return;
        if (item.id && seenIds.has(item.id)) return;
        if (item.id) seenIds.add(item.id);

        const durationMs = durationForItem(item);
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
        avatar.src = item.face || 'data:image/svg+xml;utf8,<svg xmlns="http://www.w3.org/2000/svg" width="28" height="28" viewBox="0 0 24 24" fill="%2394a3b8"><circle cx="12" cy="8" r="4"/><path d="M4 20c0-4 4-6 8-6s8 2 8 6"/></svg>';
        header.appendChild(avatar);

        const meta = document.createElement('div');
        meta.className = 'card-user-meta';

        const uname = document.createElement('span');
        uname.className = 'card-uname';
        uname.innerText = item.uname || '热心观众';
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
        body.innerText = item.content || (item.kind === 'guard' ? '开通了大航海守护' : '');
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

    function connect() {
        const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
        const ws = new WebSocket(`${protocol}//${window.location.host}/ws`);

        ws.onmessage = (event) => {
            try {
                const msg = JSON.parse(event.data);
                if (msg.type === 'Snapshot' && msg.data?.items) {
                    stack.innerHTML = '';
                    msg.data.items.forEach(addTickerCard);
                } else if (msg.type === 'Event' && msg.data) {
                    addTickerCard(msg.data);
                }
            } catch (e) {
                console.error('Failed to parse sc-ticker event:', e);
            }
        };

        ws.onclose = () => setTimeout(connect, 2000);
        ws.onerror = () => ws.close();
    }

    connect();
})();
