(function () {
    'use strict';

    // =========================================================================
    // 1. 配置与 URL 查询参数（严格零硬编码地址/端口，遵循 Dambo 插件规范）
    // =========================================================================
    const ROUND_TIMEOUT_SEC = Math.max(0, Number(Dambo.utils.getQueryParam('timeout', '300')) || 0);
    const SOLVABLE_ONLY = Dambo.utils.getQueryBool('solvable_only', false);
    const SETTLE_HOLD_MS = Math.max(1000, Math.round((Number(Dambo.utils.getQueryParam('settle_time', '3.5')) || 3.5) * 1000));
    const UNSOLVABLE_HOLD_MS = Math.max(1000, Math.round((Number(Dambo.utils.getQueryParam('skip_time', '2.5')) || 2.5) * 1000));
    const USER_COOLDOWN_MS = 400;

    const SUITS = [
        { symbol: '♠', color: 'suit-black' },
        { symbol: '♥', color: 'suit-red' },
        { symbol: '♣', color: 'suit-black' },
        { symbol: '♦', color: 'suit-red' },
    ];

    const RANK_LABELS = {
        1: 'A',
        2: '2',
        3: '3',
        4: '4',
        5: '5',
        6: '6',
        7: '7',
        8: '8',
        9: '9',
        10: '10',
        11: 'J',
        12: 'Q',
        13: 'K',
    };

    // =========================================================================
    // 2. DOM 元素引用（纯展示覆盖层，零交互按钮/表单控件）
    // =========================================================================
    const roundLabelEl = document.getElementById('round-label');
    const connBadgeEl = document.getElementById('conn-badge');
    const timerFillEl = document.getElementById('timer-fill');
    const bannerEl = document.getElementById('banner');
    const bannerAvatarEl = document.getElementById('banner-avatar');
    const bannerTitleEl = document.getElementById('banner-title');
    const bannerSubEl = document.getElementById('banner-sub');
    const cardsStageEl = document.getElementById('cards-stage');
    const toastBarEl = document.getElementById('toast-bar');
    const lbListEl = document.getElementById('lb-list');
    const statsBadgeEl = document.getElementById('stats-badge');

    // =========================================================================
    // 3. 精确有理数（分数）运算库 —— 避免 8/(3-8/3)=24 这类除法浮点精度误差
    // =========================================================================
    function gcd(a, b) {
        let x = Math.abs(a);
        let y = Math.abs(b);
        while (y !== 0) {
            const t = x % y;
            x = y;
            y = t;
        }
        return x === 0 ? 1 : x;
    }

    function makeRat(n, d) {
        if (d === 0) return null;
        if (n === 0) return { n: 0, d: 1 };
        const sign = d < 0 ? -1 : 1;
        const g = gcd(n, d);
        return {
            n: (sign * n) / g,
            d: Math.abs(d) / g,
        };
    }

    function ratAdd(a, b) {
        return makeRat(a.n * b.d + b.n * a.d, a.d * b.d);
    }

    function ratSub(a, b) {
        return makeRat(a.n * b.d - b.n * a.d, a.d * b.d);
    }

    function ratMul(a, b) {
        return makeRat(a.n * b.n, a.d * b.d);
    }

    function ratDiv(a, b) {
        if (b.n === 0) return null;
        return makeRat(a.n * b.d, a.d * b.n);
    }

    function isTarget24(r) {
        return r !== null && r.d === 1 && r.n === 24;
    }

    function formatRational(r) {
        if (!r) return 'NaN';
        return r.d === 1 ? String(r.n) : `${r.n}/${r.d}`;
    }

    // =========================================================================
    // 4. 24点全解求解器（自动判断是否有解，并生成最简参考答案）
    // =========================================================================
    function valueToCardText(val) {
        return RANK_LABELS[val] || String(val);
    }

    function stripOuterParens(expr) {
        if (!expr || expr[0] !== '(' || expr[expr.length - 1] !== ')') return expr;
        let depth = 0;
        for (let i = 0; i < expr.length - 1; i++) {
            if (expr[i] === '(') depth++;
            else if (expr[i] === ')') depth--;
            if (depth === 0) return expr; // 括号在末尾之前已闭合（如 (A+B)*(C+D)），不可去除
        }
        return expr.slice(1, -1);
    }

    /**
     * 求解给定 4 个扑克牌点数（1..13）的所有可行 24 点表达式
     * @param {number[]} nums
     * @returns {string[]}
     */
    function solve24(nums) {
        const solutions = new Set();

        function search(items) {
            if (items.length === 1) {
                if (isTarget24(items[0].val)) {
                    solutions.add(stripOuterParens(items[0].expr));
                }
                return;
            }

            for (let i = 0; i < items.length; i++) {
                for (let j = 0; j < items.length; j++) {
                    if (i === j) continue;

                    const rest = [];
                    for (let k = 0; k < items.length; k++) {
                        if (k !== i && k !== j) rest.push(items[k]);
                    }

                    const a = items[i];
                    const b = items[j];

                    // 加法与乘法满足交换律，仅在 i < j 时计算以削减冗余分支
                    if (i < j) {
                        const addVal = ratAdd(a.val, b.val);
                        if (addVal) {
                            search(rest.concat({
                                val: addVal,
                                expr: `(${a.expr} + ${b.expr})`,
                            }));
                        }

                        const mulVal = ratMul(a.val, b.val);
                        if (mulVal) {
                            search(rest.concat({
                                val: mulVal,
                                expr: `(${a.expr} × ${b.expr})`,
                            }));
                        }
                    }

                    const subVal = ratSub(a.val, b.val);
                    if (subVal) {
                        search(rest.concat({
                            val: subVal,
                            expr: `(${a.expr} - ${b.expr})`,
                        }));
                    }

                    const divVal = ratDiv(a.val, b.val);
                    if (divVal) {
                        search(rest.concat({
                            val: divVal,
                            expr: `(${a.expr} ÷ ${b.expr})`,
                        }));
                    }
                }
            }
        }

        const initial = nums.map((n) => ({
            val: makeRat(n, 1),
            expr: valueToCardText(n),
        }));
        search(initial);

        return Array.from(solutions).sort((x, y) => x.length - y.length);
    }

    // =========================================================================
    // 5. 弹幕算式词法分析与递归下降语法解析器
    // =========================================================================
    function normalizeInput(raw) {
        if (typeof raw !== 'string') return '';
        let s = raw.trim();

        // 全角数字与常见全角符号转半角
        s = s.replace(/[０-９]/g, (ch) => String.fromCharCode(ch.charCodeAt(0) - 0xfee0));
        s = s
            .replace(/[＋]/g, '+')
            .replace(/[－—–]/g, '-')
            .replace(/[×＊xX]/g, '*')
            .replace(/[÷／]/g, '/')
            .replace(/[（]/g, '(')
            .replace(/[）]/g, ')')
            .replace(/[＝]/g, '=');

        // 允许观众在末尾输入 "= 24" 或 "=24"；若附带其他等式结果直接判定为非本轮解
        const eqIdx = s.indexOf('=');
        if (eqIdx !== -1) {
            const rhs = s.slice(eqIdx + 1).trim();
            if (rhs !== '24') {
                return '';
            }
            s = s.slice(0, eqIdx).trim();
        }

        return s;
    }

    /**
     * 将标准化算式切分为 Token 流
     * @param {string} expr
     */
    function tokenizeExpression(expr) {
        if (!expr) return null;
        const tokens = [];
        let hasOperator = false;
        let i = 0;

        while (i < expr.length) {
            const ch = expr[i];

            if (/\s/.test(ch)) {
                i++;
                continue;
            }

            if (ch === '+' || ch === '-' || ch === '*' || ch === '/') {
                hasOperator = true;
                tokens.push({ type: 'OP', value: ch });
                i++;
                continue;
            }

            if (ch === '(') {
                tokens.push({ type: 'LPAREN', value: '(' });
                i++;
                continue;
            }

            if (ch === ')') {
                tokens.push({ type: 'RPAREN', value: ')' });
                i++;
                continue;
            }

            // 数字 1..13
            if (/[0-9]/.test(ch)) {
                let numStr = '';
                while (i < expr.length && /[0-9]/.test(expr[i])) {
                    numStr += expr[i];
                    i++;
                }
                // 禁止数字与字母紧连（例如 "2a" 或 "10k"）
                if (i < expr.length && /[A-Za-z]/.test(expr[i])) {
                    return null;
                }
                const val = Number(numStr);
                if (!Number.isInteger(val) || val < 1 || val > 13) {
                    return { error: '只能使用 1~13（A~K）的数字' };
                }
                tokens.push({ type: 'NUM', value: val, raw: numStr });
                continue;
            }

            // 牌面字母 A / J / Q / K（大小写兼容）
            if (/[A-Za-z]/.test(ch)) {
                let word = '';
                while (i < expr.length && /[A-Za-z0-9]/.test(expr[i])) {
                    word += expr[i];
                    i++;
                }
                const upper = word.toUpperCase();
                if (upper === 'A') {
                    tokens.push({ type: 'NUM', value: 1, raw: 'A' });
                } else if (upper === 'J') {
                    tokens.push({ type: 'NUM', value: 11, raw: 'J' });
                } else if (upper === 'Q') {
                    tokens.push({ type: 'NUM', value: 12, raw: 'Q' });
                } else if (upper === 'K') {
                    tokens.push({ type: 'NUM', value: 13, raw: 'K' });
                } else {
                    // 普通英文聊天词汇（如 lol / gg），静默忽略
                    return null;
                }
                continue;
            }

            // 包含非算术字符或中文，静默忽略
            return null;
        }

        // 必须含有运算符且具备基本算式长度
        if (!hasOperator || tokens.length < 3) {
            return null;
        }

        return { tokens };
    }

    /**
     * 校验观众弹幕算式：
     * - ignore: true 普通聊天，无需反馈
     * - valid: true 计算恰为 24 且使用的 4 张牌完全吻合
     * - valid: false 算错、用牌不符或语法错误
     */
    function validateCandidate(rawContent, targetRanks) {
        const normalized = normalizeInput(rawContent);
        const tokResult = tokenizeExpression(normalized);
        if (!tokResult) {
            return { ignore: true };
        }
        if (tokResult.error) {
            return { ignore: false, valid: false, reason: tokResult.error };
        }

        const tokens = tokResult.tokens;
        let pos = 0;
        const usedNums = [];
        let divByZero = false;

        function parseExpr() {
            let left = parseTerm();
            if (!left) return null;

            while (pos < tokens.length && tokens[pos].type === 'OP' && (tokens[pos].value === '+' || tokens[pos].value === '-')) {
                const op = tokens[pos].value;
                pos++;
                const right = parseTerm();
                if (!right) return null;
                left = op === '+' ? ratAdd(left, right) : ratSub(left, right);
                if (!left) return null;
            }
            return left;
        }

        function parseTerm() {
            let left = parseFactor();
            if (!left) return null;

            while (pos < tokens.length && tokens[pos].type === 'OP' && (tokens[pos].value === '*' || tokens[pos].value === '/')) {
                const op = tokens[pos].value;
                pos++;
                const right = parseFactor();
                if (!right) return null;
                if (op === '*') {
                    left = ratMul(left, right);
                } else {
                    const q = ratDiv(left, right);
                    if (!q) {
                        divByZero = true;
                        return null;
                    }
                    left = q;
                }
                if (!left) return null;
            }
            return left;
        }

        function parseFactor() {
            if (pos >= tokens.length) return null;
            const tok = tokens[pos];

            if (tok.type === 'NUM') {
                pos++;
                usedNums.push(tok.value);
                return makeRat(tok.value, 1);
            }

            if (tok.type === 'LPAREN') {
                pos++;
                const inner = parseExpr();
                if (!inner) return null;
                if (pos >= tokens.length || tokens[pos].type !== 'RPAREN') {
                    return null;
                }
                pos++;
                return inner;
            }

            return null;
        }

        const result = parseExpr();
        if (divByZero) {
            return { ignore: false, valid: false, reason: '除数不能为 0' };
        }
        if (!result || pos !== tokens.length) {
            return { ignore: false, valid: false, reason: '算式语法有误' };
        }

        // 必须恰好使用 4 张牌
        if (usedNums.length !== targetRanks.length) {
            return { ignore: false, valid: false, reason: `必须恰好使用 4 张牌（当前用了 ${usedNums.length} 个数）` };
        }

        const sortedUsed = usedNums.slice().sort((a, b) => a - b);
        const sortedTarget = targetRanks.slice().sort((a, b) => a - b);
        for (let i = 0; i < 4; i++) {
            if (sortedUsed[i] !== sortedTarget[i]) {
                return { ignore: false, valid: false, reason: '所用数字与当前牌面不一致' };
            }
        }

        if (!isTarget24(result)) {
            return {
                ignore: false,
                valid: false,
                reason: `计算结果为 ${formatRational(result)} ≠ 24`,
            };
        }

        const prettyExpr = normalized
            .replace(/\*/g, '×')
            .replace(/\//g, '÷');

        return {
            ignore: false,
            valid: true,
            prettyExpr,
        };
    }

    // =========================================================================
    // 6. 游戏状态管理与扑克牌 DOM 渲染
    // =========================================================================
    const gameState = {
        round: 0,
        solvedCount: 0,
        phase: 'INIT', // 'PLAYING' | 'UNSOLVABLE' | 'SOLVED' | 'TIMEOUT'
        cards: [],     // [{ rank: 1..13, suit: { symbol, color } }]
        solutions: [], // string[]
        phaseStartMs: 0,
        phaseDurationMs: 0,
        tickTimerId: null,
        scores: new Map(), // key -> { uname, score, face }
        userLastSubmit: new Map(),
    };

    function drawFourCards() {
        const deck = [];
        for (let rank = 1; rank <= 13; rank++) {
            for (let s = 0; s < SUITS.length; s++) {
                deck.push({ rank, suit: SUITS[s] });
            }
        }
        for (let i = deck.length - 1; i > 0; i--) {
            const j = Math.floor(Math.random() * (i + 1));
            const tmp = deck[i];
            deck[i] = deck[j];
            deck[j] = tmp;
        }
        return deck.slice(0, 4);
    }

    function renderCards(cards, solved) {
        cardsStageEl.replaceChildren();

        cards.forEach((card, idx) => {
            const rankText = RANK_LABELS[card.rank];
            const el = document.createElement('div');
            el.className = `poker-card ${card.suit.color}${solved ? ' solved-glow' : ''}`;
            el.style.animationDelay = `${idx * 65}ms`;

            // 左上角牌面与花色
            const topCorner = document.createElement('div');
            topCorner.className = 'card-corner';
            const topRank = document.createElement('span');
            topRank.className = 'card-rank';
            topRank.textContent = rankText;
            const topSuit = document.createElement('span');
            topSuit.className = 'card-suit-sm';
            topSuit.textContent = card.suit.symbol;
            topCorner.appendChild(topRank);
            topCorner.appendChild(topSuit);
            el.appendChild(topCorner);

            // A / J / Q / K 右上角点数明确标识（免去观众心算转换认知负担）
            if (card.rank === 1 || card.rank >= 11) {
                const tag = document.createElement('span');
                tag.className = 'card-royal-tag';
                tag.textContent = `${rankText}=${card.rank}`;
                el.appendChild(tag);
            }

            // 中央花色主视觉
            const center = document.createElement('div');
            center.className = 'card-center';
            const suitLg = document.createElement('span');
            suitLg.className = 'card-suit-lg';
            suitLg.textContent = card.suit.symbol;
            center.appendChild(suitLg);
            el.appendChild(center);

            // 右下角倒置角标
            const bottomCorner = document.createElement('div');
            bottomCorner.className = 'card-corner bottom';
            const botRank = document.createElement('span');
            botRank.className = 'card-rank';
            botRank.textContent = rankText;
            const botSuit = document.createElement('span');
            botSuit.className = 'card-suit-sm';
            botSuit.textContent = card.suit.symbol;
            bottomCorner.appendChild(botRank);
            bottomCorner.appendChild(botSuit);
            el.appendChild(bottomCorner);

            cardsStageEl.appendChild(el);
        });
    }

    function setBanner(phaseClass, titleText, subText, avatarItem) {
        bannerEl.className = `banner ${phaseClass}`;
        bannerTitleEl.textContent = titleText;
        bannerSubEl.textContent = subText;

        if (avatarItem) {
            const avatarImg = Dambo.utils.createAvatar(avatarItem, 'banner-avatar');
            bannerAvatarEl.src = avatarImg.src;
            bannerAvatarEl.classList.remove('hidden');
        } else {
            bannerAvatarEl.classList.add('hidden');
            bannerAvatarEl.removeAttribute('src');
        }
    }

    function setToast(text, tone) {
        toastBarEl.textContent = text;
        toastBarEl.className = tone ? `toast-bar ${tone}` : 'toast-bar';
    }

    function updateStats() {
        statsBadgeEl.textContent = `已开 ${gameState.round} 局 · 答对 ${gameState.solvedCount} 局`;
    }

    function updateLeaderboard(player) {
        const key = player.uid && player.uid > 0 ? `uid:${player.uid}` : `name:${player.uname}`;
        const prev = gameState.scores.get(key) || { uname: player.uname, score: 0 };
        prev.uname = player.uname || prev.uname;
        prev.score += 1;
        gameState.scores.set(key, prev);

        const top = Array.from(gameState.scores.values())
            .sort((a, b) => b.score - a.score)
            .slice(0, 4);

        lbListEl.replaceChildren();
        if (top.length === 0) {
            const empty = document.createElement('span');
            empty.className = 'lb-empty';
            empty.textContent = '暂无得分';
            lbListEl.appendChild(empty);
            return;
        }

        top.forEach((entry, idx) => {
            const chip = document.createElement('span');
            chip.className = 'lb-chip';

            const nameSpan = document.createElement('span');
            nameSpan.className = 'lb-chip-name';
            nameSpan.textContent = `#${idx + 1} ${entry.uname}`;

            const scoreSpan = document.createElement('span');
            scoreSpan.className = 'lb-chip-score';
            scoreSpan.textContent = `${entry.score}分`;

            chip.appendChild(nameSpan);
            chip.appendChild(scoreSpan);
            lbListEl.appendChild(chip);
        });
    }

    function stopTickLoop() {
        if (gameState.tickTimerId !== null) {
            clearInterval(gameState.tickTimerId);
            gameState.tickTimerId = null;
        }
    }

    function startPhaseTimer(durationMs, onTick, onComplete) {
        stopTickLoop();
        gameState.phaseStartMs = Date.now();
        gameState.phaseDurationMs = durationMs;

        if (durationMs <= 0) {
            timerFillEl.style.width = '100%';
            timerFillEl.className = 'timer-fill';
            return;
        }

        gameState.tickTimerId = setInterval(() => {
            const elapsed = Date.now() - gameState.phaseStartMs;
            const remaining = Math.max(0, durationMs - elapsed);
            const ratio = Math.max(0, Math.min(1, remaining / durationMs));
            timerFillEl.style.width = `${(ratio * 100).toFixed(1)}%`;

            if (onTick) onTick(remaining, ratio);

            if (remaining <= 0) {
                stopTickLoop();
                if (onComplete) onComplete();
            }
        }, 120);
    }

    // =========================================================================
    // 7. 核心游戏循环控制（全自动化发牌 -> 无解切题 / 弹幕抢答 -> 结算）
    // =========================================================================
    function startNextRound() {
        stopTickLoop();
        gameState.round += 1;
        roundLabelEl.textContent = `第 ${gameState.round} 局`;
        updateStats();

        let cards = drawFourCards();
        let solutions = solve24(cards.map((c) => c.rank));

        // 若开启 solvable_only=true，则自动跳过无解手牌直至抽中有解牌组
        if (SOLVABLE_ONLY) {
            let guard = 0;
            while (solutions.length === 0 && guard < 120) {
                cards = drawFourCards();
                solutions = solve24(cards.map((c) => c.rank));
                guard++;
            }
        }

        gameState.cards = cards;
        gameState.solutions = solutions;
        renderCards(cards, false);

        const handSummary = cards.map((c) => `${c.suit.symbol}${RANK_LABELS[c.rank]}`).join('  ');

        // 情况 A：当前手牌无可行四则运算解 -> 自动提示无解并跳过
        if (solutions.length === 0) {
            gameState.phase = 'UNSOLVABLE';
            timerFillEl.className = 'timer-fill auto-skip';
            timerFillEl.style.width = '100%';
            setBanner(
                'state-unsolvable',
                `本局牌面（${handSummary}）无解！`,
                '系统校验无可行 24 点解法，正在自动切换下一题…',
                null
            );
            setToast('⚠️ 本手牌无解，即将自动发下一题', 'warn');

            startPhaseTimer(
                UNSOLVABLE_HOLD_MS,
                (remMs) => {
                    const sec = (remMs / 1000).toFixed(1);
                    bannerSubEl.textContent = `本手牌无解，${sec}s 后自动切换下一题…`;
                },
                () => {
                    startNextRound();
                }
            );
            return;
        }

        // 情况 B：当前牌面有解 -> 开启弹幕抢答阶段
        gameState.phase = 'PLAYING';
        timerFillEl.className = 'timer-fill';
        timerFillEl.style.width = '100%';

        const timeoutDesc = ROUND_TIMEOUT_SEC > 0 ? `限时 ${ROUND_TIMEOUT_SEC}s` : '不限时';
        setBanner(
            'state-playing',
            `当前牌面：${handSummary}（共 ${solutions.length} 种解法）`,
            `${timeoutDesc} · 弹幕发送算式抢答！首位解出自动切题`,
            null
        );
        setToast('💡 弹幕发送如 (3+3)×(8-4) 即可作答，首位算对自动切下一题', '');

        if (ROUND_TIMEOUT_SEC > 0) {
            startPhaseTimer(
                ROUND_TIMEOUT_SEC * 1000,
                (remMs) => {
                    const sec = Math.ceil(remMs / 1000);
                    bannerSubEl.textContent = `剩余 ${sec}s · 弹幕发送算式抢答！`;
                    if (sec <= 10) {
                        timerFillEl.classList.add('warning');
                    }
                },
                () => {
                    handleRoundTimeout();
                }
            );
        }
    }

    function handleRoundSolved(player, prettyExpr) {
        if (gameState.phase !== 'PLAYING') return;
        gameState.phase = 'SOLVED';
        gameState.solvedCount += 1;
        updateStats();

        renderCards(gameState.cards, true);
        updateLeaderboard(player);

        timerFillEl.className = 'timer-fill';
        timerFillEl.style.width = '100%';
        setBanner(
            'state-solved',
            `🎉 ${player.uname} 抢答成功：${prettyExpr} = 24`,
            '3.5s 后自动切换下一题…',
            player
        );
        setToast(`✅ ${player.uname} 解出 ${prettyExpr} = 24（+1分）`, 'success');

        startPhaseTimer(
            SETTLE_HOLD_MS,
            (remMs) => {
                const sec = (remMs / 1000).toFixed(1);
                bannerSubEl.textContent = `${sec}s 后自动切换下一题…`;
            },
            () => {
                startNextRound();
            }
        );
    }

    function handleRoundTimeout() {
        if (gameState.phase !== 'PLAYING') return;
        gameState.phase = 'TIMEOUT';

        const sample = gameState.solutions[0] || '无';
        timerFillEl.className = 'timer-fill warning';
        timerFillEl.style.width = '100%';
        setBanner(
            'state-timeout',
            `⏰ 时间到！参考解法：${sample} = 24`,
            '3.5s 后自动切换下一题…',
            null
        );
        setToast(`📖 本轮无人解出，参考解：${sample} = 24`, 'warn');

        startPhaseTimer(
            SETTLE_HOLD_MS,
            (remMs) => {
                const sec = (remMs / 1000).toFixed(1);
                bannerSubEl.textContent = `${sec}s 后自动切换下一题…`;
            },
            () => {
                startNextRound();
            }
        );
    }

    function processSubmission(player, rawText) {
        if (gameState.phase !== 'PLAYING') return;

        const targetRanks = gameState.cards.map((c) => c.rank);
        const check = validateCandidate(rawText, targetRanks);
        if (check.ignore) return;

        const userKey = player.uid && player.uid > 0 ? `uid:${player.uid}` : `name:${player.uname}`;
        const now = Date.now();
        const lastTime = gameState.userLastSubmit.get(userKey) || 0;
        if (now - lastTime < USER_COOLDOWN_MS) return;
        gameState.userLastSubmit.set(userKey, now);

        if (check.valid) {
            handleRoundSolved(player, check.prettyExpr);
        } else {
            setToast(`❌ ${player.uname}：${check.reason}`, 'warn');
        }
    }

    // =========================================================================
    // 8. 接入 Dambo SDK（Channel A: 'feed' 实时弹幕广播流）
    // =========================================================================
    const client = Dambo.connect({
        channel: 'feed',
        url: Dambo.utils.getQueryParam('ws') || undefined,
        replaySnapshot: false,
        showLike: Dambo.utils.getQueryBool('show_like', false),
    });

    function updateConnectionBadge(state) {
        connBadgeEl.className = `conn-badge ${state}`;
        if (state === 'open') {
            connBadgeEl.textContent = '弹幕已连接';
        } else if (state === 'reconnecting') {
            connBadgeEl.textContent = '重连中';
        } else if (state === 'closed') {
            connBadgeEl.textContent = '已断开';
        } else {
            connBadgeEl.textContent = '连接中';
        }
    }

    client.on('state', updateConnectionBadge);

    client.on('room_state', (roomState) => {
        if (roomState && roomState.connected) {
            updateConnectionBadge('open');
        }
    });

    client.on('chat', (item, meta) => {
        // 严格忽略历史快照回放，仅响应观众实时弹幕
        if (meta && meta.isSnapshot) return;
        if (!item || typeof item.content !== 'string') return;

        processSubmission(
            {
                uid: item.uid || 0,
                uname: item.uname || '观众',
                face: item.face || '',
            },
            item.content
        );
    });

    // 启动首局游戏
    startNextRound();
})();
