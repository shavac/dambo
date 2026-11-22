(function() {
    const scPinBar = document.getElementById('sc-pin-bar');
    const container = document.getElementById('danmaku-container');
    const enterDock = document.getElementById('enter-dock');
    const SHOW_LIKE = Dambo.utils.getQueryBool('show_like', false);

    const MAX_ITEMS = 60;
    const MAX_PIN_CARDS = 4;
    const ENTER_TTL_MS = 3000;
    const GIFT_COMBO_WINDOW_MS = 4000;
    const RECENT_GIFT_TAIL_LIMIT = 8;
    const SCROLL_BOTTOM_THRESHOLD_PX = 24;
    const FADE_MS = 280;
    const DEFAULT_AVATAR_SVG = Dambo.utils.DEFAULT_AVATAR_SVG;

    const timerBag = Dambo.utils.createTimerBag();
    const autoScroll = Dambo.utils.createAutoScroll(container, {
        thresholdPx: SCROLL_BOTTOM_THRESHOLD_PX,
    });
    const comboEntriesByEl = new WeakMap();
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
        timerBag.clearAll();
        giftCombos.clear();
        clearEnterTimers();
        autoScroll.reset();
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
        parentEl.appendChild(Dambo.utils.createAvatar(faceUrl, 'avatar'));
    }

    function appendMedal(parentEl, item) {
        if (!item.medal_name) return;
        const medal = document.createElement('span');
        medal.className = 'medal-badge';
        medal.innerText = `${item.medal_name} ${item.medal_level || ''}`.trim();
        parentEl.appendChild(medal);
    }

    function appendGuardBadge(parentEl, item) {
        const guardName = Dambo.utils.guardLevelName(item?.guard_level);
        if (!guardName) return false;
        const badge = document.createElement('span');
        badge.className = `guard-role-badge guard-level-${Number(item.guard_level)}`;
        badge.innerText = `⚓ ${guardName}`;
        parentEl.appendChild(badge);
        return true;
    }

    function appendUserIdentity(parentEl, item, options = {}) {
        const showAvatar = options !== false && options?.showAvatar !== false;
        const showGuard = options?.showGuard !== false;
        const showColon = Boolean(options?.showColon);

        const identity = document.createElement('span');
        identity.className = showColon ? 'user-identity has-colon' : 'user-identity';
        if (showAvatar) {
            appendAvatar(identity, item.face);
        }
        appendMedal(identity, item);
        if (showGuard) {
            appendGuardBadge(identity, item);
        }
        const name = document.createElement('span');
        name.className = 'sender-name';
        name.innerText = item.uname || 'Viewer';
        identity.appendChild(name);
        if (showColon) {
            appendSpan(identity, 'name-colon', '：');
        }
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

        const delayMs = Dambo.utils.remainingDurationMs(item, ENTER_TTL_MS, {
            isSnapshot: Boolean(options?.isSnapshot),
        });
        if (delayMs <= 0) {
            return;
        }

        clearEnterTimers();
        enterDock.innerHTML = '';

        const div = document.createElement('div');
        div.className = 'danmaku-item item-enter';

        if (item.face) {
            const avatar = Dambo.utils.createAvatar(item.face, 'avatar');
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
        appendGuardBadge(div, item);
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
        const remainingMs = Dambo.utils.remainingDurationMs(item, totalMs, {
            isSnapshot: Boolean(options?.isSnapshot),
            minMs: 3000,
        });
        if (remainingMs <= 0) return;
        seenPinKeys.add(key);

        const isGuard = item.kind === 'guard';
        const guardInfo = isGuard ? Dambo.utils.formatGuard(item) : null;
        const card = document.createElement('div');
        card.className = `sc-pin-card ${isGuard ? 'kind-guard' : 'kind-sc'}`;

        const header = document.createElement('div');
        header.className = 'sc-pin-header';
        appendUserIdentity(header, item, { showAvatar: true, showGuard: !isGuard });
        const cnyText = Dambo.utils.formatPrice(item.price);
        const badgeText = isGuard
            ? (cnyText ? `⚓ ${guardInfo.badgeLabel} ${cnyText}` : `⚓ ${guardInfo.badgeLabel}`)
            : `SC ${cnyText || '¥0'}`;
        appendSpan(header, 'price-badge price-sc', badgeText);
        card.appendChild(header);

        const bodyText = isGuard ? guardInfo.summaryText : (item.content || '');
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

        timerBag.set(() => {
            if (!card.isConnected) return;
            card.classList.add('fade-out');
            timerBag.set(() => {
                if (card.isConnected) {
                    card.remove();
                }
                if (scPinBar.children.length === 0) {
                    scPinBar.classList.add('hidden');
                }
            }, FADE_MS);
        }, remainingMs);
    }

    function formatGiftComboCount(totalCount) {
        return `× ${Math.max(1, Math.floor(totalCount))}`;
    }

    function triggerGiftComboPulse(el) {
        if (!el) return;
        el.classList.remove('is-pulsing');
        // Force synchronous reflow so consecutive combos replay the pulse animation
        void el.offsetWidth;
        el.classList.add('is-pulsing');
    }

    function triggerGiftRelocateHighlight(el) {
        if (!el) return;
        el.classList.remove('is-relocated');
        void el.offsetWidth;
        el.classList.add('is-relocated');
    }

    function countItemsAfter(el) {
        let count = 0;
        let cur = el.nextElementSibling;
        while (cur) {
            count++;
            cur = cur.nextElementSibling;
        }
        return count;
    }

    function shouldRelocateGiftToBottom(el) {
        if (!el || !container || el.parentElement !== container) return false;
        if (!el.nextElementSibling) return false;
        if (countItemsAfter(el) >= RECENT_GIFT_TAIL_LIMIT) return true;

        const cRect = container.getBoundingClientRect();
        const eRect = el.getBoundingClientRect();
        if (cRect.height > 0 && eRect.height > 0 && eRect.top < cRect.top - 2) {
            return true;
        }
        return false;
    }

    function pruneOverflowItems() {
        while (container.children.length > MAX_ITEMS) {
            const oldest = container.firstElementChild;
            if (!oldest) break;
            const comboKey = oldest.dataset ? oldest.dataset.giftComboKey : '';
            const trackedEntry = comboEntriesByEl.get(oldest);
            oldest.remove();
            if (comboKey && trackedEntry) {
                giftCombos.expire(comboKey, trackedEntry);
            }
        }
    }

    function buildGiftCard(item, giftName, coinType, initialCount, initialPriceSeeds) {
        const div = document.createElement('div');
        div.className = 'danmaku-item item-gift';

        appendAvatar(div, item.face);
        appendMedal(div, item);
        appendGuardBadge(div, item);
        appendSpan(div, 'event-badge badge-gift', '🎁');
        appendSpan(div, 'sender-name', item.uname || item.username || 'Viewer');
        appendSpan(div, 'event-action', '送出了');
        appendSpan(div, 'event-highlight', giftName);
        const countEl = appendSpan(div, 'gift-combo-count', formatGiftComboCount(initialCount));

        let priceEl = null;
        const cny = coinType === 'gold' ? Dambo.utils.formatPrice(initialPriceSeeds) : '';
        if (cny) {
            priceEl = appendSpan(div, 'price-badge', cny);
        }

        return { el: div, countEl, priceEl };
    }

    function updateGiftPriceBadge(entry) {
        if (entry.coinType !== 'gold') return;
        const ui = entry.data;
        if (!ui) return;
        const cny = Dambo.utils.formatPrice(entry.totalPriceSeeds);
        if (!cny) return;
        if (!ui.priceEl) {
            ui.priceEl = appendSpan(ui.el, 'price-badge', cny);
        } else {
            ui.priceEl.innerText = cny;
        }
    }

    const giftCombos = Dambo.utils.createGiftComboTracker({
        windowMs: GIFT_COMBO_WINDOW_MS,
        canMerge: (entry) => Boolean(entry.data && entry.data.el && entry.data.el.isConnected),
        onCreate: (entry, item, opts) => {
            const isSnapshot = Boolean(opts?.isSnapshot);
            const ui = buildGiftCard(
                item,
                entry.giftName,
                entry.coinType,
                entry.totalCount,
                entry.totalPriceSeeds
            );
            ui.el.dataset.giftComboKey = entry.key;
            comboEntriesByEl.set(ui.el, entry);
            entry.data = ui;

            container.appendChild(ui.el);
            pruneOverflowItems();

            if (ui.el.isConnected && !isSnapshot) {
                triggerGiftComboPulse(ui.countEl);
            }
            autoScroll.schedule();
        },
        onUpdate: (entry, _delta, _item, opts) => {
            const ui = entry.data;
            if (!ui || !ui.el.isConnected) return;
            const isSnapshot = Boolean(opts?.isSnapshot);

            if (shouldRelocateGiftToBottom(ui.el)) {
                container.appendChild(ui.el);
                if (!isSnapshot) {
                    triggerGiftRelocateHighlight(ui.el);
                }
            }

            ui.countEl.innerText = formatGiftComboCount(entry.totalCount);
            updateGiftPriceBadge(entry);

            if (!isSnapshot) {
                triggerGiftComboPulse(ui.countEl);
            }
            autoScroll.schedule();
        },
        onExpire: (entry) => {
            const ui = entry.data;
            if (ui && ui.el && ui.el.dataset && ui.el.dataset.giftComboKey === entry.key) {
                delete ui.el.dataset.giftComboKey;
            }
        },
    });

    function upsertGiftItem(item, options = {}) {
        giftCombos.push(item, options);
    }

    function buildHighlightedEvent(item, spec) {
        const div = document.createElement('div');
        div.className = `danmaku-item item-${spec.kind}`;

        appendAvatar(div, item.face);
        appendMedal(div, item);
        if (spec.kind !== 'guard') {
            appendGuardBadge(div, item);
        }
        appendSpan(div, `event-badge ${spec.badgeClass}`, spec.badgeText);
        appendSpan(div, 'sender-name', item.uname || 'Viewer');
        if (spec.actionText) {
            appendSpan(div, 'event-action', spec.actionText);
        }
        appendSpan(
            div,
            'event-highlight',
            spec.highlightText !== undefined
                ? spec.highlightText
                : (item.content || spec.fallbackContent)
        );

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
        appendUserIdentity(header, item, { showAvatar: true });
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
        const hasGuard = appendGuardBadge(div, item);
        if (!(kind === 'guard_enter' && hasGuard)) {
            appendSpan(div, 'event-icon', icon);
        }
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

        appendUserIdentity(div, item, { showAvatar: true, showColon: true });

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

        if (kind === 'gift') {
            upsertGiftItem(item, options);
            return;
        }

        let div = null;
        switch (kind) {
            case 'guard': {
                const guardInfo = Dambo.utils.formatGuard(item);
                div = buildHighlightedEvent(item, {
                    kind: 'guard',
                    badgeClass: 'badge-guard',
                    badgeText: `⚓ ${guardInfo.badgeLabel}`,
                    actionText: guardInfo.actionText,
                    highlightText: guardInfo.highlightText,
                    fallbackContent: '大航海',
                });
                break;
            }
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
        pruneOverflowItems();
        autoScroll.schedule();
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

        giftCombos.pruneExpiredSnapshots();

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
