(function (root, factory) {
    if (typeof module === 'object' && module.exports) {
        module.exports = factory();
    } else {
        root.UsStockEngine = factory();
    }
})(typeof globalThis !== 'undefined' ? globalThis : window, function () {
    'use strict';

    const TICKER_REGEX = /^[A-Za-z]{1,5}(\.[A-Za-z])?$/;
    const MAX_KV_KEY_BYTES = 128;

    const DEFAULT_CONFIG = {
        finnhub_api_key: '',
        market_data_mode: 'auto',
        enable_finnhub_ws: true,
        poll_interval_sec: 10,
        market_session_override: 'auto',
        allow_trading_when_closed: true,
        marquee_major_tickers: 'SPY,QQQ,NVDA,TSLA,AAPL,MSFT,META,AMZN',
        watchlist_tickers: 'NVDA,TSLA,AAPL,MSFT,SPY,QQQ,META,AMZN',
        require_fan_medal: true,
        room_medal_name: '',
        min_medal_level: 1,
        guard_bypass_medal: false,
        focus_auto_rotate_sec: 10,
        focus_lock_lv0_sec: 8,
        focus_lock_lv1_sec: 12,
        focus_lock_lv2_sec: 20,
        focus_lock_lv3_sec: 25,
        pos_card_duration_sec: 6,
        initial_balance: 100000,
        min_trade_usd: 100,
        allow_fractional_shares: true,
        short_leverage: 2,
        liquidation_maint_ratio: 0.2,
        commission_bps: 0,
        trade_cooldown_ms: 1500,
        whale_threshold_usd: 25000,
        banner_duration_sec: 5,
        hud_title: 'US.TERM // 美股弹幕实盘',
        bg_mode: 'solid',
        bg_opacity: 0.95,
        color_up: '#00E676',
        color_down: '#FF3B30',
        color_gold: '#F59E0B',
        leaderboard_top_n: 6,
        leaderboard_sort: 'equity',
        holding_rotate_sec: 3.5,
        marquee_speed_sec: 38,
        reserve_overlay_corner: true,
    };

    const STOCK_DIRECTORY = {
        SPY: { name: 'SPDR S&P 500 ETF', price: 598.42, w52Low: 468.20, w52High: 609.15, avgVol: 58400000 },
        QQQ: { name: 'Invesco QQQ Trust', price: 514.80, w52Low: 405.10, w52High: 531.60, avgVol: 36200000 },
        DIA: { name: 'SPDR Dow Jones ETF', price: 441.25, w52Low: 365.40, w52High: 451.20, avgVol: 4100000 },
        IWM: { name: 'iShares Russell 2000', price: 234.60, w52Low: 189.20, w52High: 244.90, avgVol: 27900000 },
        NVDA: { name: 'NVIDIA Corp.', price: 141.85, w52Low: 66.25, w52High: 152.89, avgVol: 245000000 },
        TSLA: { name: 'Tesla Inc.', price: 346.20, w52Low: 138.80, w52High: 384.29, avgVol: 92400000 },
        AAPL: { name: 'Apple Inc.', price: 234.90, w52Low: 164.08, w52High: 244.63, avgVol: 48600000 },
        MSFT: { name: 'Microsoft Corp.', price: 424.60, w52Low: 366.50, w52High: 468.35, avgVol: 21300000 },
        META: { name: 'Meta Platforms', price: 589.10, w52Low: 390.40, w52High: 638.40, avgVol: 14800000 },
        AMZN: { name: 'Amazon.com Inc.', price: 211.35, w52Low: 151.61, w52High: 233.00, avgVol: 39700000 },
        GOOGL: { name: 'Alphabet Inc.', price: 176.40, w52Low: 130.67, w52High: 201.42, avgVol: 26500000 },
        AMD: { name: 'Advanced Micro Devices', price: 144.30, w52Low: 118.45, w52High: 227.30, avgVol: 44800000 },
        AVGO: { name: 'Broadcom Inc.', price: 179.55, w52Low: 109.10, w52High: 251.88, avgVol: 28400000 },
        NFLX: { name: 'Netflix Inc.', price: 892.40, w52Low: 472.20, w52High: 941.75, avgVol: 3400000 },
        COIN: { name: 'Coinbase Global', price: 298.75, w52Low: 114.51, w52High: 349.75, avgVol: 11200000 },
        PLTR: { name: 'Palantir Technologies', price: 67.80, w52Low: 15.66, w52High: 84.80, avgVol: 68500000 },
        MSTR: { name: 'MicroStrategy Inc.', price: 386.50, w52Low: 46.80, w52High: 543.00, avgVol: 23900000 },
        ARM: { name: 'Arm Holdings plc', price: 142.10, w52Low: 66.00, w52High: 188.75, avgVol: 7400000 },
        SMCI: { name: 'Super Micro Computer', price: 36.40, w52Low: 17.25, w52High: 122.90, avgVol: 55600000 },
        INTC: { name: 'Intel Corp.', price: 24.35, w52Low: 18.51, w52High: 51.28, avgVol: 64100000 },
        MU: { name: 'Micron Technology', price: 104.20, w52Low: 79.15, w52High: 157.54, avgVol: 19800000 },
        TSM: { name: 'TSMC ADR', price: 191.60, w52Low: 98.50, w52High: 212.60, avgVol: 15300000 },
        BABA: { name: 'Alibaba Group ADR', price: 91.45, w52Low: 66.63, w52High: 117.82, avgVol: 18900000 },
        PDD: { name: 'PDD Holdings ADR', price: 109.80, w52Low: 88.01, w52High: 164.69, avgVol: 9700000 },
        NIO: { name: 'NIO Inc. ADR', price: 4.86, w52Low: 3.61, w52High: 9.57, avgVol: 45200000 },
        JPM: { name: 'JPMorgan Chase', price: 244.70, w52Low: 161.20, w52High: 254.31, avgVol: 8900000 },
        GS: { name: 'Goldman Sachs', price: 591.20, w52Low: 365.40, w52High: 612.73, avgVol: 2300000 },
        V: { name: 'Visa Inc.', price: 312.80, w52Low: 252.70, w52High: 321.65, avgVol: 6100000 },
        MA: { name: 'Mastercard Inc.', price: 528.40, w52Low: 416.80, w52High: 540.10, avgVol: 2700000 },
        'BRK.B': { name: 'Berkshire Hathaway B', price: 472.10, w52Low: 355.00, w52High: 484.82, avgVol: 3900000 },
        UNH: { name: 'UnitedHealth Group', price: 592.30, w52Low: 436.38, w52High: 630.73, avgVol: 3500000 },
        LLY: { name: 'Eli Lilly & Co.', price: 794.50, w52Low: 576.10, w52High: 972.53, avgVol: 3200000 },
        XOM: { name: 'Exxon Mobil Corp.', price: 118.40, w52Low: 95.77, w52High: 126.34, avgVol: 14600000 },
        GME: { name: 'GameStop Corp.', price: 28.95, w52Low: 9.95, w52High: 64.83, avgVol: 18500000 },
        AMC: { name: 'AMC Entertainment', price: 4.52, w52Low: 2.38, w52High: 11.88, avgVol: 22100000 },
        RIVN: { name: 'Rivian Automotive', price: 12.65, w52Low: 8.26, w52High: 24.62, avgVol: 34500000 },
        HOOD: { name: 'Robinhood Markets', price: 38.40, w52Low: 10.22, w52High: 43.95, avgVol: 26300000 },
        SOFI: { name: 'SoFi Technologies', price: 15.85, w52Low: 6.01, w52High: 17.19, avgVol: 48100000 },
        UBER: { name: 'Uber Technologies', price: 71.90, w52Low: 54.84, w52High: 87.00, avgVol: 21400000 },
        COST: { name: 'Costco Wholesale', price: 964.20, w52Low: 645.30, w52High: 995.00, avgVol: 1900000 },
        WMT: { name: 'Walmart Inc.', price: 91.60, w52Low: 52.10, w52High: 96.18, avgVol: 14900000 },
        DIS: { name: 'Walt Disney Co.', price: 114.20, w52Low: 83.91, w52High: 123.74, avgVol: 9800000 },
        BA: { name: 'Boeing Co.', price: 156.80, w52Low: 137.03, w52High: 267.54, avgVol: 10300000 },
        PYPL: { name: 'PayPal Holdings', price: 86.75, w52Low: 55.77, w52High: 93.66, avgVol: 11400000 },
        CRM: { name: 'Salesforce Inc.', price: 341.20, w52Low: 212.00, w52High: 369.00, avgVol: 5400000 },
        ORCL: { name: 'Oracle Corp.', price: 184.50, w52Low: 101.20, w52High: 198.31, avgVol: 9600000 },
        TLT: { name: 'iShares 20+ Yr Treasury', price: 92.30, w52Low: 85.60, w52High: 101.64, avgVol: 33200000 },
        GLD: { name: 'SPDR Gold Shares', price: 245.10, w52Low: 184.30, w52High: 257.71, avgVol: 7200000 },
    };

    const POPULAR_FALLBACK_POOL = [
        'NVDA', 'TSLA', 'AAPL', 'MSFT', 'SPY', 'QQQ', 'META', 'AMZN',
        'GOOGL', 'AMD', 'COIN', 'PLTR', 'MSTR', 'AVGO', 'NFLX', 'BABA',
    ];

    // -------------------------------------------------------------------------
    // 1. Numeric, String & Key Helpers
    // -------------------------------------------------------------------------
    function clampNum(val, min, max, fallback) {
        const n = Number(val);
        if (!Number.isFinite(n)) return fallback;
        return Math.min(max, Math.max(min, n));
    }

    function roundMoney(v) {
        if (!Number.isFinite(v)) return 0;
        return Math.round(v * 100) / 100;
    }

    function roundPrice(v) {
        if (!Number.isFinite(v)) return 0;
        return Math.round(v * 10000) / 10000;
    }

    function roundShares(v) {
        if (!Number.isFinite(v)) return 0;
        return Math.round(v * 10000) / 10000;
    }

    function fnv1aHex(str) {
        let h = 2166136261;
        for (let i = 0; i < str.length; i++) {
            h ^= str.charCodeAt(i);
            h = Math.imul(h, 16777619);
        }
        return (h >>> 0).toString(16).padStart(8, '0');
    }

    function utf8ByteLength(str) {
        if (typeof TextEncoder === 'function') {
            return new TextEncoder().encode(str).length;
        }
        return unescape(encodeURIComponent(str)).length;
    }

    function sanitizeKvKey(raw, prefixFallback) {
        let s = String(raw || '').replace(/[\u0000-\u001f\u007f/\\]/g, '_').trim();
        if (!s || s === '.' || s === '..') {
            s = (prefixFallback || 'acct') + '_' + fnv1aHex(String(raw || 'anon'));
        }
        if (utf8ByteLength(s) <= MAX_KV_KEY_BYTES) return s;
        const suffix = '_' + fnv1aHex(s);
        let cut = s.slice(0, 64);
        while (cut.length > 1 && utf8ByteLength(cut + suffix) > MAX_KV_KEY_BYTES) {
            cut = cut.slice(0, -1);
        }
        return cut + suffix;
    }

    function normalizeTicker(raw) {
        if (raw === null || raw === undefined) return null;
        const t = String(raw).trim().replace(/^\$/, '').toUpperCase();
        return TICKER_REGEX.test(t) ? t : null;
    }

    function parseTickerList(str, fallbackCsv, limit) {
        const raw = String(str || fallbackCsv || '');
        const seen = new Set();
        const out = [];
        const parts = raw.split(/[\s,，;|]+/);
        for (let i = 0; i < parts.length; i++) {
            const sym = normalizeTicker(parts[i]);
            if (sym && !seen.has(sym)) {
                seen.add(sym);
                out.push(sym);
                if (limit && out.length >= limit) break;
            }
        }
        return out;
    }

    function formatUsd(val, compact) {
        const n = Number(val || 0);
        const abs = Math.abs(n);
        const sign = n < 0 ? '-' : '';
        if (compact) {
            if (abs >= 1e9) return sign + '$' + (abs / 1e9).toFixed(2) + 'B';
            if (abs >= 1e6) return sign + '$' + (abs / 1e6).toFixed(2) + 'M';
            if (abs >= 1e4) return sign + '$' + (abs / 1e3).toFixed(1) + 'K';
        }
        return sign + '$' + abs.toLocaleString('en-US', {
            minimumFractionDigits: abs >= 1000 ? 0 : 2,
            maximumFractionDigits: 2,
        });
    }

    function formatSignedUsd(val, compact) {
        const n = Number(val || 0);
        const prefix = n > 0 ? '+' : '';
        return prefix + formatUsd(n, compact);
    }

    function formatPrice(val) {
        const n = Number(val || 0);
        if (!Number.isFinite(n) || n <= 0) return '$--.--';
        if (n >= 1000) return '$' + n.toFixed(2);
        if (n < 1) return '$' + n.toFixed(4);
        return '$' + n.toFixed(2);
    }

    function formatPct(pct) {
        const n = Number(pct || 0);
        const sign = n > 0 ? '+' : '';
        return sign + n.toFixed(2) + '%';
    }

    function formatShares(shares) {
        const n = Number(shares || 0);
        if (!Number.isFinite(n)) return '0';
        if (Math.abs(n - Math.round(n)) < 1e-4) {
            return String(Math.round(n));
        }
        return parseFloat(n.toFixed(4)).toString();
    }

    function formatVolume(vol) {
        const n = Number(vol || 0);
        if (!Number.isFinite(n) || n <= 0) return '--';
        if (n >= 1e9) return (n / 1e9).toFixed(2) + 'B';
        if (n >= 1e6) return (n / 1e6).toFixed(2) + 'M';
        if (n >= 1e3) return (n / 1e3).toFixed(1) + 'K';
        return String(Math.round(n));
    }

    function accountKeyForPlayer(player) {
        const uidNum = Number(player && player.uid);
        if (Number.isFinite(uidNum) && uidNum > 0) {
            return 'uid:' + Math.floor(uidNum);
        }
        const uname = String((player && (player.uname || player.name)) || '观众').trim() || '观众';
        return sanitizeKvKey('name:' + uname, 'name');
    }

    // -------------------------------------------------------------------------
    // 2. Guard Priority Mapping & Fan Medal Hard Gate
    // -------------------------------------------------------------------------
    /**
     * Map Bilibili guard_level (1=总督, 2=提督, 3=舰长) to priority weight:
     * 总督 Level 3 > 提督 Level 2 > 舰长 Level 1 > 普通灯牌 Level 0
     */
    function mapBiliGuardToPriority(biliGuardLevel) {
        const lv = Number(biliGuardLevel);
        if (lv === 1) return 3;
        if (lv === 2) return 2;
        if (lv === 3) return 1;
        return 0;
    }

    function priorityToGuardMeta(priority) {
        const p = Number(priority || 0);
        if (p >= 3) {
            return {
                priority: 3,
                biliLevel: 1,
                label: '总督',
                shortLabel: '总督 Lv3',
                icon: '👑',
                cssClass: 'guard-lv3',
            };
        }
        if (p === 2) {
            return {
                priority: 2,
                biliLevel: 2,
                label: '提督',
                shortLabel: '提督 Lv2',
                icon: '⚔️',
                cssClass: 'guard-lv2',
            };
        }
        if (p === 1) {
            return {
                priority: 1,
                biliLevel: 3,
                label: '舰长',
                shortLabel: '舰长 Lv1',
                icon: '⚓',
                cssClass: 'guard-lv1',
            };
        }
        return {
            priority: 0,
            biliLevel: 0,
            label: '',
            shortLabel: '灯牌',
            icon: '',
            cssClass: 'guard-lv0',
        };
    }

    function resolvePlayerPriority(item, guardCacheMap, manualOverrideMap) {
        const acctKey = accountKeyForPlayer(item);
        if (manualOverrideMap && manualOverrideMap.has(acctKey)) {
            const ov = Number(manualOverrideMap.get(acctKey));
            if (Number.isFinite(ov) && ov >= 0 && ov <= 3) return Math.floor(ov);
        }
        if (item && Number.isFinite(Number(item.guard_priority)) && Number(item.guard_priority) >= 0) {
            return clampNum(item.guard_priority, 0, 3, 0);
        }
        const directBili = mapBiliGuardToPriority(item && item.guard_level);
        if (directBili > 0) return directBili;
        if (guardCacheMap && guardCacheMap.has(acctKey)) {
            const cached = guardCacheMap.get(acctKey);
            if (cached && Number.isFinite(Number(cached.priority))) {
                return clampNum(cached.priority, 0, 3, 0);
            }
        }
        return 0;
    }

    function checkFanMedalGate(item, cfg, currentRoomId, resolvedPriority) {
        const c = cfg || DEFAULT_CONFIG;
        if (!c.require_fan_medal) {
            return { allowed: true, reason: '' };
        }
        if (c.guard_bypass_medal && Number(resolvedPriority || 0) > 0) {
            return { allowed: true, reason: '' };
        }
        const medalName = item && typeof item.medal_name === 'string' ? item.medal_name.trim() : '';
        if (!medalName) {
            return { allowed: false, reason: 'NO_MEDAL' };
        }
        const minLv = clampNum(c.min_medal_level, 1, 40, 1);
        const rawLv = item && item.medal_level !== undefined && item.medal_level !== null
            ? Number(item.medal_level)
            : 1;
        const medalLv = Number.isFinite(rawLv) ? rawLv : 0;
        if (medalLv < minLv) {
            return { allowed: false, reason: 'MEDAL_LEVEL_LOW' };
        }
        const expectedMedal = typeof c.room_medal_name === 'string' ? c.room_medal_name.trim() : '';
        if (expectedMedal && medalName.toLowerCase() !== expectedMedal.toLowerCase()) {
            return { allowed: false, reason: 'MEDAL_ROOM_MISMATCH' };
        }
        if (
            item &&
            item.medal_room_id !== undefined &&
            item.medal_room_id !== null &&
            Number(item.medal_room_id) > 0 &&
            Number(currentRoomId) > 0 &&
            Number(item.medal_room_id) !== Number(currentRoomId)
        ) {
            return { allowed: false, reason: 'MEDAL_ROOM_ID_MISMATCH' };
        }
        return { allowed: true, reason: '' };
    }

    // -------------------------------------------------------------------------
    // 3. US Market Session & ET Clock (with NYSE Holidays)
    // -------------------------------------------------------------------------
    function nthWeekdayOfMonth(year, month1to12, weekday0Sun, n) {
        const firstDay = new Date(Date.UTC(year, month1to12 - 1, 1)).getUTCDay();
        const offset = (weekday0Sun - firstDay + 7) % 7;
        return 1 + offset + (n - 1) * 7;
    }

    function lastWeekdayOfMonth(year, month1to12, weekday0Sun) {
        const daysInMonth = new Date(Date.UTC(year, month1to12, 0)).getUTCDate();
        const lastDay = new Date(Date.UTC(year, month1to12 - 1, daysInMonth)).getUTCDay();
        const back = (lastDay - weekday0Sun + 7) % 7;
        return daysInMonth - back;
    }

    function observedFixedHoliday(year, month, day) {
        const dow = new Date(Date.UTC(year, month - 1, day)).getUTCDay();
        if (dow === 6) {
            const d = new Date(Date.UTC(year, month - 1, day - 1));
            return (d.getUTCMonth() + 1) + '-' + d.getUTCDate();
        }
        if (dow === 0) {
            const d = new Date(Date.UTC(year, month - 1, day + 1));
            return (d.getUTCMonth() + 1) + '-' + d.getUTCDate();
        }
        return month + '-' + day;
    }

    function easterGoodFridayKey(year) {
        // Anonymous Gregorian algorithm for Easter Sunday, minus 2 days for Good Friday
        const a = year % 19;
        const b = Math.floor(year / 100);
        const c = year % 100;
        const d = Math.floor(b / 4);
        const e = b % 4;
        const f = Math.floor((b + 8) / 25);
        const g = Math.floor((b - f + 1) / 3);
        const h = (19 * a + b - d - g + 15) % 30;
        const i = Math.floor(c / 4);
        const k = c % 4;
        const l = (32 + 2 * e + 2 * i - h - k) % 7;
        const m = Math.floor((a + 11 * h + 22 * l) / 451);
        const month = Math.floor((h + l - 7 * m + 114) / 31);
        const day = ((h + l - 7 * m + 114) % 31) + 1;
        const gf = new Date(Date.UTC(year, month - 1, day - 2));
        return (gf.getUTCMonth() + 1) + '-' + gf.getUTCDate();
    }

    function isNyseHolidayEt(year, month, day) {
        const key = month + '-' + day;
        const holidays = new Set([
            observedFixedHoliday(year, 1, 1),
            '1-' + nthWeekdayOfMonth(year, 1, 1, 3),   // MLK Day
            '2-' + nthWeekdayOfMonth(year, 2, 1, 3),   // Presidents' Day
            easterGoodFridayKey(year),                 // Good Friday
            '5-' + lastWeekdayOfMonth(year, 5, 1),     // Memorial Day
            observedFixedHoliday(year, 6, 19),         // Juneteenth
            observedFixedHoliday(year, 7, 4),          // Independence Day
            '9-' + nthWeekdayOfMonth(year, 9, 1, 1),   // Labor Day
            '11-' + nthWeekdayOfMonth(year, 11, 4, 4), // Thanksgiving
            observedFixedHoliday(year, 12, 25),        // Christmas
        ]);
        return holidays.has(key);
    }

    const ET_PARTS_FMT = typeof Intl !== 'undefined' && Intl.DateTimeFormat
        ? new Intl.DateTimeFormat('en-US', {
            timeZone: 'America/New_York',
            year: 'numeric',
            month: '2-digit',
            day: '2-digit',
            weekday: 'short',
            hour: '2-digit',
            minute: '2-digit',
            second: '2-digit',
            hour12: false,
            timeZoneName: 'short',
        })
        : null;

    function getEtParts(nowMs) {
        const dt = new Date(nowMs);
        if (!ET_PARTS_FMT) {
            return {
                year: dt.getUTCFullYear(),
                month: dt.getUTCMonth() + 1,
                day: dt.getUTCDate(),
                weekday: ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'][dt.getUTCDay()],
                hour: dt.getUTCHours(),
                minute: dt.getUTCMinutes(),
                second: dt.getUTCSeconds(),
                tzName: 'ET',
            };
        }
        const parts = ET_PARTS_FMT.formatToParts(dt);
        const map = {};
        for (let i = 0; i < parts.length; i++) {
            map[parts[i].type] = parts[i].value;
        }
        let hour = parseInt(map.hour || '0', 10);
        if (hour === 24) hour = 0;
        return {
            year: parseInt(map.year || '2026', 10),
            month: parseInt(map.month || '1', 10),
            day: parseInt(map.day || '1', 10),
            weekday: map.weekday || 'Mon',
            hour: hour,
            minute: parseInt(map.minute || '0', 10),
            second: parseInt(map.second || '0', 10),
            tzName: map.timeZoneName || 'ET',
        };
    }

    function pad2(n) {
        return n < 10 ? ('0' + n) : String(n);
    }

    const SESSION_LABELS = {
        regular: { code: 'regular', cnLabel: '已开市·盘中交易', enLabel: 'Regular Hours', fullLabel: '已开市·盘中交易 Regular Hours' },
        pre: { code: 'pre', cnLabel: '盘前交易', enLabel: 'Pre-Market', fullLabel: '盘前交易 Pre-Market' },
        after: { code: 'after', cnLabel: '盘后交易', enLabel: 'After-Hours', fullLabel: '盘后交易 After-Hours' },
        closed: { code: 'closed', cnLabel: '已闭市', enLabel: 'Market Closed', fullLabel: '已闭市 Market Closed' },
    };

    function getUsMarketSession(nowMs, overrideMode) {
        const ts = typeof nowMs === 'number' && Number.isFinite(nowMs) ? nowMs : Date.now();
        const et = getEtParts(ts);
        const etTimeStr = pad2(et.hour) + ':' + pad2(et.minute) + ':' + pad2(et.second);
        const etDateStr = et.year + '-' + pad2(et.month) + '-' + pad2(et.day);

        let code = 'closed';
        const ov = typeof overrideMode === 'string' ? overrideMode.trim().toLowerCase() : 'auto';
        if (ov === 'regular' || ov === 'pre' || ov === 'after' || ov === 'closed') {
            code = ov;
        } else {
            const isWeekend = et.weekday === 'Sat' || et.weekday === 'Sun';
            const isHoliday = isNyseHolidayEt(et.year, et.month, et.day);
            if (!isWeekend && !isHoliday) {
                const mins = et.hour * 60 + et.minute;
                if (mins >= 240 && mins < 570) {
                    code = 'pre';
                } else if (mins >= 570 && mins < 960) {
                    code = 'regular';
                } else if (mins >= 960 && mins < 1200) {
                    code = 'after';
                } else {
                    code = 'closed';
                }
            }
        }

        const meta = SESSION_LABELS[code] || SESSION_LABELS.closed;
        return {
            code: meta.code,
            cnLabel: meta.cnLabel,
            enLabel: meta.enLabel,
            fullLabel: meta.fullLabel,
            etTimeStr: etTimeStr,
            etDateStr: etDateStr,
            tzName: et.tzName || 'ET',
        };
    }

    // -------------------------------------------------------------------------
    // 4. Danmaku Command Parser
    // -------------------------------------------------------------------------
    function parseAmountToken(rawToken) {
        if (!rawToken) return null;
        const t = String(rawToken).trim().toLowerCase().replace(/,/g, '').replace(/^\$/, '');
        if (!t) return null;
        if (/^(all|allin|all-in|max|梭哈|全仓|全部)$/.test(t)) {
            return { isAll: true, value: 0 };
        }
        const m = t.match(/^(\d+(?:\.\d+)?)\s*(k|千|w|万)?$/i);
        if (!m) return null;
        let num = parseFloat(m[1]);
        if (!Number.isFinite(num) || num <= 0) return null;
        const unit = (m[2] || '').toLowerCase();
        if (unit === 'k' || unit === '千') num *= 1000;
        if (unit === 'w' || unit === '万') num *= 10000;
        return { isAll: false, value: num };
    }

    function parseDanmakuCommand(rawText) {
        if (typeof rawText !== 'string') return null;
        const cleaned = rawText
            .replace(/[\s\u3000]+/g, ' ')
            .replace(/^[/!！]+/, '')
            .trim();
        if (!cleaned || cleaned.length > 80) return null;

        // 1. 持仓(pos)
        if (/^(?:持仓|我的持仓|查询持仓|账户|查账|开户|pos|position|positions|balance|account|open)$/i.test(cleaned)) {
            return { kind: 'pos' };
        }

        const parts = cleaned.split(' ');
        const verb = parts[0].toLowerCase();

        // 2. 查股(quote) 代码
        if (/^(?:查股|查|行情|报价|quote|q)$/i.test(verb) && parts.length === 2) {
            const ticker = normalizeTicker(parts[1]);
            if (!ticker) return null;
            return { kind: 'quote', ticker: ticker };
        }

        // 3. 买入(buy) 代码 金额/all  &  全仓(allin) 代码
        if (/^(?:全仓|梭哈|allin|all-in)$/i.test(verb) && parts.length === 2) {
            const ticker = normalizeTicker(parts[1]);
            if (!ticker) return null;
            return { kind: 'buy', ticker: ticker, amount: 0, isAll: true };
        }
        if (/^(?:买入|买|做多|buy|b|long)$/i.test(verb) && parts.length >= 2 && parts.length <= 3) {
            const ticker = normalizeTicker(parts[1]);
            if (!ticker) return null;
            if (parts.length === 2) return null;
            const parsedAmt = parseAmountToken(parts[2]);
            if (!parsedAmt) return null;
            return {
                kind: 'buy',
                ticker: ticker,
                amount: parsedAmt.value,
                isAll: parsedAmt.isAll,
            };
        }

        // 4. 卖出(sell) 代码 数量/all  &  清仓(close) 代码
        if (/^(?:清仓|close|sellall)$/i.test(verb) && parts.length === 2) {
            const ticker = normalizeTicker(parts[1]);
            if (!ticker) return null;
            return { kind: 'sell', ticker: ticker, shares: 0, isAll: true };
        }
        if (/^(?:卖出|卖|sell|s)$/i.test(verb) && parts.length >= 2 && parts.length <= 3) {
            const ticker = normalizeTicker(parts[1]);
            if (!ticker) return null;
            if (parts.length === 2) return null;
            const parsedQty = parseAmountToken(parts[2]);
            if (!parsedQty) return null;
            return {
                kind: 'sell',
                ticker: ticker,
                shares: parsedQty.value,
                isAll: parsedQty.isAll,
            };
        }

        // 5. 做空(short) 代码 金额/all
        if (/^(?:做空|空|short)$/i.test(verb) && parts.length === 3) {
            const ticker = normalizeTicker(parts[1]);
            if (!ticker) return null;
            const parsedAmt = parseAmountToken(parts[2]);
            if (!parsedAmt) return null;
            return {
                kind: 'short',
                ticker: ticker,
                amount: parsedAmt.value,
                isAll: parsedAmt.isAll,
            };
        }

        // 6. 平空(cover) 代码 [数量/all]
        if (/^(?:平空|cover)$/i.test(verb) && parts.length >= 2 && parts.length <= 3) {
            const ticker = normalizeTicker(parts[1]);
            if (!ticker) return null;
            if (parts.length === 2) {
                return { kind: 'cover', ticker: ticker, shares: 0, isAll: true };
            }
            const parsedQty = parseAmountToken(parts[2]);
            if (!parsedQty) return null;
            return {
                kind: 'cover',
                ticker: ticker,
                shares: parsedQty.value,
                isAll: parsedQty.isAll,
            };
        }

        return null;
    }

    // -------------------------------------------------------------------------
    // 5. Focus & Super Quote Preemption Scheduler
    // -------------------------------------------------------------------------
    function getLockDurationMsForPriority(priority, cfg) {
        const c = cfg || DEFAULT_CONFIG;
        const p = Number(priority || 0);
        if (p >= 3) return clampNum(c.focus_lock_lv3_sec, 5, 120, 25) * 1000;
        if (p === 2) return clampNum(c.focus_lock_lv2_sec, 5, 120, 20) * 1000;
        if (p === 1) return clampNum(c.focus_lock_lv1_sec, 4, 60, 12) * 1000;
        return clampNum(c.focus_lock_lv0_sec, 3, 60, 8) * 1000;
    }

    function createFocusScheduler(initialSymbol) {
        let currentFocus = normalizeTicker(initialSymbol) || 'NVDA';
        let activeLock = null;
        let queue = [];
        let lastRotateAt = 0;
        let rotateCursor = 0;
        let seqCounter = 1;

        function sortQueue() {
            queue.sort(function (a, b) {
                if (b.priority !== a.priority) return b.priority - a.priority;
                return a.seq - b.seq;
            });
        }

        function activateEntry(req, nowMs, cfg) {
            const durationMs = req.durationMs > 0
                ? req.durationMs
                : getLockDurationMsForPriority(req.priority, cfg);
            currentFocus = req.symbol;
            activeLock = {
                symbol: req.symbol,
                kind: req.kind || 'quote',
                showSuperCard: Boolean(req.showSuperCard),
                priority: Number(req.priority || 0),
                ownerUid: Number(req.ownerUid || 0),
                ownerName: req.ownerName || '观众',
                ownerFace: req.ownerFace || '',
                guardMeta: priorityToGuardMeta(req.priority),
                startedAt: nowMs,
                expiresAt: nowMs + durationMs,
                durationMs: durationMs,
                preempted: Boolean(req.preempted),
            };
            lastRotateAt = nowMs;
            return activeLock;
        }

        return {
            getState: function (nowMs) {
                const ts = typeof nowMs === 'number' ? nowMs : Date.now();
                const remainingMs = activeLock ? Math.max(0, activeLock.expiresAt - ts) : 0;
                return {
                    currentFocus: currentFocus,
                    activeLock: activeLock,
                    remainingMs: remainingMs,
                    queue: queue.slice(),
                    queueLength: queue.length,
                    lastRotateAt: lastRotateAt,
                };
            },

            setManualFocus: function (symbol, nowMs, cfg) {
                const sym = normalizeTicker(symbol);
                if (!sym) return null;
                const ts = typeof nowMs === 'number' ? nowMs : Date.now();
                queue = queue.filter(function (q) { return q.symbol !== sym; });
                return activateEntry({
                    symbol: sym,
                    kind: 'manual',
                    showSuperCard: false,
                    priority: 3,
                    ownerUid: 0,
                    ownerName: '主播控制台',
                    ownerFace: '',
                    durationMs: getLockDurationMsForPriority(2, cfg),
                    preempted: Boolean(activeLock),
                }, ts, cfg);
            },

            requestFocus: function (req, nowMs, cfg) {
                const sym = normalizeTicker(req && req.symbol);
                if (!sym) return { activated: false, queued: false };
                const ts = typeof nowMs === 'number' ? nowMs : Date.now();
                const priority = clampNum(req.priority, 0, 3, 0);

                // Clean expired lock first
                if (activeLock && ts >= activeLock.expiresAt) {
                    activeLock = null;
                }

                // Immediate activation or high-guard preemption
                if (!activeLock || priority > activeLock.priority) {
                    const prev = activeLock;
                    const activated = activateEntry({
                        symbol: sym,
                        kind: req.kind || 'quote',
                        showSuperCard: Boolean(req.showSuperCard),
                        priority: priority,
                        ownerUid: req.ownerUid || 0,
                        ownerName: req.ownerName || '观众',
                        ownerFace: req.ownerFace || '',
                        preempted: Boolean(prev),
                    }, ts, cfg);
                    return {
                        activated: true,
                        preempted: Boolean(prev),
                        preemptedOwner: prev ? prev.ownerName : '',
                        item: activated,
                    };
                }

                // If same user/symbol extends same non-lower priority active card without flicker
                if (
                    activeLock.symbol === sym &&
                    activeLock.ownerName === (req.ownerName || '观众') &&
                    priority === activeLock.priority
                ) {
                    const extDur = getLockDurationMsForPriority(priority, cfg);
                    activeLock.expiresAt = ts + extDur;
                    activeLock.durationMs = extDur;
                    if (req.showSuperCard) activeLock.showSuperCard = true;
                    return {
                        activated: true,
                        extended: true,
                        preempted: false,
                        item: activeLock,
                    };
                }

                // Enqueue for same or lower priority while lock is held
                const existingIdx = queue.findIndex(function (q) {
                    return q.symbol === sym && q.showSuperCard === Boolean(req.showSuperCard);
                });
                const entry = {
                    seq: seqCounter++,
                    symbol: sym,
                    kind: req.kind || 'quote',
                    showSuperCard: Boolean(req.showSuperCard),
                    priority: priority,
                    ownerUid: req.ownerUid || 0,
                    ownerName: req.ownerName || '观众',
                    ownerFace: req.ownerFace || '',
                    guardMeta: priorityToGuardMeta(priority),
                    enqueuedAt: ts,
                };
                if (existingIdx !== -1) {
                    if (priority >= queue[existingIdx].priority) {
                        queue[existingIdx] = entry;
                    }
                } else {
                    queue.push(entry);
                }
                sortQueue();
                if (queue.length > 24) {
                    queue.length = 24;
                }
                const pos = queue.findIndex(function (q) { return q.seq === entry.seq; }) + 1;
                return {
                    activated: false,
                    queued: true,
                    queuePos: pos > 0 ? pos : queue.length,
                    item: entry,
                };
            },

            tick: function (nowMs, watchlist, cfg) {
                const ts = typeof nowMs === 'number' ? nowMs : Date.now();
                const list = Array.isArray(watchlist) && watchlist.length > 0 ? watchlist : ['NVDA'];

                if (activeLock && ts >= activeLock.expiresAt) {
                    activeLock = null;
                    if (queue.length > 0) {
                        const next = queue.shift();
                        const activated = activateEntry(next, ts, cfg);
                        return { changed: true, reason: 'dequeue', activeLock: activated };
                    }
                    lastRotateAt = ts;
                    return { changed: true, reason: 'unlock', activeLock: null };
                }

                if (!activeLock && queue.length > 0) {
                    const next = queue.shift();
                    const activated = activateEntry(next, ts, cfg);
                    return { changed: true, reason: 'dequeue', activeLock: activated };
                }

                if (!activeLock) {
                    if (lastRotateAt === 0) {
                        lastRotateAt = ts;
                        if (list.indexOf(currentFocus) === -1) {
                            currentFocus = list[0];
                            return { changed: true, reason: 'init', activeLock: null };
                        }
                        return { changed: false, reason: 'idle', activeLock: null };
                    }
                    const rotMs = clampNum(cfg && cfg.focus_auto_rotate_sec, 3, 120, 10) * 1000;
                    if (ts - lastRotateAt >= rotMs) {
                        lastRotateAt = ts;
                        const curIdx = list.indexOf(currentFocus);
                        rotateCursor = curIdx >= 0 ? (curIdx + 1) % list.length : (rotateCursor + 1) % list.length;
                        currentFocus = list[rotateCursor];
                        return { changed: true, reason: 'rotate', activeLock: null };
                    }
                }

                return { changed: false, reason: 'steady', activeLock: activeLock };
            },

            reset: function (symbol) {
                currentFocus = normalizeTicker(symbol) || 'NVDA';
                activeLock = null;
                queue = [];
                lastRotateAt = Date.now();
            },
        };
    }

    // -------------------------------------------------------------------------
    // 6. Personal Position Card Priority Queue (6s dwell)
    // -------------------------------------------------------------------------
    function createPosCardQueue() {
        let activeCard = null;
        let queue = [];
        let seqCounter = 1;

        function sortQueue() {
            queue.sort(function (a, b) {
                if (b.priority !== a.priority) return b.priority - a.priority;
                return a.seq - b.seq;
            });
        }

        function activate(req, nowMs, cfg) {
            const dwellMs = clampNum(cfg && cfg.pos_card_duration_sec, 3, 20, 6) * 1000;
            activeCard = {
                acctKey: req.acctKey,
                uid: req.uid,
                name: req.name,
                face: req.face || '',
                priority: Number(req.priority || 0),
                guardMeta: priorityToGuardMeta(req.priority),
                rank: req.rank,
                metrics: req.metrics,
                startedAt: nowMs,
                expiresAt: nowMs + dwellMs,
                durationMs: dwellMs,
                preempted: Boolean(req.preempted),
            };
            return activeCard;
        }

        return {
            getState: function (nowMs) {
                const ts = typeof nowMs === 'number' ? nowMs : Date.now();
                return {
                    activeCard: activeCard,
                    remainingMs: activeCard ? Math.max(0, activeCard.expiresAt - ts) : 0,
                    queueLength: queue.length,
                };
            },

            push: function (req, nowMs, cfg) {
                const ts = typeof nowMs === 'number' ? nowMs : Date.now();
                const priority = clampNum(req.priority, 0, 3, 0);
                if (activeCard && ts >= activeCard.expiresAt) {
                    activeCard = null;
                }

                if (!activeCard || priority > activeCard.priority || activeCard.acctKey === req.acctKey) {
                    const prev = activeCard && activeCard.acctKey !== req.acctKey ? activeCard : null;
                    const card = activate({
                        acctKey: req.acctKey,
                        uid: req.uid,
                        name: req.name,
                        face: req.face,
                        priority: priority,
                        rank: req.rank,
                        metrics: req.metrics,
                        preempted: Boolean(prev),
                    }, ts, cfg);
                    return { shown: true, preempted: Boolean(prev), card: card };
                }

                const idx = queue.findIndex(function (q) { return q.acctKey === req.acctKey; });
                const entry = {
                    seq: seqCounter++,
                    acctKey: req.acctKey,
                    uid: req.uid,
                    name: req.name,
                    face: req.face || '',
                    priority: priority,
                    rank: req.rank,
                    metrics: req.metrics,
                };
                if (idx !== -1) {
                    queue[idx] = entry;
                } else {
                    queue.push(entry);
                }
                sortQueue();
                if (queue.length > 16) queue.length = 16;
                return { shown: false, queued: true, queueLength: queue.length };
            },

            tick: function (nowMs, cfg) {
                const ts = typeof nowMs === 'number' ? nowMs : Date.now();
                if (activeCard && ts >= activeCard.expiresAt) {
                    activeCard = null;
                    if (queue.length > 0) {
                        const next = queue.shift();
                        return { changed: true, activeCard: activate(next, ts, cfg) };
                    }
                    return { changed: true, activeCard: null };
                }
                if (!activeCard && queue.length > 0) {
                    const next = queue.shift();
                    return { changed: true, activeCard: activate(next, ts, cfg) };
                }
                return { changed: false, activeCard: activeCard };
            },

            clear: function () {
                activeCard = null;
                queue = [];
            },
        };
    }

    // -------------------------------------------------------------------------
    // 7. Account Valuation, Trading & Risk / Liquidation Engine
    // -------------------------------------------------------------------------
    function createInitialAccount(player, priority, cfg, nowMs) {
        const initBal = clampNum(cfg && cfg.initial_balance, 1000, 100000000, 100000);
        const ts = typeof nowMs === 'number' ? nowMs : Date.now();
        return {
            uid: Number((player && player.uid) || 0),
            name: String((player && (player.uname || player.name)) || '观众').trim() || '观众',
            face: (player && player.face) || '',
            medal_name: (player && player.medal_name) || '',
            medal_level: Number((player && player.medal_level) || 0),
            guard_priority: clampNum(priority, 0, 3, 0),
            cash: initBal,
            initial_balance: initBal,
            positions: {},
            realized_pnl: 0,
            trade_count: 0,
            liquidation_count: 0,
            created_at: ts,
            updated_at: ts,
        };
    }

    function getMarkPrice(symbol, quotesMap, fallbackPrice) {
        const q = quotesMap && quotesMap.get ? quotesMap.get(symbol) : null;
        if (q && Number(q.c) > 0) return Number(q.c);
        const dir = STOCK_DIRECTORY[symbol];
        if (fallbackPrice && Number(fallbackPrice) > 0) return Number(fallbackPrice);
        if (dir && dir.price > 0) return dir.price;
        return 0;
    }

    function computeAccountMetrics(acct, quotesMap, cfg) {
        const cash = Number((acct && acct.cash) || 0);
        const positions = (acct && acct.positions && typeof acct.positions === 'object') ? acct.positions : {};
        let longMarketVal = 0;
        let shortEquityVal = 0;
        let longNotional = 0;
        let shortNotional = 0;
        const holdings = [];

        Object.keys(positions).forEach(function (sym) {
            const p = positions[sym];
            if (!p || Number(p.shares) <= 1e-6) return;
            const shares = Number(p.shares);
            const side = p.side === 'short' ? 'short' : 'long';

            if (side === 'long') {
                const avgCost = Number(p.avgCost !== undefined ? p.avgCost : (p.cost || 0));
                const markPrice = getMarkPrice(sym, quotesMap, avgCost);
                const marketVal = shares * markPrice;
                const costVal = shares * avgCost;
                const unrealizedPnl = marketVal - costVal;
                const pnlPct = costVal > 0 ? (unrealizedPnl / costVal) * 100 : 0;
                longMarketVal += marketVal;
                longNotional += marketVal;
                holdings.push({
                    side: 'long',
                    ticker: sym,
                    shares: shares,
                    entryPrice: avgCost,
                    markPrice: markPrice,
                    marketVal: roundMoney(marketVal),
                    notional: roundMoney(marketVal),
                    margin: roundMoney(costVal),
                    leverage: 1,
                    unrealizedPnl: roundMoney(unrealizedPnl),
                    pnlPct: Math.round(pnlPct * 100) / 100,
                });
            } else {
                const entryPrice = Number(p.entryPrice !== undefined ? p.entryPrice : (p.cost || 0));
                const margin = Number(p.margin || 0);
                const leverage = Number(p.leverage || (cfg && cfg.short_leverage) || 2);
                const markPrice = getMarkPrice(sym, quotesMap, entryPrice);
                const unrealizedPnl = (entryPrice - markPrice) * shares;
                const posEquity = margin + unrealizedPnl;
                const notional = shares * markPrice;
                const pnlPct = margin > 0 ? (unrealizedPnl / margin) * 100 : 0;
                shortEquityVal += posEquity;
                shortNotional += notional;
                holdings.push({
                    side: 'short',
                    ticker: sym,
                    shares: shares,
                    entryPrice: entryPrice,
                    markPrice: markPrice,
                    marketVal: roundMoney(posEquity),
                    notional: roundMoney(notional),
                    margin: roundMoney(margin),
                    leverage: leverage,
                    unrealizedPnl: roundMoney(unrealizedPnl),
                    pnlPct: Math.round(pnlPct * 100) / 100,
                });
            }
        });

        holdings.sort(function (a, b) {
            return b.notional - a.notional;
        });

        const totalEquity = roundMoney(cash + longMarketVal + shortEquityVal);
        const initBal = Number((acct && acct.initial_balance) || (cfg && cfg.initial_balance) || 100000);
        const pnl = roundMoney(totalEquity - initBal);
        const pnlPct = initBal > 0 ? (pnl / initBal) * 100 : 0;

        return {
            cash: roundMoney(cash),
            longMarketVal: roundMoney(longMarketVal),
            shortEquityVal: roundMoney(shortEquityVal),
            longNotional: roundMoney(longNotional),
            shortNotional: roundMoney(shortNotional),
            totalEquity: totalEquity,
            initialBalance: initBal,
            pnl: pnl,
            pnlPct: Math.round(pnlPct * 100) / 100,
            holdings: holdings,
        };
    }

    function computeLeaderboard(accountsMap, quotesMap, cfg) {
        const list = [];
        accountsMap.forEach(function (acct, key) {
            if (!acct || typeof acct !== 'object') return;
            const metrics = computeAccountMetrics(acct, quotesMap, cfg);
            const priority = clampNum(acct.guard_priority, 0, 3, 0);
            list.push({
                key: key,
                uid: Number(acct.uid || 0),
                name: acct.name || '观众',
                face: acct.face || '',
                medalName: acct.medal_name || '',
                medalLevel: Number(acct.medal_level || 0),
                guardPriority: priority,
                guardMeta: priorityToGuardMeta(priority),
                cash: metrics.cash,
                totalEquity: metrics.totalEquity,
                initialBalance: metrics.initialBalance,
                pnl: metrics.pnl,
                pnlPct: metrics.pnlPct,
                holdings: metrics.holdings,
                tradeCount: Number(acct.trade_count || 0),
                liquidationCount: Number(acct.liquidation_count || 0),
                updatedAt: Number(acct.updated_at || 0),
            });
        });

        const sortMode = cfg && cfg.leaderboard_sort === 'return_pct' ? 'return_pct' : 'equity';
        list.sort(function (a, b) {
            if (sortMode === 'return_pct') {
                if (b.pnlPct !== a.pnlPct) return b.pnlPct - a.pnlPct;
                if (b.totalEquity !== a.totalEquity) return b.totalEquity - a.totalEquity;
            } else {
                if (b.totalEquity !== a.totalEquity) return b.totalEquity - a.totalEquity;
                if (b.pnlPct !== a.pnlPct) return b.pnlPct - a.pnlPct;
            }
            if (b.guardPriority !== a.guardPriority) return b.guardPriority - a.guardPriority;
            return b.updatedAt - a.updatedAt;
        });

        for (let i = 0; i < list.length; i++) {
            list[i].rank = i + 1;
        }
        return list;
    }

    function computeRoomSymbolStats(symbol, accountsMap, quotesMap) {
        const sym = normalizeTicker(symbol);
        let longNotional = 0;
        let shortNotional = 0;
        let longShares = 0;
        let shortShares = 0;
        let longCostSum = 0;
        let shortEntrySum = 0;
        let longHolders = 0;
        let shortHolders = 0;

        if (sym && accountsMap) {
            accountsMap.forEach(function (acct) {
                const pos = acct && acct.positions && acct.positions[sym];
                if (!pos || Number(pos.shares) <= 1e-6) return;
                const shares = Number(pos.shares);
                const markPrice = getMarkPrice(sym, quotesMap, pos.avgCost || pos.entryPrice || 0);
                if (pos.side === 'short') {
                    const entry = Number(pos.entryPrice || pos.cost || markPrice);
                    shortHolders += 1;
                    shortShares += shares;
                    shortEntrySum += shares * entry;
                    shortNotional += shares * markPrice;
                } else {
                    const avgCost = Number(pos.avgCost || pos.cost || markPrice);
                    longHolders += 1;
                    longShares += shares;
                    longCostSum += shares * avgCost;
                    longNotional += shares * markPrice;
                }
            });
        }

        const totalNotional = longNotional + shortNotional;
        const longPct = totalNotional > 0 ? Math.round((longNotional / totalNotional) * 1000) / 10 : 50;
        const shortPct = totalNotional > 0 ? Math.round((100 - longPct) * 10) / 10 : 50;
        const longAvgCost = longShares > 0 ? roundPrice(longCostSum / longShares) : 0;
        const shortAvgCost = shortShares > 0 ? roundPrice(shortEntrySum / shortShares) : 0;
        const totalShares = longShares + shortShares;
        const overallAvgCost = totalShares > 0
            ? roundPrice((longCostSum + shortEntrySum) / totalShares)
            : 0;

        return {
            symbol: sym || '',
            longNotional: roundMoney(longNotional),
            shortNotional: roundMoney(shortNotional),
            totalNotional: roundMoney(totalNotional),
            longPct: longPct,
            shortPct: shortPct,
            longHolders: longHolders,
            shortHolders: shortHolders,
            longAvgCost: longAvgCost,
            shortAvgCost: shortAvgCost,
            overallAvgCost: overallAvgCost,
        };
    }

    function computeGlobalBattleStats(accountsMap, quotesMap) {
        let totalLongUsd = 0;
        let totalShortUsd = 0;
        let longCount = 0;
        let shortCount = 0;

        accountsMap.forEach(function (acct) {
            const positions = acct && acct.positions;
            if (!positions || typeof positions !== 'object') return;
            Object.keys(positions).forEach(function (sym) {
                const p = positions[sym];
                if (!p || Number(p.shares) <= 1e-6) return;
                const mark = getMarkPrice(sym, quotesMap, p.avgCost || p.entryPrice || 0);
                const notional = Number(p.shares) * mark;
                if (p.side === 'short') {
                    totalShortUsd += notional;
                    shortCount += 1;
                } else {
                    totalLongUsd += notional;
                    longCount += 1;
                }
            });
        });

        const total = totalLongUsd + totalShortUsd;
        const longPct = total > 0 ? Math.round((totalLongUsd / total) * 1000) / 10 : 50;
        const shortPct = total > 0 ? Math.round((100 - longPct) * 10) / 10 : 50;

        return {
            totalLongUsd: roundMoney(totalLongUsd),
            totalShortUsd: roundMoney(totalShortUsd),
            totalUsd: roundMoney(total),
            longPct: longPct,
            shortPct: shortPct,
            longCount: longCount,
            shortCount: shortCount,
        };
    }

    function computeTopHotTickers(accountsMap, quotesMap, watchlist, limit) {
        const cap = clampNum(limit, 1, 20, 12);
        const map = new Map();

        accountsMap.forEach(function (acct) {
            const positions = acct && acct.positions;
            if (!positions || typeof positions !== 'object') return;
            Object.keys(positions).forEach(function (sym) {
                const p = positions[sym];
                if (!p || Number(p.shares) <= 1e-6) return;
                const mark = getMarkPrice(sym, quotesMap, p.avgCost || p.entryPrice || 0);
                const notional = Number(p.shares) * mark;
                const cur = map.get(sym) || { symbol: sym, capital: 0, holders: 0, isHeld: true };
                cur.capital += notional;
                cur.holders += 1;
                map.set(sym, cur);
            });
        });

        const ranked = Array.from(map.values()).sort(function (a, b) {
            if (b.capital !== a.capital) return b.capital - a.capital;
            return b.holders - a.holders;
        });

        const seen = new Set(ranked.map(function (r) { return r.symbol; }));
        const padPool = (Array.isArray(watchlist) ? watchlist : []).concat(POPULAR_FALLBACK_POOL);
        for (let i = 0; i < padPool.length && ranked.length < cap; i++) {
            const sym = normalizeTicker(padPool[i]);
            if (sym && !seen.has(sym)) {
                seen.add(sym);
                ranked.push({ symbol: sym, capital: 0, holders: 0, isHeld: false });
            }
        }
        return ranked.slice(0, cap);
    }

    function executeTrade(acct, cmd, markPrice, cfg, nowMs) {
        const c = cfg || DEFAULT_CONFIG;
        const ts = typeof nowMs === 'number' ? nowMs : Date.now();
        const price = Number(markPrice);
        if (!Number.isFinite(price) || price <= 0) {
            return { ok: false, reason: cmd.ticker + ' 暂无有效行情报价' };
        }

        const feeRate = clampNum(c.commission_bps, 0, 100, 0) / 10000;
        const minTradeUsd = clampNum(c.min_trade_usd, 1, 100000, 100);
        const allowFrac = c.allow_fractional_shares !== false;
        const whaleThreshold = clampNum(c.whale_threshold_usd, 1000, 10000000, 25000);
        const nextAcct = JSON.parse(JSON.stringify(acct));
        if (!nextAcct.positions || typeof nextAcct.positions !== 'object') {
            nextAcct.positions = {};
        }

        const existing = nextAcct.positions[cmd.ticker];

        // -----------------------------------------------------------------
        // BUY (做多买入)
        // -----------------------------------------------------------------
        if (cmd.kind === 'buy') {
            if (existing && existing.side === 'short' && Number(existing.shares) > 1e-6) {
                return {
                    ok: false,
                    reason: '持有 ' + cmd.ticker + ' 空仓，请先 平空(cover) ' + cmd.ticker,
                };
            }
            const availCash = Number(nextAcct.cash || 0);
            const rawBudget = cmd.isAll ? availCash : Number(cmd.amount || 0);
            if (!Number.isFinite(rawBudget) || rawBudget <= 0) {
                return { ok: false, reason: '买入金额必须大于 0' };
            }
            if (rawBudget > availCash + 1e-6) {
                return {
                    ok: false,
                    reason: '可用现金不足（需 ' + formatUsd(rawBudget) + '，可用 ' + formatUsd(availCash) + '）',
                };
            }
            const spendBudget = Math.min(availCash, rawBudget);
            const effectiveBudget = spendBudget / (1 + feeRate);
            let shares = allowFrac
                ? roundShares(effectiveBudget / price)
                : Math.floor((effectiveBudget + 1e-9) / price);
            if (!allowFrac && shares < 1) {
                return {
                    ok: false,
                    reason: '金额 ' + formatUsd(spendBudget) + ' 不足买入 1 股 ' + cmd.ticker + ' (' + formatPrice(price) + ')',
                };
            }
            if (shares <= 0) {
                return { ok: false, reason: '成交股数过小' };
            }
            const notional = roundMoney(shares * price);
            if (!cmd.isAll && notional < minTradeUsd) {
                return {
                    ok: false,
                    reason: '单笔买入最低 ' + formatUsd(minTradeUsd),
                };
            }
            const fee = roundMoney(notional * feeRate);
            let totalDeduction = roundMoney(notional + fee);
            if (totalDeduction > availCash) {
                totalDeduction = availCash;
            }

            const prevShares = existing ? Number(existing.shares || 0) : 0;
            const prevCost = existing ? Number(existing.avgCost || existing.cost || 0) : 0;
            const nextShares = roundShares(prevShares + shares);
            const weightedCost = nextShares > 0
                ? roundPrice(((prevShares * prevCost) + notional) / nextShares)
                : price;

            nextAcct.cash = roundMoney(Math.max(0, availCash - totalDeduction));
            nextAcct.positions[cmd.ticker] = {
                side: 'long',
                ticker: cmd.ticker,
                shares: nextShares,
                avgCost: weightedCost,
                openedAt: (existing && existing.openedAt) || ts,
            };
            nextAcct.trade_count = Number(nextAcct.trade_count || 0) + 1;
            nextAcct.updated_at = ts;

            const isAllIn = cmd.isAll || (availCash > 0 && totalDeduction >= availCash * 0.96);
            const isWhale = notional >= whaleThreshold || (isAllIn && notional >= minTradeUsd);
            const snapshotText = '持仓 多' + formatShares(nextShares) + '股 · 现金 ' + formatUsd(nextAcct.cash);
            const actionLabel = isAllIn ? '全仓买入' : '买入';

            return {
                ok: true,
                account: nextAcct,
                trade: {
                    kind: 'buy',
                    ticker: cmd.ticker,
                    shares: shares,
                    price: price,
                    notional: notional,
                    fee: fee,
                    isAllIn: isAllIn,
                    isWhale: isWhale,
                    remainingShares: nextShares,
                    remainingCash: nextAcct.cash,
                    snapshotText: snapshotText,
                    summary: actionLabel + ' ' + cmd.ticker + ' ' + formatUsd(notional) + ' (' + formatShares(shares) + '股 @ ' + formatPrice(price) + ')',
                },
            };
        }

        // -----------------------------------------------------------------
        // SELL (卖出多仓)
        // -----------------------------------------------------------------
        if (cmd.kind === 'sell') {
            if (!existing || existing.side !== 'long' || Number(existing.shares) <= 1e-6) {
                return { ok: false, reason: '未持有 ' + cmd.ticker + ' 多头仓位' };
            }
            const heldShares = Number(existing.shares);
            let sellShares = cmd.isAll ? heldShares : Number(cmd.shares || 0);
            if (!allowFrac) sellShares = Math.floor(sellShares);
            sellShares = roundShares(sellShares);
            if (!Number.isFinite(sellShares) || sellShares <= 0) {
                return { ok: false, reason: '卖出数量必须大于 0' };
            }
            if (sellShares > heldShares + 1e-4) {
                return {
                    ok: false,
                    reason: '仅持有 ' + formatShares(heldShares) + ' 股 ' + cmd.ticker + '，不足 ' + formatShares(sellShares) + ' 股',
                };
            }
            if (sellShares >= heldShares - 1e-4) {
                sellShares = heldShares;
            }
            const avgCost = Number(existing.avgCost || existing.cost || price);
            const proceeds = roundMoney(sellShares * price);
            const fee = roundMoney(proceeds * feeRate);
            const costBasis = roundMoney(sellShares * avgCost);
            const pnl = roundMoney(proceeds - costBasis - fee);
            const pnlPct = costBasis > 0 ? Math.round((pnl / costBasis) * 10000) / 100 : 0;

            nextAcct.cash = roundMoney(Number(nextAcct.cash || 0) + Math.max(0, proceeds - fee));
            nextAcct.realized_pnl = roundMoney(Number(nextAcct.realized_pnl || 0) + pnl);
            const remShares = roundShares(heldShares - sellShares);
            if (remShares <= 1e-5) {
                delete nextAcct.positions[cmd.ticker];
            } else {
                nextAcct.positions[cmd.ticker].shares = remShares;
            }
            nextAcct.trade_count = Number(nextAcct.trade_count || 0) + 1;
            nextAcct.updated_at = ts;

            const finalRem = remShares <= 1e-5 ? 0 : remShares;
            const snapshotText = (finalRem > 0 ? ('持仓 多' + formatShares(finalRem) + '股') : '已清仓') + ' · 现金 ' + formatUsd(nextAcct.cash);
            const actionLabel = finalRem === 0 ? '清仓卖出' : '卖出';

            return {
                ok: true,
                account: nextAcct,
                trade: {
                    kind: 'sell',
                    ticker: cmd.ticker,
                    shares: sellShares,
                    price: price,
                    notional: proceeds,
                    fee: fee,
                    pnl: pnl,
                    pnlPct: pnlPct,
                    isWhale: false,
                    isAllIn: false,
                    remainingShares: finalRem,
                    remainingCash: nextAcct.cash,
                    snapshotText: snapshotText,
                    summary: actionLabel + ' ' + cmd.ticker + ' ×' + formatShares(sellShares) + ' @ ' + formatPrice(price) + ' · ' + (pnl >= 0 ? '盈 ' : '亏 ') + formatSignedUsd(pnl) + ' (' + formatPct(pnlPct) + ')',
                },
            };
        }

        // -----------------------------------------------------------------
        // SHORT (保证金杠杆做空)
        // -----------------------------------------------------------------
        if (cmd.kind === 'short') {
            if (existing && existing.side === 'long' && Number(existing.shares) > 1e-6) {
                return {
                    ok: false,
                    reason: '持有 ' + cmd.ticker + ' 多仓，请先 卖出(sell) ' + cmd.ticker,
                };
            }
            const availCash = Number(nextAcct.cash || 0);
            const rawMargin = cmd.isAll ? availCash : Number(cmd.amount || 0);
            if (!Number.isFinite(rawMargin) || rawMargin <= 0) {
                return { ok: false, reason: '做空保证金金额必须大于 0' };
            }
            if (rawMargin > availCash + 1e-6) {
                return {
                    ok: false,
                    reason: '可用现金不足做空保证金（需 ' + formatUsd(rawMargin) + '，可用 ' + formatUsd(availCash) + '）',
                };
            }
            const marginPosted = roundMoney(Math.min(availCash, rawMargin));
            if (!cmd.isAll && marginPosted < minTradeUsd) {
                return { ok: false, reason: '做空最低保证金 ' + formatUsd(minTradeUsd) };
            }
            const leverage = clampNum(c.short_leverage, 1, 10, 2);
            const fee = roundMoney(marginPosted * leverage * feeRate);
            const actualMargin = roundMoney(Math.max(0, marginPosted - fee));
            if (actualMargin <= 0) {
                return { ok: false, reason: '扣除手续费后保证金不足' };
            }
            const notional = roundMoney(actualMargin * leverage);
            let shares = allowFrac
                ? roundShares(notional / price)
                : Math.floor((notional + 1e-9) / price);
            if (shares <= 0) {
                return { ok: false, reason: '保证金不足做空 1 股 ' + cmd.ticker };
            }

            const prevShares = existing ? Number(existing.shares || 0) : 0;
            const prevEntry = existing ? Number(existing.entryPrice || price) : 0;
            const prevMargin = existing ? Number(existing.margin || 0) : 0;
            const nextShares = roundShares(prevShares + shares);
            const weightedEntry = nextShares > 0
                ? roundPrice(((prevShares * prevEntry) + (shares * price)) / nextShares)
                : price;
            const nextMargin = roundMoney(prevMargin + actualMargin);

            nextAcct.cash = roundMoney(Math.max(0, availCash - marginPosted));
            nextAcct.positions[cmd.ticker] = {
                side: 'short',
                ticker: cmd.ticker,
                shares: nextShares,
                entryPrice: weightedEntry,
                margin: nextMargin,
                leverage: leverage,
                openedAt: (existing && existing.openedAt) || ts,
            };
            nextAcct.trade_count = Number(nextAcct.trade_count || 0) + 1;
            nextAcct.updated_at = ts;

            const isAllIn = cmd.isAll || (availCash > 0 && marginPosted >= availCash * 0.96);
            const isWhale = notional >= whaleThreshold || (isAllIn && actualMargin >= minTradeUsd);
            const snapshotText = '持仓 空' + formatShares(nextShares) + '股(' + leverage + 'x) · 现金 ' + formatUsd(nextAcct.cash);

            return {
                ok: true,
                account: nextAcct,
                trade: {
                    kind: 'short',
                    ticker: cmd.ticker,
                    shares: shares,
                    price: price,
                    notional: notional,
                    margin: actualMargin,
                    leverage: leverage,
                    fee: fee,
                    isAllIn: isAllIn,
                    isWhale: isWhale,
                    remainingShares: nextShares,
                    remainingCash: nextAcct.cash,
                    snapshotText: snapshotText,
                    summary: '做空(' + leverage + 'x) ' + cmd.ticker + ' 保证金 ' + formatUsd(actualMargin) + ' (空' + formatShares(shares) + '股 @ ' + formatPrice(price) + ')',
                },
            };
        }

        // -----------------------------------------------------------------
        // COVER (平空回补)
        // -----------------------------------------------------------------
        if (cmd.kind === 'cover') {
            if (!existing || existing.side !== 'short' || Number(existing.shares) <= 1e-6) {
                return { ok: false, reason: '未持有 ' + cmd.ticker + ' 空头仓位' };
            }
            const heldShares = Number(existing.shares);
            let coverShares = cmd.isAll ? heldShares : Number(cmd.shares || 0);
            if (!allowFrac) coverShares = Math.floor(coverShares);
            coverShares = roundShares(coverShares);
            if (!Number.isFinite(coverShares) || coverShares <= 0) {
                return { ok: false, reason: '平空数量必须大于 0' };
            }
            if (coverShares > heldShares + 1e-4) {
                return {
                    ok: false,
                    reason: '仅做空 ' + formatShares(heldShares) + ' 股 ' + cmd.ticker + '，不足 ' + formatShares(coverShares) + ' 股',
                };
            }
            if (coverShares >= heldShares - 1e-4) {
                coverShares = heldShares;
            }
            const ratio = coverShares / heldShares;
            const entryPrice = Number(existing.entryPrice || price);
            const totalMargin = Number(existing.margin || 0);
            const releasedMargin = roundMoney(totalMargin * ratio);
            const grossPnl = roundMoney((entryPrice - price) * coverShares);
            const fee = roundMoney(coverShares * price * feeRate);
            const netPnl = roundMoney(grossPnl - fee);
            const payout = roundMoney(Math.max(0, releasedMargin + netPnl));
            const pnlPct = releasedMargin > 0 ? Math.round((netPnl / releasedMargin) * 10000) / 100 : 0;

            nextAcct.cash = roundMoney(Number(nextAcct.cash || 0) + payout);
            nextAcct.realized_pnl = roundMoney(Number(nextAcct.realized_pnl || 0) + netPnl);

            const remShares = roundShares(heldShares - coverShares);
            if (remShares <= 1e-5) {
                delete nextAcct.positions[cmd.ticker];
            } else {
                nextAcct.positions[cmd.ticker].shares = remShares;
                nextAcct.positions[cmd.ticker].margin = roundMoney(Math.max(0, totalMargin - releasedMargin));
            }
            nextAcct.trade_count = Number(nextAcct.trade_count || 0) + 1;
            nextAcct.updated_at = ts;

            const finalRem = remShares <= 1e-5 ? 0 : remShares;
            const snapshotText = (finalRem > 0 ? ('持仓 空' + formatShares(finalRem) + '股') : '已平空') + ' · 现金 ' + formatUsd(nextAcct.cash);

            return {
                ok: true,
                account: nextAcct,
                trade: {
                    kind: 'cover',
                    ticker: cmd.ticker,
                    shares: coverShares,
                    price: price,
                    notional: roundMoney(coverShares * price),
                    pnl: netPnl,
                    pnlPct: pnlPct,
                    fee: fee,
                    isAllIn: false,
                    isWhale: false,
                    remainingShares: finalRem,
                    remainingCash: nextAcct.cash,
                    snapshotText: snapshotText,
                    summary: '平空 ' + cmd.ticker + ' ×' + formatShares(coverShares) + ' @ ' + formatPrice(price) + ' · ' + (netPnl >= 0 ? '盈 ' : '亏 ') + formatSignedUsd(netPnl) + ' (' + formatPct(pnlPct) + ')',
                },
            };
        }

        return { ok: false, reason: '不支持的交易指令' };
    }

    function checkLiquidationsForSymbol(symbol, markPrice, accountsMap, cfg, nowMs) {
        const sym = normalizeTicker(symbol);
        const price = Number(markPrice);
        if (!sym || !Number.isFinite(price) || price <= 0) return [];
        const maintRatio = clampNum(cfg && cfg.liquidation_maint_ratio, 0.05, 0.8, 0.2);
        const ts = typeof nowMs === 'number' ? nowMs : Date.now();
        const events = [];

        accountsMap.forEach(function (acct, acctKey) {
            const pos = acct && acct.positions && acct.positions[sym];
            if (!pos || pos.side !== 'short' || Number(pos.shares) <= 1e-6) return;
            const shares = Number(pos.shares);
            const entryPrice = Number(pos.entryPrice || price);
            const margin = Number(pos.margin || 0);
            if (margin <= 0) return;

            const unrealizedPnl = (entryPrice - price) * shares;
            const posEquity = margin + unrealizedPnl;
            if (posEquity <= margin * maintRatio) {
                const updated = JSON.parse(JSON.stringify(acct));
                const residual = roundMoney(Math.max(0, posEquity));
                const loss = roundMoney(residual - margin);
                updated.cash = roundMoney(Number(updated.cash || 0) + residual);
                updated.realized_pnl = roundMoney(Number(updated.realized_pnl || 0) + loss);
                updated.liquidation_count = Number(updated.liquidation_count || 0) + 1;
                updated.updated_at = ts;
                delete updated.positions[sym];

                events.push({
                    acctKey: acctKey,
                    account: updated,
                    ticker: sym,
                    shares: shares,
                    entryPrice: entryPrice,
                    markPrice: price,
                    marginLost: roundMoney(margin - residual),
                    loss: loss,
                    remainingCash: updated.cash,
                    snapshotText: '空仓强平 · 现金 ' + formatUsd(updated.cash),
                    priority: clampNum(updated.guard_priority, 0, 3, 0),
                    ts: ts,
                });
            }
        });

        return events;
    }

    return {
        DEFAULT_CONFIG: DEFAULT_CONFIG,
        STOCK_DIRECTORY: STOCK_DIRECTORY,
        POPULAR_FALLBACK_POOL: POPULAR_FALLBACK_POOL,
        clampNum: clampNum,
        roundMoney: roundMoney,
        roundPrice: roundPrice,
        roundShares: roundShares,
        sanitizeKvKey: sanitizeKvKey,
        normalizeTicker: normalizeTicker,
        parseTickerList: parseTickerList,
        formatUsd: formatUsd,
        formatSignedUsd: formatSignedUsd,
        formatPrice: formatPrice,
        formatPct: formatPct,
        formatShares: formatShares,
        formatVolume: formatVolume,
        accountKeyForPlayer: accountKeyForPlayer,
        mapBiliGuardToPriority: mapBiliGuardToPriority,
        priorityToGuardMeta: priorityToGuardMeta,
        resolvePlayerPriority: resolvePlayerPriority,
        checkFanMedalGate: checkFanMedalGate,
        getUsMarketSession: getUsMarketSession,
        parseDanmakuCommand: parseDanmakuCommand,
        getLockDurationMsForPriority: getLockDurationMsForPriority,
        createFocusScheduler: createFocusScheduler,
        createPosCardQueue: createPosCardQueue,
        createInitialAccount: createInitialAccount,
        computeAccountMetrics: computeAccountMetrics,
        computeLeaderboard: computeLeaderboard,
        computeRoomSymbolStats: computeRoomSymbolStats,
        computeGlobalBattleStats: computeGlobalBattleStats,
        computeTopHotTickers: computeTopHotTickers,
        executeTrade: executeTrade,
        checkLiquidationsForSymbol: checkLiquidationsForSymbol,
    };
});
