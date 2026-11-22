(function() {
    const scPinBar = document.getElementById('sc-pin-bar');
    const container = document.getElementById('danmaku-container');
    const enterDock = document.getElementById('enter-dock');
    const SHOW_LIKE = Dambo.utils.getQueryBool('show_like', false);

    const MAX_ITEMS = 60;
    const MAX_PIN_CARDS = 4;
    const ENTER_TTL_MS = 3000;
    const FADE_MS = 280;
    const DEFAULT_AVATAR_SVG = Dambo.utils.DEFAULT_AVATAR_SVG;

    const activeTimers = new Set();
    const seenPinKeys = new Set();
    let enterDismissTimer = null;
    let enterRemoveTimer = null;

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
        const identity = document.createElement('span');
        identity.className = 'user-identity';
        if (showAvatar) {
            appendAvatar(identity, item.face);
        }
        appendMedal(identity, item);
        const name = document.createElement('span');
        name.className = 'sender-name';
        name.innerText = item.uname || 'Viewer';
        identity.appendChild(name);
        parentEl.appendChild(identity);
    }

    function appendSpan(parentEl, className, text) {
        const span = document.createElement('span');
        span.className = className;
        span.innerText = text;
        parentEl.appendChild(span);
        return span;
    }

    function appendContentWithEmotes(parentEl, item) {
        Dambo.utils.renderContent(parentEl, item, {
            stickerClass: 'danmaku-sticker',
            emotClass: 'danmaku-emot',
        });
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

    function addPinnedHighlight(item, options) {
        if (!scPinBar || (!Dambo.utils.isSuperChat(item.kind) && item.kind !== 'guard')) return;
        const key = pinnedHighlightKey(item);
        if (seenPinKeys.has(key)) return;

        const totalMs = Dambo.utils.superChatDurationMs(item);
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
        const cnyText = Dambo.utils.formatPrice(item.price);
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

        const cny = Dambo.utils.formatPrice(item.price);
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
        const priceText = Dambo.utils.formatPrice(item.price) || '¥0';
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

        if (Dambo.utils.isSuperChat(kind) || kind === 'guard') {
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
        container.scrollTop = container.scrollHeight;
    }

    const client = Dambo.connect({
        channel: 'feed',
        replaySnapshot: false,
        showLike: SHOW_LIKE,
    });

    client.on('snapshot', (snapshotData) => {
        clearAllState();
        const items = snapshotData?.items;
        if (!Array.isArray(items)) return;

        const now = Date.now();
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
    });

    client.on('event', (item, meta) => {
        renderItem(item, meta || { isSnapshot: false });
    });
})();
