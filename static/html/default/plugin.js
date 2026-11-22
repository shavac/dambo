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

    const activeTimers = new Set();
    const activeGiftCombos = new Map();
    const seenPinKeys = new Set();
    let enterDismissTimer = null;
    let enterRemoveTimer = null;
    let autoFollowBottom = true;
    let pendingScrollRaf = null;
    let programmaticScrollLock = false;
    let programmaticScrollUnlockRaf = null;

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

    function isContainerNearBottom() {
        if (!container) return true;
        const maxScrollTop = container.scrollHeight - container.clientHeight;
        if (maxScrollTop <= 0) return true;
        return maxScrollTop - container.scrollTop <= SCROLL_BOTTOM_THRESHOLD_PX;
    }

    function cancelPendingBottomScroll() {
        if (pendingScrollRaf !== null) {
            cancelAnimationFrame(pendingScrollRaf);
            pendingScrollRaf = null;
        }
        if (programmaticScrollUnlockRaf !== null) {
            cancelAnimationFrame(programmaticScrollUnlockRaf);
            programmaticScrollUnlockRaf = null;
        }
        programmaticScrollLock = false;
    }

    function scheduleScrollToBottom() {
        if (!container || !autoFollowBottom) return;
        if (pendingScrollRaf !== null) return;
        pendingScrollRaf = requestAnimationFrame(() => {
            pendingScrollRaf = null;
            if (!container || !autoFollowBottom) return;
            programmaticScrollLock = true;
            container.scrollTop = container.scrollHeight;
            if (programmaticScrollUnlockRaf !== null) {
                cancelAnimationFrame(programmaticScrollUnlockRaf);
            }
            programmaticScrollUnlockRaf = requestAnimationFrame(() => {
                programmaticScrollUnlockRaf = null;
                programmaticScrollLock = false;
            });
        });
    }

    function clearGiftCombos() {
        for (const entry of activeGiftCombos.values()) {
            if (entry.timerId !== null) {
                clearTimeout(entry.timerId);
                entry.timerId = null;
            }
        }
        activeGiftCombos.clear();
    }

    function clearAllState() {
        for (const id of activeTimers) {
            clearTimeout(id);
        }
        activeTimers.clear();
        clearGiftCombos();
        clearEnterTimers();
        cancelPendingBottomScroll();
        autoFollowBottom = true;
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

    if (container) {
        container.addEventListener(
            'wheel',
            (e) => {
                if (e.deltaY < 0) {
                    autoFollowBottom = false;
                    cancelPendingBottomScroll();
                } else if (e.deltaY > 0 && isContainerNearBottom()) {
                    autoFollowBottom = true;
                }
            },
            { passive: true }
        );

        container.addEventListener(
            'touchstart',
            () => {
                if (pendingScrollRaf !== null) {
                    cancelPendingBottomScroll();
                }
            },
            { passive: true }
        );

        container.addEventListener(
            'scroll',
            () => {
                if (programmaticScrollLock || pendingScrollRaf !== null) return;
                autoFollowBottom = isContainerNearBottom();
            },
            { passive: true }
        );
    }

    window.addEventListener('resize', () => {
        if (autoFollowBottom) {
            scheduleScrollToBottom();
        }
    });

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

    function positiveInt(val) {
        const n = Number(val);
        return Number.isFinite(n) && n > 0 ? Math.floor(n) : 0;
    }

    function positiveNum(val) {
        const n = Number(val);
        return Number.isFinite(n) && n > 0 ? n : 0;
    }

    function parseGiftBaseName(item) {
        const explicit = item.gift_name || item.giftName;
        if (typeof explicit === 'string' && explicit.trim()) {
            return explicit.trim();
        }
        const raw = String(item.content || '').trim();
        if (!raw) return '礼物';
        const stripped = raw.replace(/\s*[x×]\s*\d+\s*$/i, '').trim();
        return stripped || raw;
    }

    function formatGiftComboCount(totalCount) {
        return `× ${Math.max(1, Math.floor(totalCount))}`;
    }

    function resolveGiftMeta(item) {
        const rawBatch =
            typeof item.batch_combo_id === 'string' && item.batch_combo_id.trim()
                ? item.batch_combo_id.trim()
                : typeof item.combo_id === 'string' && item.combo_id.trim()
                  ? item.combo_id.trim()
                  : '';
        const batchComboId = rawBatch && rawBatch !== '0' ? rawBatch : '';

        const uidNum = Number(item.uid || 0);
        const rawUname = String(item.uname || item.username || '').trim();
        const userKey =
            Number.isFinite(uidNum) && uidNum > 0
                ? `uid:${Math.floor(uidNum)}`
                : `uname:${rawUname || 'viewer'}`;

        const giftName = parseGiftBaseName(item);
        const rawGiftId =
            item.gift_id !== undefined && item.gift_id !== null
                ? String(item.gift_id).trim()
                : item.giftId !== undefined && item.giftId !== null
                  ? String(item.giftId).trim()
                  : '';
        const giftKey = rawGiftId && rawGiftId !== '0' ? `gid:${rawGiftId}` : `gname:${giftName}`;

        const rawCoin =
            typeof item.coin_type === 'string' ? item.coin_type.trim().toLowerCase() : '';
        const comboTotalCoin = positiveNum(item.combo_total_coin);
        const totalCoin = positiveNum(item.total_coin);
        const itemPrice = positiveNum(item.price);
        const unitPrice = positiveNum(item.unit_price);

        let coinType;
        if (rawCoin === 'silver' || rawCoin === 'free') {
            coinType = 'silver';
        } else if (rawCoin === 'gold' || rawCoin === 'paid') {
            coinType = 'gold';
        } else {
            coinType =
                comboTotalCoin > 0 || totalCoin > 0 || itemPrice > 0 || unitPrice > 0
                    ? 'gold'
                    : 'silver';
        }

        const mergeKey = batchComboId
            ? `batch:${batchComboId}`
            : `combo:${userKey}|${giftKey}|${coinType}`;

        return {
            mergeKey,
            giftName,
            coinType,
            rawNum: positiveInt(item.num ?? item.gift_num ?? item.count),
            rawComboNum: positiveInt(item.combo_num),
            comboTotalCoin,
            totalCoin,
            itemPrice,
            unitPrice,
        };
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

    function expireGiftCombo(entry) {
        if (!entry) return;
        if (entry.timerId !== null) {
            clearTimeout(entry.timerId);
            entry.timerId = null;
        }
        if (activeGiftCombos.get(entry.key) === entry) {
            activeGiftCombos.delete(entry.key);
        }
        if (entry.el && entry.el.dataset && entry.el.dataset.giftComboKey === entry.key) {
            delete entry.el.dataset.giftComboKey;
        }
    }

    function scheduleGiftComboExpiry(entry, eventTimeMs, isSnapshot) {
        if (entry.timerId !== null) {
            clearTimeout(entry.timerId);
            entry.timerId = null;
        }
        let delayMs = GIFT_COMBO_WINDOW_MS;
        if (isSnapshot) {
            delayMs = GIFT_COMBO_WINDOW_MS - (Date.now() - eventTimeMs);
            if (!Number.isFinite(delayMs) || delayMs <= 0) {
                return;
            }
        }
        entry.timerId = setTimeout(() => {
            entry.timerId = null;
            expireGiftCombo(entry);
        }, delayMs);
    }

    function pruneOverflowItems() {
        while (container.children.length > MAX_ITEMS) {
            const oldest = container.firstElementChild;
            if (!oldest) break;
            const comboKey = oldest.dataset ? oldest.dataset.giftComboKey : '';
            if (comboKey) {
                const entry = activeGiftCombos.get(comboKey);
                if (entry && entry.el === oldest) {
                    expireGiftCombo(entry);
                }
            }
            oldest.remove();
        }
    }

    function buildGiftCard(item, meta, initialCount, initialPriceSeeds) {
        const div = document.createElement('div');
        div.className = 'danmaku-item item-gift';

        appendAvatar(div, item.face);
        appendMedal(div, item);
        appendGuardBadge(div, item);
        appendSpan(div, 'event-badge badge-gift', '🎁');
        appendSpan(div, 'sender-name', item.uname || item.username || 'Viewer');
        appendSpan(div, 'event-action', '送出了');
        appendSpan(div, 'event-highlight', meta.giftName);
        const countEl = appendSpan(div, 'gift-combo-count', formatGiftComboCount(initialCount));

        let priceEl = null;
        const cny = meta.coinType === 'gold' ? Dambo.utils.formatPrice(initialPriceSeeds) : '';
        if (cny) {
            priceEl = appendSpan(div, 'price-badge', cny);
        }

        return { div, countEl, priceEl };
    }

    function updateGiftPriceBadge(entry) {
        if (entry.coinType !== 'gold') return;
        const cny = Dambo.utils.formatPrice(entry.totalPriceSeeds);
        if (!cny) return;
        if (!entry.priceEl) {
            entry.priceEl = appendSpan(entry.el, 'price-badge', cny);
        } else {
            entry.priceEl.innerText = cny;
        }
    }

    function computeDeltaForGiftEvent(entry, meta) {
        let deltaCount;
        if (meta.rawComboNum > 0) {
            if (!entry) {
                deltaCount = Math.max(meta.rawComboNum, meta.rawNum || 0);
            } else if (meta.rawComboNum > entry.lastComboNum) {
                deltaCount = meta.rawComboNum - entry.lastComboNum;
            } else {
                deltaCount = meta.rawNum || meta.rawComboNum;
            }
        } else {
            deltaCount = meta.rawNum || 1;
        }

        let deltaPriceSeeds = 0;
        if (meta.coinType === 'gold') {
            if (meta.comboTotalCoin > 0) {
                if (!entry) {
                    deltaPriceSeeds = meta.comboTotalCoin;
                } else if (meta.comboTotalCoin > entry.lastComboTotalCoin) {
                    deltaPriceSeeds = meta.comboTotalCoin - entry.lastComboTotalCoin;
                } else if (meta.totalCoin > 0) {
                    deltaPriceSeeds = meta.totalCoin;
                } else if (meta.unitPrice > 0) {
                    deltaPriceSeeds = meta.unitPrice * deltaCount;
                } else {
                    deltaPriceSeeds = meta.itemPrice;
                }
            } else if (meta.totalCoin > 0) {
                deltaPriceSeeds = meta.totalCoin;
            } else if (meta.unitPrice > 0) {
                deltaPriceSeeds = meta.unitPrice * deltaCount;
            } else {
                deltaPriceSeeds = meta.itemPrice;
            }
        }

        return { deltaCount, deltaPriceSeeds };
    }

    function upsertGiftItem(item, options = {}) {
        const isSnapshot = Boolean(options?.isSnapshot);
        const meta = resolveGiftMeta(item);
        const rawTs = Number(item.timestamp);
        const eventTimeMs = Number.isFinite(rawTs) && rawTs > 0 ? rawTs : Date.now();

        let existing = activeGiftCombos.get(meta.mergeKey);
        if (existing) {
            if (!existing.el || !existing.el.isConnected) {
                expireGiftCombo(existing);
                existing = null;
            } else if (isSnapshot && eventTimeMs - existing.lastEventTimeMs > GIFT_COMBO_WINDOW_MS) {
                expireGiftCombo(existing);
                existing = null;
            }
        }

        if (existing) {
            if (item.id !== undefined && item.id !== null) {
                if (existing.seenIds.has(item.id)) {
                    return;
                }
                if (existing.seenIds.size > 2000) {
                    existing.seenIds.clear();
                }
                existing.seenIds.add(item.id);
            }

            const { deltaCount, deltaPriceSeeds } = computeDeltaForGiftEvent(existing, meta);
            existing.totalCount += deltaCount;
            existing.totalPriceSeeds += deltaPriceSeeds;
            existing.lastEventTimeMs = eventTimeMs;
            if (meta.rawComboNum > 0) {
                existing.lastComboNum = meta.rawComboNum;
            }
            if (meta.comboTotalCoin > 0) {
                existing.lastComboTotalCoin = meta.comboTotalCoin;
            }

            if (shouldRelocateGiftToBottom(existing.el)) {
                container.appendChild(existing.el);
                if (!isSnapshot) {
                    triggerGiftRelocateHighlight(existing.el);
                }
            }

            existing.countEl.innerText = formatGiftComboCount(existing.totalCount);
            updateGiftPriceBadge(existing);

            if (!isSnapshot) {
                triggerGiftComboPulse(existing.countEl);
            }

            scheduleGiftComboExpiry(existing, eventTimeMs, isSnapshot);
            scheduleScrollToBottom();
            return;
        }

        const { deltaCount, deltaPriceSeeds } = computeDeltaForGiftEvent(null, meta);
        const { div, countEl, priceEl } = buildGiftCard(item, meta, deltaCount, deltaPriceSeeds);
        div.dataset.giftComboKey = meta.mergeKey;

        const seenIds = new Set();
        if (item.id !== undefined && item.id !== null) {
            seenIds.add(item.id);
        }

        const entry = {
            key: meta.mergeKey,
            el: div,
            countEl,
            priceEl,
            coinType: meta.coinType,
            totalCount: deltaCount,
            totalPriceSeeds: deltaPriceSeeds,
            lastComboNum: meta.rawComboNum,
            lastComboTotalCoin: meta.comboTotalCoin,
            lastEventTimeMs: eventTimeMs,
            seenIds,
            timerId: null,
        };

        activeGiftCombos.set(meta.mergeKey, entry);
        container.appendChild(div);
        pruneOverflowItems();

        if (entry.el.isConnected) {
            if (!isSnapshot) {
                triggerGiftComboPulse(entry.countEl);
            }
            scheduleGiftComboExpiry(entry, eventTimeMs, isSnapshot);
        }
        scheduleScrollToBottom();
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
        scheduleScrollToBottom();
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

        for (const entry of Array.from(activeGiftCombos.values())) {
            if (entry.timerId === null) {
                expireGiftCombo(entry);
            }
        }

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
