(function () {
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

    const INTERACT_ICONS = {
        guard_enter: '⚓',
        follow: '❤️',
        special_follow: '⭐',
        mutual_follow: '🤝',
        share: '🔗',
        like: '👍',
    };

    const SYSTEM_EVENT_SPECS = {
        live: { icon: '🟢', fallback: '直播开始', alert: false },
        preparing: { icon: '🌙', fallback: '主播下播', alert: false },
        room_change: { icon: '📢', prefix: '房间变更：', fallback: '房间信息变更', alert: false },
        room_silent: { icon: '🔇', fallback: '房间禁言状态变更', alert: false },
        room_block: { icon: '🚫', includeUname: true, fallback: '被禁言', alert: true },
        warning: { icon: '⚠️', fallback: '系统警告', alert: true },
        cut_off: { icon: '⚠️', fallback: '直播已切断', alert: true },
    };

    const timerBag = Dambo.utils.createTimerBag();
    const autoScroll = Dambo.utils.createAutoScroll(container, {
        thresholdPx: SCROLL_BOTTOM_THRESHOLD_PX,
    });
    const comboEntriesByEl = new WeakMap();
    const seenPinKeys = new Set();
    let enterTimers = [];

    function clearEnterTimers() {
        enterTimers.forEach((id) => clearTimeout(id));
        enterTimers = [];
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

    function createSpan(className, text) {
        const span = document.createElement('span');
        span.className = className;
        span.innerText = text;
        return span;
    }

    function appendSpan(parentEl, className, text) {
        const span = createSpan(className, text);
        parentEl.appendChild(span);
        return span;
    }

    function replayClassAnimation(el, className) {
        if (!el) return;
        el.classList.remove(className);
        // Force synchronous reflow so consecutive updates restart CSS animation
        void el.offsetWidth;
        el.classList.add(className);
    }

    function buildUserIdentity(item, options = {}) {
        const showAvatar = options.showAvatar !== false;
        const showMedal = options.showMedal !== false;
        const showGuard = options.showGuard !== false;
        const showColon = Boolean(options.showColon);

        const identity = document.createElement('span');
        identity.className = showColon ? 'user-identity has-colon' : 'user-identity';

        if (showAvatar) {
            if (options.avatarFallbackIcon && !item.face) {
                appendSpan(identity, 'event-icon', options.avatarFallbackIcon);
            } else {
                const avatar = Dambo.utils.createAvatar(item, 'avatar');
                if (options.avatarFallbackIcon) {
                    avatar.onerror = () => {
                        avatar.replaceWith(createSpan('event-icon', options.avatarFallbackIcon));
                    };
                }
                identity.appendChild(avatar);
            }
        }

        if (showMedal && item.medal_name) {
            appendSpan(
                identity,
                'medal-badge',
                `${item.medal_name} ${item.medal_level || ''}`.trim()
            );
        }

        if (showGuard) {
            const guardName = Dambo.utils.guardLevelName(item?.guard_level);
            if (guardName) {
                appendSpan(
                    identity,
                    `guard-role-badge guard-level-${Number(item.guard_level)}`,
                    `⚓ ${guardName}`
                );
            }
        }

        if (options.extraBadge) {
            appendSpan(identity, options.extraBadge.className, options.extraBadge.text);
        }

        appendSpan(identity, 'sender-name', item.uname || item.username || 'Viewer');
        if (showColon) {
            appendSpan(identity, 'name-colon', '：');
        }
        return identity;
    }

    function renderTextOrEmotes(parentEl, item) {
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
        if (delayMs <= 0) return;

        clearEnterTimers();
        enterDock.innerHTML = '';

        const row = document.createElement('div');
        row.className = 'item-enter';
        row.appendChild(
            buildUserIdentity(item, {
                showAvatar: true,
                avatarFallbackIcon: '👋',
            })
        );
        appendSpan(row, 'event-action', Dambo.utils.formatInteract(item).summaryText);
        enterDock.appendChild(row);

        const dismissId = setTimeout(() => {
            if (!row.isConnected) return;
            row.classList.add('fade-out');
            const removeId = setTimeout(() => {
                if (row.parentElement === enterDock) {
                    row.remove();
                }
            }, FADE_MS);
            enterTimers.push(removeId);
        }, delayMs);
        enterTimers.push(dismissId);
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
        header.appendChild(buildUserIdentity(item, { showAvatar: true, showGuard: !isGuard }));
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
                renderTextOrEmotes(body, item);
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

    function shouldRelocateGiftToBottom(el) {
        if (!el || !container || el.parentElement !== container || !el.nextElementSibling) {
            return false;
        }
        let afterCount = 0;
        for (let cur = el.nextElementSibling; cur; cur = cur.nextElementSibling) {
            afterCount++;
            if (afterCount >= RECENT_GIFT_TAIL_LIMIT) return true;
        }
        const cRect = container.getBoundingClientRect();
        const eRect = el.getBoundingClientRect();
        return cRect.height > 0 && eRect.height > 0 && eRect.top < cRect.top - 2;
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

        div.appendChild(
            buildUserIdentity(item, {
                extraBadge: { className: 'event-badge badge-gift', text: '🎁' },
            })
        );
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
        if (entry.coinType !== 'gold' || !entry.data) return;
        const cny = Dambo.utils.formatPrice(entry.totalPriceSeeds);
        if (!cny) return;
        if (!entry.data.priceEl) {
            entry.data.priceEl = appendSpan(entry.data.el, 'price-badge', cny);
        } else {
            entry.data.priceEl.innerText = cny;
        }
    }

    const giftCombos = Dambo.utils.createGiftComboTracker({
        windowMs: GIFT_COMBO_WINDOW_MS,
        useServerComboStayTime: true,
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
                replayClassAnimation(ui.countEl, 'is-pulsing');
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
                    replayClassAnimation(ui.el, 'is-relocated');
                }
            }

            ui.countEl.innerText = formatGiftComboCount(entry.totalCount);
            updateGiftPriceBadge(entry);

            if (!isSnapshot) {
                replayClassAnimation(ui.countEl, 'is-pulsing');
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

    function buildGuardItem(item) {
        const guardInfo = Dambo.utils.formatGuard(item);
        const div = document.createElement('div');
        div.className = 'danmaku-item item-guard';

        div.appendChild(
            buildUserIdentity(item, {
                showGuard: false,
                extraBadge: { className: 'event-badge badge-guard', text: `⚓ ${guardInfo.badgeLabel}` },
            })
        );
        if (guardInfo.actionText) {
            appendSpan(div, 'event-action', guardInfo.actionText);
        }
        appendSpan(div, 'event-highlight', guardInfo.highlightText || '大航海');

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
        header.appendChild(buildUserIdentity(item));
        const priceText = Dambo.utils.formatPrice(item.price) || '¥0';
        appendSpan(header, 'price-badge price-sc', `SC ${priceText}`);
        div.appendChild(header);

        if (item.content || item.emoticon) {
            const body = document.createElement('div');
            body.className = 'sc-body';
            renderTextOrEmotes(body, item);
            div.appendChild(body);
        }
        return div;
    }

    function buildInteractionItem(kind, item) {
        const div = document.createElement('div');
        div.className = `danmaku-item item-interaction item-${kind}`;

        const icon = INTERACT_ICONS[kind] || '👋';
        const skipDuplicateGuardIcon =
            kind === 'guard_enter' && Boolean(Dambo.utils.guardLevelName(item?.guard_level));
        div.appendChild(
            buildUserIdentity(item, {
                extraBadge: skipDuplicateGuardIcon
                    ? null
                    : { className: 'event-icon', text: icon },
            })
        );
        const actionText =
            kind === 'like'
                ? item.content || '为主播点赞了'
                : Dambo.utils.formatInteract(item).summaryText;
        appendSpan(div, 'event-action', actionText);
        return div;
    }

    function buildSystemItem(kind, item) {
        const spec = SYSTEM_EVENT_SPECS[kind] || { icon: '📢', fallback: '', alert: false };
        const div = document.createElement('div');
        div.className = `danmaku-item item-system${spec.alert ? ' item-alert' : ''}`;

        let text = item.content || '';
        if (spec.prefix) {
            text = text ? `${spec.prefix}${text}` : spec.fallback;
        } else if (spec.includeUname) {
            const uname = item.uname ? `${item.uname} ` : '';
            text = `${uname}${text || spec.fallback}`;
        } else {
            text = text || spec.fallback;
        }

        appendSpan(div, 'event-icon', spec.icon);
        appendSpan(div, 'system-text', text);
        return div;
    }

    function buildChatItem(item) {
        const div = document.createElement('div');
        div.className = 'danmaku-item item-chat';

        div.appendChild(buildUserIdentity(item, { showColon: true }));
        const content = document.createElement('span');
        content.className = 'danmaku-content';
        renderTextOrEmotes(content, item);
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
            giftCombos.push(item, options);
            return;
        }

        let div = null;
        if (kind === 'guard') {
            div = buildGuardItem(item);
        } else if (Dambo.utils.isSuperChat(kind)) {
            div = buildSuperChatItem(item);
        } else if (kind in INTERACT_ICONS) {
            div = buildInteractionItem(kind, item);
        } else if (kind in SYSTEM_EVENT_SPECS) {
            div = buildSystemItem(kind, item);
        } else {
            div = buildChatItem(item);
        }

        if (!div) return;
        container.appendChild(div);
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
        items
            .filter((it) => it && it.kind !== 'enter')
            .slice(-MAX_ITEMS)
            .forEach((it) => renderItem(it, { isSnapshot: true }));

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
