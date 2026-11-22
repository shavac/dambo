/**
 * Dambo Pure-Web Plugin SDK
 *
 * Single source of truth: `dambo-core/assets/dambo.js` (embedded at compile time and served at `GET /sdk/dambo.js`).
 *
 * Constraints:
 * - Zero external dependencies and zero desktop/Tauri bridge globals (standard Web APIs only).
 * - Compatible with both OBS Browser Source and Dambo desktop transparent overlay windows.
 *
 * @version 1.1.0
 */
(function (root, factory) {
    if (typeof module === 'object' && module.exports) {
        module.exports = factory();
    } else {
        root.Dambo = factory();
    }
})(typeof globalThis !== 'undefined' ? globalThis : window, function () {
    'use strict';

    /**
     * 直播间轻量实时状态快照（Channel A `RoomState` 帧与 `SnapshotData` 基础字段）。
     * @typedef {Object} RoomStateSnapshot
     * @property {number} room_id 直播间真实数字房间号（未连接时为 `0`）
     * @property {string} room_uname 主播昵称
     * @property {string} room_title 直播间标题
     * @property {boolean} is_live 是否处于开播状态（`live_status === 1`）
     * @property {boolean} connected 采集器是否已建立 WebSocket 连接
     */

    /**
     * Channel A (`/ws`) 推送的单条实时视觉事件或历史回放条目。
     * 仅关注 OBS / 悬浮窗展示，包含全量头像 URL（`face`）与表情贴图映射。
     * @typedef {Object} SnapshotItem
     * @property {number} id 环形缓冲单调递增内部 ID（实时/快照渲染去重参考）
     * @property {number} seq 单场次严格递增序列号（状态通知类可能为 `0`）
     * @property {string} kind 事件类型：`chat` | `gift` | `superchat` | `guard` | `guard_enter` | `enter` | `follow` | `special_follow` | `mutual_follow` | `share` | `like` | `live` | `preparing` | `room_change` | `room_silent` | `room_block` | `superchat_delete` | `warning` | `cut_off` | `reconnected`
     * @property {number=} msg_type 原始互动消息子类型（仅 `INTERACT_WORD*` 事件出现：1=进房, 2=关注, 3=分享, 4=特别关注, 5=互粉）
     * @property {number} uid 发送者 B 站 UID（系统事件通常为 `0`）
     * @property {string} uname 发送者昵称
     * @property {string=} face 用户头像 URL（可能缺省；Channel A 保留全量可用头像）
     * @property {string=} medal_name 粉丝勋章名称（受服务端 `[feed].show_medal` 开关控制）
     * @property {number=} medal_level 粉丝勋章等级
     * @property {string} content 事件文本内容（如弹幕正文、礼物 `"小心心 x2"`、SC 留言、系统文案）
     * @property {string=} emoticon 弹幕大表情图片 URL（当 `DANMU_MSG` 为表情弹幕时出现）
     * @property {Object.<string, string>=} emots 弹幕内嵌文字表情映射表（`"[dog]" -> URL`）
     * @property {number} price 金额（单位：金瓜子，`1000` 金瓜子 = `¥1`；免费礼物/普通事件为 `0`）
     * @property {number=} num 礼物数量或大航海开通月数
     * @property {number=} guard_level 大航海等级（`1`=总督, `2`=提督, `3`=舰长）
     * @property {number[]=} ids 被撤回的 SuperChat ID 列表（仅 `kind === "superchat_delete"` 时出现）
     * @property {number} timestamp 13 位 Unix 毫秒时间戳
     */

    /**
     * Channel A (`/ws`) 首帧及滞后恢复时下发的环形历史快照载荷。
     * @typedef {Object} SnapshotData
     * @property {number} room_id 直播间房间号
     * @property {string} room_uname 主播昵称
     * @property {string} room_title 直播间标题
     * @property {boolean} is_live 是否开播中
     * @property {boolean} connected 采集器连接状态
     * @property {SnapshotItem[]} items 环形历史缓冲事件（仅保留 `chat`/`gift`/`superchat`/`guard`，上限由 `[feed].max_history` 控制，默认 `300`）
     */

    /**
     * Channel A (`/ws`) WebSocket JSON 封装信封。
     * @typedef {{type: 'Snapshot', data: SnapshotData} | {type: 'Event', data: SnapshotItem} | {type: 'RoomState', data: RoomStateSnapshot}} ChannelAMessage
     */

    /**
     * 场次结束汇总数据（仅写入 `.jsonl` `session_end`，保留类型作完整模型参考）。
     * @typedef {Object} SessionSummary
     * @property {number} chat_count 弹幕数
     * @property {number} like_count 点赞数
     * @property {number} guard_count 上舰单数
     * @property {number} gift_total_coin 付费礼物金瓜子总额
     * @property {number} superchat_total_coin 醒目留言金瓜子总额
     * @property {number} enter_count 进房数
     * @property {number} follow_count 关注数
     * @property {number} share_count 分享数
     */

    /**
     * Channel B (`/ws/event`) 建连首帧下发的完整房间元数据事件（对应后端 `RawBiliEvent` 且 `kind === "room_info"`）。
     * @typedef {Object} RoomInfoEvent
     * @property {number} seq 固定为 `0`
     * @property {number} offset_ms 固定为 `0`
     * @property {number} timestamp 13 位 Unix 毫秒时间戳
     * @property {number} room_id 直播间真实房间号
     * @property {string} room_uname 主播昵称
     * @property {string} kind 固定为 `"room_info"`
     * @property {number} uid 主播 UID
     * @property {string} uname 主播昵称
     * @property {string} content 直播间标题
     * @property {number=} short_id 直播间短号
     * @property {string=} title 直播间标题
     * @property {string=} area_name 分区名称
     * @property {string=} parent_area_name 父分区名称
     * @property {number=} live_status 开播状态（`1`=直播中, `0`=未开播, `2`=轮播中）
     * @property {number=} start_time 本场开播时间（Unix 秒时间戳，未开播时省略）
     * @property {string} raw_cmd 固定为 `"ROOM_INFO"`
     */

    /**
     * Channel B (`/ws/event`) 经三通道流量整形、蓄水池聚合或礼物连击结算后的标准事件项。
     * 仅付费事件（`superchat` / `guard` / `price > 0` 的 `gift`）保留 `face`，非付费互动一律省略 `face`。
     * @typedef {Object} EventItem
     * @property {number} seq 该事件（或聚合桶末条事件）的场次序号
     * @property {string} kind 事件类型
     * @property {number=} msg_type 原始互动 `msg_type`
     * @property {number} uid 触发用户 UID（或聚合桶首条代表用户）
     * @property {string} uname 触发用户昵称（或聚合桶首条代表用户）
     * @property {string=} face 头像 URL（仅付费事件保留）
     * @property {number} medal_level 粉丝勋章等级（未佩戴为 `0`；聚合桶取最高等级）
     * @property {string} content 标准化文本（礼物合并后如 `"小心心 x5"`）
     * @property {number} count 聚合数量 / 连击总数量 / 单条 `1`
     * @property {string[]} sample_users 参与该聚合桶的代表用户名列表（最多 5 个去重昵称）
     * @property {number} price 金瓜子总额（`1000` = `¥1`）
     * @property {number=} guard_level 大航海等级（`1`=总督, `2`=提督, `3`=舰长）
     * @property {string=} batch_combo_id 礼物连击批次 ID
     * @property {number[]=} ids 被撤回的 SuperChat ID 列表
     */

    /**
     * Channel B (`/ws/event`) 批次载荷。
     * @typedef {Object} EventBatchData
     * @property {number} batch_id 自增批次 ID
     * @property {number} room_id 直播间房间号
     * @property {string} room_uname 主播昵称
     * @property {0|1|2} priority 优先级：`0`=蓄水池定时批量, `1`=直通弹幕/连击结算/互动, `2`=SC/上舰/直播状态变更
     * @property {number} timestamp 13 位 Unix 毫秒时间戳
     * @property {EventItem[]} events 本批次事件列表
     */

    /**
     * Channel B (`/ws/event`) WebSocket JSON 封装信封。
     * @typedef {{type: 'RoomInfo', data: RoomInfoEvent} | {type: 'EventBatch', data: EventBatchData}} ChannelBMessage
     */

    /**
     * SDK 连接状态枚举。
     * @typedef {'connecting' | 'open' | 'reconnecting' | 'closed'} ConnectionState
     */

    /**
     * `Dambo.connect` 调度到事件回调的第二参数元信息。
     * @typedef {Object} EventMeta
     * @property {boolean} isSnapshot 是否来源于连接首帧 / 重连 `Snapshot` 历史回放（实时推送为 `false`）
     * @property {EventBatchData=} batch 当 `channel === 'event'` 时附带所属批次上下文
     */

    /**
     * `Dambo.connect(options)` 配置项。
     * @typedef {Object} ConnectOptions
     * @property {'feed' | 'event'=} channel 订阅通道：`'feed'` 连接 `/ws`（默认，适合视觉挂件）；`'event'` 连接 `/ws/event`（适合整形后事件流与下游分析）
     * @property {string=} url 显式指定 WebSocket URL，未传时自动基于当前 `window.location.host` 与 `channel` 拼接
     * @property {boolean=} autoReconnect 断开后是否启用指数退避自动重连，默认 `true`
     * @property {number=} reconnectDelayMs 初始重连延迟（毫秒），默认 `2000`
     * @property {number=} maxReconnectDelayMs 最大重连退避上限（毫秒），默认 `10000`
     * @property {number=} connectTimeoutMs 单次建连超时看门狗（毫秒，`0` 表示禁用），默认 `8000`
     * @property {boolean=} replaySnapshot 是否将 `Snapshot.items`（`/ws`）按顺序重放至 `event` 与具体 `kind` 回调并标记 `meta.isSnapshot = true`，默认 `false`
     * @property {boolean|null=} showLike 设为 `false` 时自动屏蔽 `kind === 'like'` 事件；默认 `null`（不自动过滤）
     * @property {((snapshot: SnapshotData) => void)=} onReset 收到新 `Snapshot` 时在重放历史前触发的清理回调
     */

    /**
     * `Dambo.utils.renderContent` 可选样式类名配置。
     * @typedef {Object} RenderContentOptions
     * @property {string=} stickerClass 弹幕大表情 `<img>` 的 CSS 类名，默认 `'dambo-sticker'`
     * @property {string=} emotClass 行内文字表情 `<img>` 的 CSS 类名，默认 `'dambo-emot'`
     */

    /**
     * `Dambo.utils.formatGuard` 返回的标准化上舰信息结构。
     * @typedef {Object} GuardFormattedInfo
     * @property {number} guardLevel 大航海等级数值（`1`=总督, `2`=提督, `3`=舰长, `0`=未知）
     * @property {string} guardName 大航海等级名称（`'总督'` | `'提督'` | `'舰长'` | `'大航海'`）
     * @property {string} badgeLabel 徽章简短标签（`'总督'` | `'提督'` | `'舰长'` | `'上舰'`）
     * @property {number} num 开通月数或数量（`>= 1`）
     * @property {string} monthsSuffix 当 `num > 1` 时为 `' ×N个月'`，否则为 `''`
     * @property {string} actionText 动作动词（如 `'开通了'` | `'自动续费了'` | `'续费了'` | `''`）
     * @property {string} highlightText 动作目标高亮文本（如 `'舰长'` | `'3个月提督'` | `'舰长 ×2个月'`）
     * @property {string} summaryText 清洗后的完整动作短句（如 `'开通了舰长'` | `'自动续费了3个月提督'`）
     */

    /**
     * `Dambo.utils.remainingDurationMs` 可选配置。
     * @typedef {Object} RemainingDurationOptions
     * @property {boolean=} isSnapshot 是否为快照历史回放（为 `false` 时直接返回 `totalMs`），默认 `true`
     * @property {number=} minMs 未过期时的最小保留时长下限（毫秒），默认 `0`
     * @property {number=} now 当前毫秒时间戳，默认 `Date.now()`
     */

    const DEFAULT_AVATAR_SVG =
        'data:image/svg+xml;utf8,<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="%2364748b"><circle cx="12" cy="8" r="4"/><path d="M4 20c0-4 4-6 8-6s8 2 8 6"/></svg>';

    /**
     * @param {string} str
     * @returns {string}
     */
    function escapeRegExp(str) {
        return str.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    }

    /**
     * 规范化图片/资源 URL，自动将 B 站 CDN 的 `http://` 升级为 `https://`。
     * @param {string | null | undefined} url
     * @returns {string}
     */
    function normalizeUrl(url) {
        if (!url || typeof url !== 'string') return '';
        const trimmed = url.trim();
        if (!trimmed) return '';
        if (trimmed.startsWith('//')) {
            return 'https:' + trimmed;
        }
        return trimmed.replace(
            /^http:\/\/((?:i\d|s\d)\.hdslb\.com|archive\.biliimg\.com)\//i,
            'https://$1/'
        );
    }

    /**
     * 读取当前页面 URL 查询参数。
     * @param {string} name 参数名
     * @param {string|null} [fallback=null] 缺省返回值
     * @returns {string|null}
     */
    function getQueryParam(name, fallback) {
        const defaultValue = fallback === undefined ? null : fallback;
        try {
            const search =
                typeof window !== 'undefined' && window.location
                    ? window.location.search
                    : '';
            const params = new URLSearchParams(search);
            const val = params.get(name);
            return val === null ? defaultValue : val;
        } catch (_) {
            return defaultValue;
        }
    }

    /**
     * 将 URL 查询参数解析为布尔值（`'1'` / `'true'` / `'yes'` 为 `true`，`'0'` / `'false'` / `'no'` 为 `false`）。
     * @param {string} name 参数名
     * @param {boolean} [fallback=false] 缺省或无法识别时的返回值
     * @returns {boolean}
     */
    function getQueryBool(name, fallback) {
        const defaultValue = Boolean(fallback);
        const raw = getQueryParam(name, null);
        if (raw === null) return defaultValue;
        const normalized = raw.trim().toLowerCase();
        if (normalized === '1' || normalized === 'true' || normalized === 'yes') return true;
        if (normalized === '0' || normalized === 'false' || normalized === 'no') return false;
        return defaultValue;
    }

    /**
     * 判断事件对象或 `kind` 字符串是否为醒目留言（兼容 `'superchat'` 与 `'super_chat'`）。
     * @param {string | SnapshotItem | EventItem | null | undefined} itemOrKind
     * @returns {boolean}
     */
    function isSuperChat(itemOrKind) {
        const kind = typeof itemOrKind === 'string' ? itemOrKind : itemOrKind && itemOrKind.kind;
        return kind === 'superchat' || kind === 'super_chat';
    }

    /**
     * 判断事件是否属于付费事件（醒目留言、大航海、或 `price > 0` 的礼物），与服务端头像保留策略一致。
     * @param {SnapshotItem | EventItem | null | undefined} item
     * @returns {boolean}
     */
    function isPaidEvent(item) {
        if (!item) return false;
        if (isSuperChat(item.kind) || item.kind === 'guard') return true;
        if (item.kind === 'gift') return Number(item.price || 0) > 0;
        return false;
    }

    /**
     * 将后端金瓜子价格换算为人民币金额字符串（`1000` 金瓜子 = `1` 元）；免费事件返回空字符串 `''`。
     * @param {number | null | undefined} priceSeeds 金瓜子数量
     * @param {string} [currencySymbol='¥'] 货币符号前缀
     * @returns {string}
     */
    function formatPrice(priceSeeds, currencySymbol) {
        const prefix = currencySymbol === undefined ? '¥' : String(currencySymbol);
        const numeric = Number(priceSeeds || 0);
        if (!Number.isFinite(numeric) || numeric <= 0) return '';
        const cny = numeric / 1000;
        const formatted = Number.isInteger(cny)
            ? String(cny)
            : String(parseFloat(cny.toFixed(2)));
        return prefix + formatted;
    }

    /**
     * 将 `guard_level` 数值映射为大航海中文名称；非有效等级返回 `''`。
     * @param {number | null | undefined} level 大航海等级（1/2/3）
     * @returns {string}
     */
    function guardLevelName(level) {
        switch (Number(level)) {
            case 1:
                return '总督';
            case 2:
                return '提督';
            case 3:
                return '舰长';
            default:
                return '';
        }
    }

    /**
     * 清洗并结构化上舰（`kind === 'guard'`）事件文案，自动剥离 `USER_TOAST_MSG` 中的 `<%...%>` 标记、
     * 开头重复用户名及 `在主播(的)房间/直播间` 冗余前缀，提取动作动词与等级月数。
     * @param {SnapshotItem | EventItem | null | undefined} item
     * @returns {GuardFormattedInfo}
     */
    function formatGuard(item) {
        const levelNum = Number(item && item.guard_level) || 0;
        const resolvedName = guardLevelName(levelNum);
        const guardName = resolvedName || '大航海';
        const badgeLabel = resolvedName || '上舰';
        const rawNum = Number((item && (item.num || item.count)) || 1);
        const num = Number.isFinite(rawNum) && rawNum > 1 ? Math.floor(rawNum) : 1;
        const monthsSuffix = num > 1 ? ' ×' + num + '个月' : '';

        let raw = String((item && item.content) || '')
            .replace(/<%/g, '')
            .replace(/%>/g, '')
            .trim();

        const uname = item && typeof item.uname === 'string' ? item.uname.trim() : '';
        if (uname && raw.startsWith(uname)) {
            raw = raw.slice(uname.length).replace(/^[\s：:]+/, '').trim();
        }
        raw = raw.replace(/^在?主播(?:的)?(?:房间|直播间)\s*/, '').trim();

        let actionText = '';
        let highlightText = '';

        if (!raw) {
            actionText = '开通了';
            highlightText = guardName + monthsSuffix;
        } else {
            const actionMatch = raw.match(
                /^(自动续费了|续费了|开通了|升级了|升级为|赠送了|购买了)\s*(.*)$/
            );
            if (actionMatch) {
                actionText = actionMatch[1];
                highlightText = actionMatch[2].trim() || guardName + monthsSuffix;
            } else if (/^(?:舰长|提督|总督|大航海)(?:\s*[x×]\s*\d+)?$/i.test(raw)) {
                actionText = '开通了';
                highlightText = guardName + monthsSuffix;
            } else if (/开通|续费|赠送|升级|购买/.test(raw)) {
                actionText = '';
                highlightText = raw;
            } else {
                actionText = '开通了';
                highlightText = raw;
            }
        }

        return {
            guardLevel: resolvedName ? levelNum : 0,
            guardName: guardName,
            badgeLabel: badgeLabel,
            num: num,
            monthsSuffix: monthsSuffix,
            actionText: actionText,
            highlightText: highlightText,
            summaryText: actionText ? actionText + highlightText : highlightText,
        };
    }

    /**
     * 计算 SuperChat / Guard 推荐置顶停留时长（毫秒），与官方默认挂件档位一致。
     * @param {SnapshotItem | EventItem | null | undefined} item
     * @returns {number}
     */
    function superChatDurationMs(item) {
        if (item && item.kind === 'guard') return 90000;
        const cny = Number((item && item.price) || 0) / 1000;
        if (cny >= 1000) return 300000;
        if (cny >= 500) return 180000;
        if (cny >= 100) return 120000;
        return 60000;
    }

    /**
     * 计算事件在历史快照（`Snapshot`）回放或实时推送下的剩余展示毫秒数；已过期返回 `0`。
     * @param {SnapshotItem | EventItem | {timestamp?: number} | null | undefined} item
     * @param {number} totalMs 标准总驻留毫秒数
     * @param {RemainingDurationOptions | number} [options] 配置项或 `minMs` 下限数值
     * @returns {number} 剩余毫秒数（`<= 0` 表示已过期不应展示）
     */
    function remainingDurationMs(item, totalMs, options) {
        const total = Number(totalMs || 0);
        if (!Number.isFinite(total) || total <= 0) return 0;

        const opts =
            typeof options === 'number'
                ? { isSnapshot: true, minMs: options }
                : options || {};
        const isSnapshot = opts.isSnapshot !== false;
        if (!isSnapshot) {
            return total;
        }

        const ts = Number(item && item.timestamp);
        if (!Number.isFinite(ts) || ts <= 0) {
            return 0;
        }
        const now = Number.isFinite(opts.now) ? Number(opts.now) : Date.now();
        const remaining = total - (now - ts);
        if (!Number.isFinite(remaining) || remaining <= 0) {
            return 0;
        }
        const minMs = Number(opts.minMs || 0);
        const clamped = Math.min(total, remaining);
        return minMs > 0 ? Math.max(minMs, clamped) : clamped;
    }

    /**
     * 创建带懒加载、HTTPS 归一化与防死循环默认头像回退的 `<img>` 节点。
     * @param {string | SnapshotItem | EventItem | null | undefined} faceOrItem 头像 URL 或事件对象
     * @param {string} [className='dambo-avatar'] `<img>` CSS 类名
     * @returns {HTMLImageElement}
     */
    function createAvatar(faceOrItem, className) {
        const rawUrl =
            typeof faceOrItem === 'string'
                ? faceOrItem
                : faceOrItem && typeof faceOrItem.face === 'string'
                  ? faceOrItem.face
                  : '';
        const normalized = normalizeUrl(rawUrl);
        const avatar = document.createElement('img');
        avatar.className = className || 'dambo-avatar';
        avatar.src = normalized || DEFAULT_AVATAR_SVG;
        avatar.loading = 'lazy';
        avatar.onerror = function () {
            if (avatar.src !== DEFAULT_AVATAR_SVG) {
                avatar.src = DEFAULT_AVATAR_SVG;
            }
        };
        return avatar;
    }

    /**
     * 格式化房间状态文本（示例：`21452505(主播): 标题`、`... · Offline`、`Disconnected`）。
     * @param {RoomStateSnapshot | SnapshotData | null | undefined} roomState
     * @returns {string}
     */
    function formatRoomBadge(roomState) {
        if (!roomState || !roomState.room_id) return 'Disconnected';
        const head = roomState.room_uname
            ? roomState.room_id + '(' + roomState.room_uname + ')'
            : String(roomState.room_id);
        const label = roomState.room_title ? head + ': ' + roomState.room_title : head;
        if (!roomState.connected) return label + ' · Disconnected';
        return roomState.is_live ? label : label + ' · Offline';
    }

    /**
     * 安全地将消息正文、大表情贴图（`emoticon`）与行内文字表情（`emots`）渲染到容器 DOM 中（全程零 `innerHTML`）。
     * @param {HTMLElement} parentEl 目标容器节点
     * @param {SnapshotItem | EventItem | {content?: string, emoticon?: string, emots?: Object.<string, string>}} item 事件对象
     * @param {RenderContentOptions} [options] 自定义样式类配置
     * @returns {void}
     */
    function renderContent(parentEl, item, options) {
        if (!parentEl || !item) return;
        const stickerClass = (options && options.stickerClass) || 'dambo-sticker';
        const emotClass = (options && options.emotClass) || 'dambo-emot';

        if (item.emoticon) {
            const sticker = document.createElement('img');
            sticker.className = stickerClass;
            sticker.src = normalizeUrl(item.emoticon);
            sticker.alt = item.content || 'Sticker';
            sticker.loading = 'lazy';
            sticker.onerror = function () {
                sticker.replaceWith(document.createTextNode(item.content || ''));
            };
            parentEl.appendChild(sticker);
            return;
        }

        const text = item.content || '';
        const emots = item.emots;
        if (!emots || typeof emots !== 'object') {
            parentEl.appendChild(document.createTextNode(text));
            return;
        }

        const keys = Object.keys(emots).filter(function (k) {
            return Boolean(k && emots[k]);
        });
        if (keys.length === 0) {
            parentEl.appendChild(document.createTextNode(text));
            return;
        }

        keys.sort(function (a, b) {
            return b.length - a.length;
        });
        const pattern = new RegExp(keys.map(escapeRegExp).join('|'), 'g');
        const frag = document.createDocumentFragment();
        let lastIdx = 0;
        let match;
        while ((match = pattern.exec(text)) !== null) {
            if (match.index > lastIdx) {
                frag.appendChild(
                    document.createTextNode(text.slice(lastIdx, match.index))
                );
            }
            const token = match[0];
            const img = document.createElement('img');
            img.className = emotClass;
            img.src = normalizeUrl(emots[token]);
            img.alt = token;
            img.title = token;
            img.loading = 'lazy';
            img.onerror = function () {
                img.replaceWith(document.createTextNode(token));
            };
            frag.appendChild(img);
            lastIdx = pattern.lastIndex;
        }
        if (lastIdx < text.length) {
            frag.appendChild(document.createTextNode(text.slice(lastIdx)));
        }
        parentEl.appendChild(frag);
    }

    /**
     * @param {'feed' | 'event'} channel
     * @param {string | undefined} customUrl
     * @returns {string}
     */
    function resolveWsUrl(channel, customUrl) {
        if (customUrl) return String(customUrl);
        const path = channel === 'event' ? '/ws/event' : '/ws';
        if (typeof window !== 'undefined' && window.location && window.location.host) {
            const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
            return protocol + '//' + window.location.host + path;
        }
        return 'ws://localhost:3838' + path;
    }

    /**
     * Dambo WebSocket 客户端实例。
     * 通过 `Dambo.connect(options)` 创建，支持事件订阅、断线指数退避抖动重连、建连看门狗与状态读取。
     */
    class DamboClient {
        /**
         * @param {ConnectOptions} [options]
         */
        constructor(options) {
            const opts = options || {};
            /** @type {'feed' | 'event'} */
            this._channel = opts.channel === 'event' ? 'event' : 'feed';
            /** @type {string} */
            this._url = resolveWsUrl(this._channel, opts.url);
            /** @type {boolean} */
            this._autoReconnect = opts.autoReconnect !== false;
            /** @type {number} */
            this._reconnectDelayMs = Math.max(200, Number(opts.reconnectDelayMs || 2000));
            /** @type {number} */
            this._maxReconnectDelayMs = Math.max(
                this._reconnectDelayMs,
                Number(opts.maxReconnectDelayMs || 10000)
            );
            const rawTimeout =
                opts.connectTimeoutMs === undefined ? 8000 : Number(opts.connectTimeoutMs);
            /** @type {number} */
            this._connectTimeoutMs =
                Number.isFinite(rawTimeout) && rawTimeout > 0 ? Math.max(500, rawTimeout) : 0;
            /** @type {boolean} */
            this._replaySnapshot = Boolean(opts.replaySnapshot);
            /** @type {boolean | null} */
            this._showLike = opts.showLike === undefined ? null : opts.showLike;

            /** @type {Map<string, Function[]>} */
            this._listeners = new Map();
            /** @type {WebSocket | null} */
            this._ws = null;
            /** @type {number | null} */
            this._reconnectTimer = null;
            /** @type {number | null} */
            this._connectTimer = null;
            /** @type {number} */
            this._reconnectAttempt = 0;
            /** @type {boolean} */
            this._manualClosed = false;
            /** @type {ConnectionState} */
            this._state = 'connecting';
            /** @type {RoomStateSnapshot | null} */
            this._roomState = null;
            /** @type {SnapshotData | null} */
            this._snapshot = null;
            /** @type {RoomInfoEvent | null} */
            this._roomInfo = null;

            if (typeof opts.onReset === 'function') {
                this.on('reset', opts.onReset);
            }

            this._bindLifecycleHooks();
        }

        /** @private */
        _bindLifecycleHooks() {
            if (typeof window === 'undefined' || typeof window.addEventListener !== 'function') {
                return;
            }
            const self = this;
            this._onWindowOnline = function () {
                if (!self._manualClosed && self._state === 'reconnecting') {
                    self.reconnect();
                }
            };
            this._onWindowPageHide = function () {
                self._clearReconnectTimer();
                self._clearConnectTimer();
                const socket = self._ws;
                self._ws = null;
                if (socket) {
                    try {
                        socket.close(1000, 'pagehide');
                    } catch (_) {}
                }
            };
            window.addEventListener('online', this._onWindowOnline);
            window.addEventListener('pagehide', this._onWindowPageHide);
        }

        /**
         * 注册事件监听器并返回取消订阅函数。
         * @param {string} eventName 事件名：生命周期（`open`/`close`/`error`/`reconnect`/`state`/`reset`）、Channel A（`snapshot`/`room_state`/`event`/具体 `kind`）、Channel B（`room_info`/`batch`/`batch_item`/`event`/具体 `kind`）或通配符 `'*'`
         * @param {Function} handler 回调函数
         * @returns {() => void} 取消当前监听器的函数
         */
        on(eventName, handler) {
            if (!eventName || typeof handler !== 'function') {
                return function () {};
            }
            let list = this._listeners.get(eventName);
            if (!list) {
                list = [];
                this._listeners.set(eventName, list);
            }
            list.push(handler);
            const self = this;
            return function () {
                self.off(eventName, handler);
            };
        }

        /**
         * 注册仅触发一次的事件监听器并返回取消订阅函数。
         * @param {string} eventName 事件名
         * @param {Function} handler 回调函数
         * @returns {() => void} 取消当前监听器的函数
         */
        once(eventName, handler) {
            if (!eventName || typeof handler !== 'function') {
                return function () {};
            }
            const self = this;
            /** @param {...any} args */
            const wrapper = function () {
                self.off(eventName, wrapper);
                handler.apply(null, arguments);
            };
            return this.on(eventName, wrapper);
        }

        /**
         * 移除事件监听器；省略 `handler` 时移除该事件名下的全部监听器。
         * @param {string} eventName 事件名
         * @param {Function} [handler] 指定移除的函数引用
         * @returns {void}
         */
        off(eventName, handler) {
            if (!eventName) return;
            if (!handler) {
                this._listeners.delete(eventName);
                return;
            }
            const list = this._listeners.get(eventName);
            if (!list) return;
            const next = list.filter(function (fn) {
                return fn !== handler;
            });
            if (next.length === 0) {
                this._listeners.delete(eventName);
            } else {
                this._listeners.set(eventName, next);
            }
        }

        /**
         * 主动断开连接并停止后续自动重连。
         * @returns {void}
         */
        disconnect() {
            this._manualClosed = true;
            this._clearReconnectTimer();
            this._clearConnectTimer();
            const socket = this._ws;
            this._ws = null;
            this._setState('closed');
            if (socket) {
                try {
                    socket.close(1000, 'client disconnect');
                } catch (_) {
                    // Ignore already-closed WebSocket
                }
            }
        }

        /**
         * `disconnect()` 的语义别名，符合原生 WebSocket 习惯。
         * @returns {void}
         */
        close() {
            this.disconnect();
        }

        /**
         * 立即重置重连计数并重新建立 WebSocket 连接。
         * @returns {void}
         */
        reconnect() {
            this._manualClosed = false;
            this._clearReconnectTimer();
            this._clearConnectTimer();
            this._reconnectAttempt = 0;
            const old = this._ws;
            this._ws = null;
            if (old) {
                try {
                    old.close(1000, 'client reconnect');
                } catch (_) {
                    // Ignore already-closed WebSocket
                }
            }
            this._openSocket();
        }

        /**
         * 获取当前 WebSocket 连接生命周期状态。
         * @returns {ConnectionState}
         */
        getState() {
            return this._state;
        }

        /**
         * 获取最近一次接收到的直播间实时状态（Channel A 可用，未收到前返回 `null`）。
         * @returns {RoomStateSnapshot | null}
         */
        getRoomState() {
            return this._roomState;
        }

        /**
         * 获取最近一次接收到的完整历史快照载荷（Channel A 可用，未收到前返回 `null`）。
         * @returns {SnapshotData | null}
         */
        getSnapshot() {
            return this._snapshot;
        }

        /**
         * 获取最近一次接收到的完整房间元数据（Channel B 可用，未收到前返回 `null`）。
         * @returns {RoomInfoEvent | null}
         */
        getRoomInfo() {
            return this._roomInfo;
        }

        /**
         * 获取当前连接的 WebSocket 完整地址。
         * @returns {string}
         */
        getUrl() {
            return this._url;
        }

        /** @private */
        _clearReconnectTimer() {
            if (this._reconnectTimer !== null) {
                clearTimeout(this._reconnectTimer);
                this._reconnectTimer = null;
            }
        }

        /** @private */
        _clearConnectTimer() {
            if (this._connectTimer !== null) {
                clearTimeout(this._connectTimer);
                this._connectTimer = null;
            }
        }

        /**
         * @private
         * @param {ConnectionState} nextState
         */
        _setState(nextState) {
            if (this._state === nextState) return;
            this._state = nextState;
            this._emit('state', nextState);
        }

        /**
         * @private
         * @param {string} eventName
         * @param {...any} args
         */
        _emit(eventName, ...args) {
            const list = this._listeners.get(eventName);
            if (!list || list.length === 0) return;
            const snapshot = list.slice();
            for (let i = 0; i < snapshot.length; i++) {
                try {
                    snapshot[i].apply(null, args);
                } catch (err) {
                    console.error('[dambo-sdk] listener error on "' + eventName + '":', err);
                }
            }
        }

        /**
         * @private
         * @param {SnapshotItem | EventItem} item
         * @param {EventMeta} meta
         */
        _dispatchEventItem(item, meta) {
            if (!item || !item.kind) return;
            if (this._showLike === false && item.kind === 'like') return;
            this._emit('event', item, meta);
            this._emit(item.kind, item, meta);
            if (item.kind === 'super_chat') {
                this._emit('superchat', item, meta);
            }
        }

        /**
         * @private
         * @param {ChannelAMessage | ChannelBMessage} msg
         */
        _handleMessage(msg) {
            if (!msg || typeof msg !== 'object') return;
            this._emit('*', msg);

            if (this._channel === 'feed') {
                if (msg.type === 'Snapshot' && msg.data) {
                    const data = /** @type {SnapshotData} */ (msg.data);
                    this._snapshot = data;
                    this._roomState = {
                        room_id: data.room_id,
                        room_uname: data.room_uname,
                        room_title: data.room_title || '',
                        is_live: Boolean(data.is_live),
                        connected: Boolean(data.connected),
                    };
                    this._emit('reset', data);
                    this._emit('snapshot', data);
                    this._emit('room_state', this._roomState);
                    if (this._replaySnapshot && Array.isArray(data.items)) {
                        const meta = { isSnapshot: true };
                        for (let i = 0; i < data.items.length; i++) {
                            this._dispatchEventItem(data.items[i], meta);
                        }
                    }
                } else if (msg.type === 'Event' && msg.data) {
                    this._dispatchEventItem(
                        /** @type {SnapshotItem} */ (msg.data),
                        { isSnapshot: false }
                    );
                } else if (msg.type === 'RoomState' && msg.data) {
                    this._roomState = /** @type {RoomStateSnapshot} */ (msg.data);
                    this._emit('room_state', this._roomState);
                }
                return;
            }

            if (msg.type === 'RoomInfo' && msg.data) {
                this._roomInfo = /** @type {RoomInfoEvent} */ (msg.data);
                this._emit('room_info', this._roomInfo);
            } else if (msg.type === 'EventBatch' && msg.data) {
                const batch = /** @type {EventBatchData} */ (msg.data);
                this._emit('batch', batch);
                if (Array.isArray(batch.events)) {
                    const meta = { isSnapshot: false, batch: batch };
                    for (let i = 0; i < batch.events.length; i++) {
                        const item = batch.events[i];
                        if (!item) continue;
                        if (this._showLike === false && item.kind === 'like') continue;
                        this._emit('batch_item', item, batch);
                        this._dispatchEventItem(item, meta);
                    }
                }
            }
        }

        /** @private */
        _openSocket() {
            this._clearReconnectTimer();
            this._clearConnectTimer();
            if (this._manualClosed) return;

            this._setState(this._reconnectAttempt > 0 ? 'reconnecting' : 'connecting');

            const socket = new WebSocket(this._url);
            this._ws = socket;
            const self = this;

            if (this._connectTimeoutMs > 0) {
                this._connectTimer = setTimeout(function () {
                    self._connectTimer = null;
                    if (socket !== self._ws || self._manualClosed) return;
                    if (self._state === 'connecting' || self._state === 'reconnecting') {
                        try {
                            socket.close();
                        } catch (_) {}
                    }
                }, this._connectTimeoutMs);
            }

            socket.onopen = function () {
                if (socket !== self._ws || self._manualClosed) return;
                self._clearConnectTimer();
                self._reconnectAttempt = 0;
                self._setState('open');
                self._emit('open');
            };

            socket.onmessage = function (ev) {
                if (socket !== self._ws || self._manualClosed) return;
                try {
                    const parsed = JSON.parse(ev.data);
                    self._handleMessage(parsed);
                } catch (err) {
                    console.error('[dambo-sdk] failed to parse WebSocket payload:', err);
                    self._emit('error', err);
                }
            };

            socket.onerror = function (ev) {
                if (socket !== self._ws || self._manualClosed) return;
                self._emit('error', ev);
                try {
                    socket.close();
                } catch (_) {
                    // Ignore errors closing failed socket
                }
            };

            socket.onclose = function (ev) {
                if (socket !== self._ws) return;
                self._clearConnectTimer();
                self._ws = null;
                self._emit('close', ev);
                if (self._manualClosed || !self._autoReconnect) {
                    self._setState('closed');
                    return;
                }
                self._reconnectAttempt += 1;
                const baseDelay = Math.min(
                    self._maxReconnectDelayMs,
                    self._reconnectDelayMs * Math.pow(2, self._reconnectAttempt - 1)
                );
                const jitter = baseDelay * 0.15 * (Math.random() * 2 - 1);
                const delayMs = Math.max(
                    200,
                    Math.min(self._maxReconnectDelayMs, Math.round(baseDelay + jitter))
                );
                self._setState('reconnecting');
                self._emit('reconnect', {
                    attempt: self._reconnectAttempt,
                    delayMs: delayMs,
                });
                self._reconnectTimer = setTimeout(function () {
                    self._reconnectTimer = null;
                    self._openSocket();
                }, delayMs);
            };
        }
    }

    /**
     * 创建并立即连接 Dambo 实时 WebSocket 客户端。
     * @param {ConnectOptions} [options] 连接与重连配置
     * @returns {DamboClient}
     */
    function connect(options) {
        const client = new DamboClient(options);
        client._openSocket();
        return client;
    }

    return {
        VERSION: '1.1.0',
        DamboClient: DamboClient,
        connect: connect,
        utils: {
            DEFAULT_AVATAR_SVG: DEFAULT_AVATAR_SVG,
            normalizeUrl: normalizeUrl,
            isSuperChat: isSuperChat,
            isPaidEvent: isPaidEvent,
            formatPrice: formatPrice,
            guardLevelName: guardLevelName,
            formatGuard: formatGuard,
            superChatDurationMs: superChatDurationMs,
            remainingDurationMs: remainingDurationMs,
            createAvatar: createAvatar,
            formatRoomBadge: formatRoomBadge,
            renderContent: renderContent,
            getQueryParam: getQueryParam,
            getQueryBool: getQueryBool,
        },
    };
});
