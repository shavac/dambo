(function() {
    const container = document.getElementById('danmaku-container');
    const MAX_ITEMS = 60;
    let ws = null;

    function escapeRegExp(str) {
        return str.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    }

    function appendContentWithEmotes(parentEl, item) {
        if (item.emoticon) {
            const sticker = document.createElement('img');
            sticker.className = 'danmaku-sticker';
            sticker.src = item.emoticon;
            sticker.alt = item.content || '表情';
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
            img.className = 'danmaku-emot';
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

    function renderItem(item) {
        const div = document.createElement('div');
        div.className = `danmaku-item item-${item.kind || 'chat'}`;

        // Avatar
        const avatar = document.createElement('img');
        avatar.className = 'avatar';
        avatar.src = item.face || 'data:image/svg+xml;utf8,<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="%2364748b"><circle cx="12" cy="8" r="4"/><path d="M4 20c0-4 4-6 8-6s8 2 8 6"/></svg>';
        avatar.loading = 'lazy';
        div.appendChild(avatar);

        // Fan Medal
        if (item.medal_name) {
            const medal = document.createElement('span');
            medal.className = 'medal-badge';
            medal.innerText = `${item.medal_name} ${item.medal_level || ''}`;
            div.appendChild(medal);
        }

        // Name
        const name = document.createElement('span');
        name.className = 'sender-name';
        name.innerText = item.uname || '观众';
        div.appendChild(name);

        // Content (with sticker / inline emote support)
        const content = document.createElement('span');
        content.className = 'danmaku-content';
        appendContentWithEmotes(content, item);
        div.appendChild(content);

        container.appendChild(div);

        // Prune older messages
        while (container.children.length > MAX_ITEMS) {
            container.removeChild(container.firstChild);
        }
    }

    function connect() {
        const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
        const wsUrl = `${protocol}//${window.location.host}/ws`;

        ws = new WebSocket(wsUrl);

        ws.onmessage = (event) => {
            try {
                const msg = JSON.parse(event.data);
                if (msg.type === 'Snapshot' && msg.data?.items) {
                    container.innerHTML = '';
                    const recent = msg.data.items.slice(-MAX_ITEMS);
                    recent.forEach(renderItem);
                } else if (msg.type === 'Event' && msg.data) {
                    renderItem(msg.data);
                }
            } catch (e) {
                console.error("Failed to parse ws event:", e);
            }
        };

        ws.onclose = () => {
            setTimeout(connect, 2000);
        };

        ws.onerror = () => {
            ws.close();
        };
    }

    connect();
})();
