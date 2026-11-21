(function() {
    const scPinBar = document.getElementById('sc-pin-bar');
    const container = document.getElementById('danmaku-container');
    const enterDock = document.getElementById('enter-dock');
    const SHOW_LIKE = new URLSearchParams(window.location.search).get('show_like') === '1';

    function isSuperChatKind(kind) {
        return kind === 'superchat' || kind === 'super_chat';
    }

    const MAX_ITEMS = 60;
    const MAX_PIN_CARDS = 4;
    const ENTER_TTL_MS = 3000;
    const FADE_MS = 280;
    const DEFAULT_AVATAR_SVG = 'data:image/svg+xml;utf8,<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="%2364748b"><circle cx="12" cy="8" r="4"/><path d="M4 20c0-4 4-6 8-6s8 2 8 6"/></svg>';

    const activeTimers = new Set();
    const seenPinKeys = new Set();
    let enterDismissTimer = null;
    let enterRemoveTimer = null;
    let ws = null;

    function clearEnterTimers() {
        if (enterDismissTimer !== null) {
            clearTimeout(enterDismissTimer);
            enterDismissTimer = null;
        }
        if (enterRemoveTimer !== null) {
            clearTimeout(enterRemoveTimer);
            enterRemoveTimer = null;
        }
    }

    function clearAllState() {
        for (const id of activeTimers) {
            clearTimeout(id);
        }
        activeTimers.clear();
        clearEnterTimers();
        seenPinKeys.clear();
        container.innerHTML = '';
        if (enterDock) {
            enterDock.innerHTML = '';
        }
        if (scPinBar) {
            scPinBar.innerHTML = '';
            scPinBar.classList.add('hidden');
        }
    }

    function escapeRegExp(str) {
        return str.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    }

    function formatCny(price) {
        if (!price || price <= 0) return '';
        const cny = price / 1000;
        const formatted = Number.isInteger(cny) ? String(cny) : String(parseFloat(cny.toFixed(2)));
        return `¥${formatted}`;
    }

    function appendAvatar(parentEl, faceUrl) {
        const avatar = document.createElement('img');
        avatar.className = 'avatar';
        avatar.src = faceUrl || DEFAULT_AVATAR_SVG;
        avatar.loading = 'lazy';
        avatar.onerror = () => {
            if (avatar.src !== DEFAULT_AVATAR_SVG) {
                avatar.src = DEFAULT_AVATAR_SVG;
            }
        };
        parentEl.appendChild(avatar);
    }

    function appendMedal(parentEl, item) {
        if (!item.medal_name) return;
        const medal = document.createElement('span');
        medal.className = 'medal-badge';
        medal.innerText = `${item.medal_name} ${item.medal_level || ''}`.trim();
        parentEl.appendChild(medal);
    }

    function appendUserIdentity(parentEl, item, showAvatar = true) {
        if (showAvatar) {
            appendAvatar(parentEl, item.face);
        }
        appendMedal(parentEl, item);
        const name = document.createElement('span');
        name.className = 'sender-name';
        name.innerText = item.uname || 'Viewer';
        parentEl.appendChild(name);
    }

    function appendSpan(parentEl, className, text) {
        const span = document.createElement('span');
        span.className = className;
        span.innerText = text;
        parentEl.appendChild(span);
        return span;
    }

    function appendContentWithEmotes(parentEl, item) {
        if (item.emoticon) {
            const sticker = document.createElement('img');
            sticker.className = 'danmaku-sticker';
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

    function showEnterInDock(item, options) {
        if (!enterDock || !item) return;

        const delayMs = options?.isSnapshot
            ? ENTER_TTL_MS - (Date.now() - (item.timestamp || 0))
            : ENTER_TTL_MS;
        if (!Number.isFinite(delayMs) || delayMs <= 0) {
            return;
        }

        clearEnterTimers();
        enterDock.innerHTML = '';

        const div = document.createElement('div');
        div.className = 'danmaku-item item-enter';

        if (item.face) {
            const avatar = document.createElement('img');
            avatar.className = 'avatar';
            avatar.src = item.face;
            avatar.loading = 'lazy';
            avatar.onerror = () => {
                const icon = document.createElement('span');
                icon.className = 'event-icon';
                icon.innerText = '👋';
                avatar.replaceWith(icon);
            };
            div.appendChild(avatar);
        } else {
            appendSpan(div, 'event-icon', '👋');
        }

        appendMedal(div, item);
        appendSpan(div, 'sender-name', item.uname || 'Viewer');
        appendSpan(div, 'event-action', item.content || '进入直播间');

        enterDock.appendChild(div);

        enterDismissTimer = setTimeout(() => {
            enterDismissTimer = null;
            if (!div.isConnected) return;
            div.classList.add('fade-out');
            enterRemoveTimer = setTimeout(() => {
                enterRemoveTimer = null;
                if (div.parentElement === enterDock) {
                    div.remove();
                }
            }, FADE_MS);
        }, delayMs);
    }

    function pinnedHighlightKey(item) {
        if (item.id !== undefined && item.id !== null) {
            return `id:${item.id}`;
        }
        return `${item.kind}:${item.uid || 0}:${item.timestamp || 0}:${item.content || ''}`;
    }

    function durationForPinned(item) {
        if (item.kind === 'guard') return 90000;
        const cny = (item.price || 0) / 1000;
        if (cny >= 1000) return 300000;
        if (cny >= 500) return 180000;
        if (cny >= 100) return 120000;
        return 60000;
    }

    function addPinnedHighlight(item, options) {
        if (!scPinBar || (!isSuperChatKind(item.kind) && item.kind !== 'guard')) return;
        const key = pinnedHighlightKey(item);
        if (seenPinKeys.has(key)) return;

        const totalMs = durationForPinned(item);
        let remainingMs = totalMs;
        if (options?.isSnapshot) {
            if (!item.timestamp) return;
            remainingMs = totalMs - (Date.now() - item.timestamp);
            if (remainingMs <= 0) return;
            remainingMs = Math.max(3000, Math.min(totalMs, remainingMs));
        }
        seenPinKeys.add(key);

        const isGuard = item.kind === 'guard';
        const card = document.createElement('div');
        card.className = `sc-pin-card ${isGuard ? 'kind-guard' : 'kind-sc'}`;

        const header = document.createElement('div');
        header.className = 'sc-pin-header';
        appendUserIdentity(header, item, true);
        const cnyText = formatCny(item.price);
        const badgeText = isGuard
            ? (cnyText ? `⚓ 舰队 ${cnyText}` : '⚓ 舰队')
            : `SC ${cnyText || '¥0'}`;
        appendSpan(header, 'price-badge price-sc', badgeText);
        card.appendChild(header);

        const bodyText = item.content || (isGuard ? '开通了大航海守护' : '');
        if (bodyText || item.emoticon) {
            const body = document.createElement('div');
            body.className = 'sc-pin-body';
            if (isGuard && !item.emoticon) {
                body.innerText = bodyText;
            } else {
                appendContentWithEmotes(body, item);
            }
            card.appendChild(body);
        }

        const track = document.createElement('div');
        track.className = 'sc-pin-progress-track';
        const bar = document.createElement('div');
        bar.className = 'sc-pin-progress-bar';
        bar.style.transition = `transform ${remainingMs}ms linear`;
        track.appendChild(bar);
        card.appendChild(track);

        scPinBar.classList.remove('hidden');
        scPinBar.prepend(card);
        while (scPinBar.children.length > MAX_PIN_CARDS) {
            scPinBar.removeChild(scPinBar.lastChild);
        }

        requestAnimationFrame(() => {
            requestAnimationFrame(() => {
                bar.style.transform = 'scaleX(0)';
            });
        });

        const t1 = setTimeout(() => {
            activeTimers.delete(t1);
            if (!card.isConnected) return;
            card.classList.add('fade-out');
            const t2 = setTimeout(() => {
                activeTimers.delete(t2);
                if (card.isConnected) {
                    card.remove();
                }
                if (scPinBar.children.length === 0) {
                    scPinBar.classList.add('hidden');
                }
            }, FADE_MS);
            activeTimers.add(t2);
        }, remainingMs);
        activeTimers.add(t1);
    }

    function buildHighlightedEvent(item, spec) {
        const div = document.createElement('div');
        div.className = `danmaku-item item-${spec.kind}`;

        appendAvatar(div, item.face);
        appendMedal(div, item);
        appendSpan(div, `event-badge ${spec.badgeClass}`, spec.badgeText);
        appendSpan(div, 'sender-name', item.uname || 'Viewer');
        appendSpan(div, 'event-action', spec.actionText);
        appendSpan(div, 'event-highlight', item.content || spec.fallbackContent);

        const cny = formatCny(item.price);
        if (cny) {
            appendSpan(div, 'price-badge', cny);
        }
        return div;
    }

    function buildSuperChatItem(item) {
        const div = document.createElement('div');
        div.className = 'danmaku-item item-superchat item-super_chat';

        const header = document.createElement('div');
        header.className = 'sc-header';
        appendUserIdentity(header, item, true);
        const priceText = formatCny(item.price) || '¥0';
        appendSpan(header, 'price-badge price-sc', `SC ${priceText}`);
        div.appendChild(header);

        if (item.content || item.emoticon) {
            const body = document.createElement('div');
            body.className = 'sc-body';
            appendContentWithEmotes(body, item);
            div.appendChild(body);
        }
        return div;
    }

    function buildInteractionItem(kind, item, icon, fallbackAction) {
        const div = document.createElement('div');
        div.className = `danmaku-item item-interaction item-${kind}`;

        appendAvatar(div, item.face);
        appendMedal(div, item);
        appendSpan(div, 'event-icon', icon);
        appendSpan(div, 'sender-name', item.uname || 'Viewer');
        appendSpan(div, 'event-action', item.content || fallbackAction);
        return div;
    }

    function buildSystemItem(kind, item) {
        const isAlert = kind === 'warning' || kind === 'cut_off' || kind === 'room_block';
        const div = document.createElement('div');
        div.className = `danmaku-item item-system${isAlert ? ' item-alert' : ''}`;

        let icon = '📢';
        let text = item.content || '';
        if (kind === 'live') {
            icon = '🟢';
            text = text || '直播开始';
        } else if (kind === 'preparing') {
            icon = '🌙';
            text = text || '主播下播';
        } else if (kind === 'room_change') {
            icon = '📢';
            text = text ? `房间变更：${text}` : '房间信息变更';
        } else if (kind === 'room_block') {
            icon = '🚫';
            const uname = item.uname ? `${item.uname} ` : '';
            text = `${uname}${text || '被禁言'}`;
        } else if (kind === 'room_silent') {
            icon = '🔇';
            text = text || '房间禁言状态变更';
        } else if (kind === 'warning' || kind === 'cut_off') {
            icon = '⚠️';
            text = text || (kind === 'cut_off' ? '直播已切断' : '系统警告');
        }

        appendSpan(div, 'event-icon', icon);
        appendSpan(div, 'system-text', text);
        return div;
    }

    function buildChatItem(item) {
        const div = document.createElement('div');
        div.className = 'danmaku-item item-chat';

        appendUserIdentity(div, item, true);

        const content = document.createElement('span');
        content.className = 'danmaku-content';
        appendContentWithEmotes(content, item);
        div.appendChild(content);
        return div;
    }

    function renderItem(item, options = {}) {
        if (!item) return;
        const kind = item.kind || 'chat';

        if (kind === 'like' && !SHOW_LIKE) return;
        if (
            kind === 'room_info' ||
            kind === 'session_end' ||
            kind === 'reconnected' ||
            kind === 'superchat_delete' ||
            kind === 'combo_end'
        ) {
            return;
        }

        if (kind === 'enter') {
            showEnterInDock(item, options);
            return;
        }

        if (isSuperChatKind(kind) || kind === 'guard') {
            addPinnedHighlight(item, options);
        }

        let div = null;
        switch (kind) {
            case 'gift':
                div = buildHighlightedEvent(item, {
                    kind: 'gift',
                    badgeClass: 'badge-gift',
                    badgeText: '🎁',
                    actionText: '送出了',
                    fallbackContent: '礼物',
                });
                break;
            case 'guard':
                div = buildHighlightedEvent(item, {
                    kind: 'guard',
                    badgeClass: 'badge-guard',
                    badgeText: '⚓ 上舰',
                    actionText: '开通了',
                    fallbackContent: '大航海',
                });
                break;
            case 'superchat':
            case 'super_chat':
                div = buildSuperChatItem(item);
                break;
            case 'guard_enter':
                div = buildInteractionItem(kind, item, '⚓', '进入直播间');
                break;
            case 'follow':
                div = buildInteractionItem(kind, item, '❤️', '关注了直播间');
                break;
            case 'special_follow':
                div = buildInteractionItem(kind, item, '⭐', '特别关注了直播间');
                break;
            case 'mutual_follow':
                div = buildInteractionItem(kind, item, '🤝', '互粉了直播间');
                break;
            case 'share':
                div = buildInteractionItem(kind, item, '🔗', '分享了直播间');
                break;
            case 'like':
                div = buildInteractionItem(kind, item, '👍', '为主播点赞了');
                break;
            case 'live':
            case 'preparing':
            case 'room_change':
            case 'room_silent':
            case 'room_block':
            case 'warning':
            case 'cut_off':
                div = buildSystemItem(kind, item);
                break;
            default:
                div = buildChatItem(item);
                break;
        }

        if (!div) return;
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
                    clearAllState();
                    const now = Date.now();
                    const items = msg.data.items;

                    const streamItems = items
                        .filter((it) => it && it.kind !== 'enter')
                        .slice(-MAX_ITEMS);
                    streamItems.forEach((it) => renderItem(it, { isSnapshot: true }));

                    for (let i = items.length - 1; i >= 0; i--) {
                        const it = items[i];
                        if (it && it.kind === 'enter') {
                            if (it.timestamp && now - it.timestamp < ENTER_TTL_MS) {
                                showEnterInDock(it, { isSnapshot: true });
                            }
                            break;
                        }
                    }
                } else if (msg.type === 'Event' && msg.data) {
                    renderItem(msg.data, { isSnapshot: false });
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
