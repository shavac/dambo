(function() {
    const stack = document.getElementById('ticker-stack');
    const seenIds = new Set();

    const searchParams = new URLSearchParams(window.location.search);
    const hasQueryOverride = {
        max_cards: searchParams.has('max_cards'),
        min_duration_sec: searchParams.has('min_duration_sec'),
        high_tier_cny: searchParams.has('high_tier_cny'),
        show_guard: searchParams.has('show_guard'),
        show_medal: searchParams.has('show_medal'),
    };
    const runtimeConfig = {
        maxCards: Math.max(1, Math.min(20, Number(Dambo.utils.getQueryParam('max_cards', '8')) || 8)),
        minDurationMs: Math.max(1000, Math.min(60000, (Number(Dambo.utils.getQueryParam('min_duration_sec', '5')) || 5) * 1000)),
        highTierCny: Math.max(10, Math.min(5000, Number(Dambo.utils.getQueryParam('high_tier_cny', '100')) || 100)),
        showGuard: Dambo.utils.getQueryBool('show_guard', true),
        showMedal: Dambo.utils.getQueryBool('show_medal', true),
    };

    const timerBag = Dambo.utils.createTimerBag();

    function applyPlatformConfig(cfg) {
        if (!cfg || typeof cfg !== 'object') return;
        if (!hasQueryOverride.max_cards && typeof cfg.max_cards === 'number' && Number.isFinite(cfg.max_cards)) {
            runtimeConfig.maxCards = Math.max(1, Math.min(20, Math.floor(cfg.max_cards)));
            while (stack.children.length > runtimeConfig.maxCards) {
                stack.removeChild(stack.lastChild);
            }
        }
        if (!hasQueryOverride.min_duration_sec && typeof cfg.min_duration_sec === 'number' && Number.isFinite(cfg.min_duration_sec)) {
            runtimeConfig.minDurationMs = Math.max(1000, Math.min(60000, Math.floor(cfg.min_duration_sec * 1000)));
        }
        if (!hasQueryOverride.high_tier_cny && typeof cfg.high_tier_cny === 'number' && Number.isFinite(cfg.high_tier_cny)) {
            runtimeConfig.highTierCny = Math.max(10, Math.min(5000, Math.floor(cfg.high_tier_cny)));
        }
        if (!hasQueryOverride.show_guard && typeof cfg.show_guard === 'boolean') {
            runtimeConfig.showGuard = cfg.show_guard;
        }
        if (!hasQueryOverride.show_medal && typeof cfg.show_medal === 'boolean') {
            runtimeConfig.showMedal = cfg.show_medal;
        }
    }

    function clearTickerState() {
        timerBag.clearAll();
        seenIds.clear();
        stack.replaceChildren();
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
        if (item.kind === 'guard' && !runtimeConfig.showGuard) return;
        if (item.id && seenIds.has(item.id)) return;

        const durationMs = Dambo.utils.superChatDurationMs(item);
        const remainingMs = Dambo.utils.remainingDurationMs(item, durationMs, {
            isSnapshot: Boolean(options?.isSnapshot),
            minMs: runtimeConfig.minDurationMs,
        });
        if (remainingMs <= 0) return;
        if (item.id) seenIds.add(item.id);

        const isGuard = item.kind === 'guard';
        const guardInfo = isGuard ? Dambo.utils.formatGuard(item) : null;
        const card = document.createElement('div');
        const cny = (item.price || 0) / 1000;
        const isHigh = cny >= runtimeConfig.highTierCny;
        card.className = `ticker-card ${isGuard ? 'kind-guard' : 'kind-sc'}${isHigh ? ' tier-high' : ''}`;

        const header = document.createElement('div');
        header.className = 'card-header';

        header.appendChild(Dambo.utils.createAvatar(item, 'card-avatar'));

        const meta = document.createElement('div');
        meta.className = 'card-user-meta';

        const uname = document.createElement('span');
        uname.className = 'card-uname';
        uname.textContent = item.uname || 'Viewer';
        meta.appendChild(uname);

        if (runtimeConfig.showMedal && item.medal_name) {
            const medal = document.createElement('span');
            medal.className = 'card-medal';
            medal.textContent = `${item.medal_name} ${item.medal_level || ''}`.trim();
            meta.appendChild(medal);
        }

        header.appendChild(meta);

        const price = document.createElement('span');
        price.className = 'card-price';
        price.textContent = formatPrice(item, guardInfo);
        header.appendChild(price);

        card.appendChild(header);

        const body = document.createElement('div');
        body.className = 'card-body';
        if (isGuard && !item.emoticon) {
            body.textContent = guardInfo.summaryText;
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
        while (stack.children.length > runtimeConfig.maxCards) {
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

    if (Dambo.config && typeof Dambo.config.get === 'function') {
        Dambo.config
            .get()
            .then(applyPlatformConfig)
            .catch(() => {});
    }
    if (Dambo.config && typeof Dambo.config.watch === 'function') {
        Dambo.config.watch(() => {
            Dambo.config
                .get()
                .then(applyPlatformConfig)
                .catch(() => {});
        });
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
