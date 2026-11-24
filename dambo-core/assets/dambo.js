/**
 * Dambo Pure-Web Plugin SDK
 *
 * Single source of truth: `dambo-core/assets/dambo.js` (embedded at compile time and served at `GET /sdk/dambo.js`).
 *
 * Constraints:
 * - Zero external dependencies and zero desktop/Tauri bridge globals (standard Web APIs only).
 * - Compatible with both OBS Browser Source and Dambo desktop transparent overlay windows.
 *
 * @version 2.0.0
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
     * @property {string=} batch_combo_id 原生礼物连击批次 ID（非连击时省略）
     * @property {number=} combo_stay_time 礼物连击停留秒数（`0` 或省略表示非连击）
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
     * `Dambo.utils.formatGift` 返回的标准化礼物信息结构。
     * @typedef {Object} GiftFormattedInfo
     * @property {string} giftName 标准化礼物基础名称（自动剥离末尾 `xN` / `×N` 数量后缀）
     * @property {string} giftId 礼物 ID 字符串（无有效 ID 时为 `''`）
     * @property {string} batchComboId 原生连击批次 ID（无有效 ID 时为 `''`）
     * @property {'gold' | 'silver'} coinType 币种分类（`'gold'`=付费金瓜子, `'silver'`=免费银瓜子）
     * @property {boolean} isPaid 是否为付费礼物
     * @property {number} num 单次增量数量（未提供时为 `0`）
     * @property {number} comboNum 原生连击累计数量（未提供时为 `0`）
     * @property {number} totalCoin 单次总金瓜子数（未提供时为 `0`）
     * @property {number} comboTotalCoin 原生连击累计金瓜子数（未提供时为 `0`）
     * @property {number} unitPrice 礼物单价（未提供时为 `0`）
     * @property {number} priceSeeds 事件金额字段 `price`（金瓜子，`0` 表示免费）
     * @property {number} comboStayTimeMs 服务端连击停留窗口（毫秒，`0` 表示未提供）
     * @property {string} mergeKey 推荐礼物连击归并主键（优先 `batch:<batchComboId>`，否则 `combo:<user>|<gift>|<coinType>`）
     */

    /**
     * `Dambo.utils.formatInteract` 返回的标准化互动/进房语义结构。
     * @typedef {Object} InteractFormattedInfo
     * @property {string} kind 标准化事件类型（`enter` | `follow` | `share` | `special_follow` | `mutual_follow` | `guard_enter`）
     * @property {number} msgType 原始 `msg_type`（`1`~`5`，未知时为 `0`）
     * @property {number} guardLevel 大航海等级（`1`=总督, `2`=提督, `3`=舰长, `0`=非大航海）
     * @property {string} guardName 大航海等级名称（`'总督'` | `'提督'` | `'舰长'` | `''`）
     * @property {string} actionText 标准化中文动作短语（如 `'进入了直播间'` | `'关注了直播间'` | `'分享了直播间'`）
     * @property {string} summaryText 清洗后的完整展示文案（优先 `item.content`，否则回退到 `actionText`）
     */

    /**
     * 礼物连击追踪器中的单条活跃记录。
     * @typedef {Object} GiftComboEntry
     * @property {string} key 归并主键
     * @property {string} giftName 礼物基础名称
     * @property {string} giftId 礼物 ID
     * @property {'gold' | 'silver'} coinType 币种分类
     * @property {number} totalCount 累计送出总数量
     * @property {number} totalPriceSeeds 累计价值（金瓜子，`silver` 恒为 `0`）
     * @property {number} lastComboNum 最近一次观测到的 `combo_num`
     * @property {number} lastComboTotalCoin 最近一次观测到的 `combo_total_coin`
     * @property {number} lastEventTimeMs 最近一次命中事件时间戳（毫秒）
     * @property {*} data 插件自定义的附属状态（如 DOM 引用等）
     */

    /**
     * `Dambo.utils.createGiftComboTracker(options)` 配置项。
     * @typedef {Object} GiftComboTrackerOptions
     * @property {number=} windowMs 滑动连击合并窗口（毫秒），默认 `4000`
     * @property {boolean=} useServerComboStayTime 为 `true` 且事件包含 `combo_stay_time > 0` 时采用服务端连击停留窗口，默认 `false`
     * @property {((entry: GiftComboEntry, item: Object, opts: Object) => boolean)=} canMerge 自定义判断是否允许合并到当前条目
     * @property {((entry: GiftComboEntry, item: Object, opts: Object) => void)=} onCreate 新建连击条目回调
     * @property {((entry: GiftComboEntry, delta: {deltaCount: number, deltaPriceSeeds: number, meta: GiftFormattedInfo}, item: Object, opts: Object) => void)=} onUpdate 连击归并刷新回调
     * @property {((entry: GiftComboEntry) => void)=} onExpire 连击窗口过期或被显式清理时回调
     */

    /**
     * `Dambo.utils.createAutoScroll(containerEl, options)` 配置项。
     * @typedef {Object} AutoScrollOptions
     * @property {number=} thresholdPx 判定接近底部的像素阈值，默认 `24`
     * @property {boolean=} bindWindowResize 是否监听 `window.resize` 自动维持贴底，默认 `true`
     * @property {((following: boolean) => void)=} onStateChange 自动跟随状态切换回调
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
        return /^http:\/\//i.test(trimmed) ? 'https://' + trimmed.slice(7) : trimmed;
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
     * 标准化解析进房/关注/分享/特关/互粉/舰长进场等互动事件，统一提取 `msg_type`、`guard_level` 与兜底动作文案。
     * @param {SnapshotItem | EventItem | null | undefined} item
     * @returns {InteractFormattedInfo}
     */
    function formatInteract(item) {
        const obj = item && typeof item === 'object' ? item : {};
        const rawMsgType = Number(obj.msg_type || 0);
        const msgType = Number.isFinite(rawMsgType) && rawMsgType > 0 ? Math.floor(rawMsgType) : 0;
        let kind = typeof obj.kind === 'string' && obj.kind.trim() ? obj.kind.trim() : '';

        if (!kind) {
            switch (msgType) {
                case 1:
                    kind = 'enter';
                    break;
                case 2:
                    kind = 'follow';
                    break;
                case 3:
                    kind = 'share';
                    break;
                case 4:
                    kind = 'special_follow';
                    break;
                case 5:
                    kind = 'mutual_follow';
                    break;
                default:
                    kind = 'enter';
                    break;
            }
        }

        const rawGuard = Number(obj.guard_level || 0);
        const guardName = guardLevelName(rawGuard);
        const guardLevel = guardName ? Math.floor(rawGuard) : 0;

        let actionText;
        switch (kind) {
            case 'follow':
                actionText = '关注了直播间';
                break;
            case 'share':
                actionText = '分享了直播间';
                break;
            case 'special_follow':
                actionText = '特别关注了直播间';
                break;
            case 'mutual_follow':
                actionText = '互粉了直播间';
                break;
            case 'guard_enter':
                actionText = guardName ? '欢迎' + guardName + '进入直播间' : '进入了直播间';
                break;
            case 'enter':
            default:
                actionText = '进入了直播间';
                break;
        }

        const rawContent =
            typeof obj.content === 'string'
                ? obj.content.replace(/<%/g, '').replace(/%>/g, '').trim()
                : '';

        return {
            kind: kind,
            msgType: msgType,
            guardLevel: guardLevel,
            guardName: guardName,
            actionText: actionText,
            summaryText: rawContent || actionText,
        };
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
     * 标准化解析礼物事件字段，统一提取礼物基础名称、币种（免费/付费）、数量与连击归并主键。
     * @param {SnapshotItem | EventItem | Object | null | undefined} item
     * @returns {GiftFormattedInfo}
     */
    function formatGift(item) {
        var obj = item && typeof item === 'object' ? item : {};
        var rawBatch =
            typeof obj.batch_combo_id === 'string' && obj.batch_combo_id.trim()
                ? obj.batch_combo_id.trim()
                : typeof obj.combo_id === 'string' && obj.combo_id.trim()
                  ? obj.combo_id.trim()
                  : '';
        var batchComboId = rawBatch && rawBatch !== '0' ? rawBatch : '';

        var uidNum = Number(obj.uid || 0);
        var rawUname = String(obj.uname || obj.username || '').trim();
        var userKey =
            Number.isFinite(uidNum) && uidNum > 0
                ? 'uid:' + Math.floor(uidNum)
                : 'uname:' + (rawUname || 'viewer');

        var explicitName = obj.gift_name || obj.giftName;
        var giftName;
        if (typeof explicitName === 'string' && explicitName.trim()) {
            giftName = explicitName.trim();
        } else {
            var rawContent = String(obj.content || '').trim();
            if (!rawContent) {
                giftName = '礼物';
            } else {
                var stripped = rawContent.replace(/\s*[x×]\s*\d+\s*$/i, '').trim();
                giftName = stripped || rawContent;
            }
        }

        var rawGiftId =
            obj.gift_id !== undefined && obj.gift_id !== null
                ? String(obj.gift_id).trim()
                : obj.giftId !== undefined && obj.giftId !== null
                  ? String(obj.giftId).trim()
                  : '';
        var giftId = rawGiftId && rawGiftId !== '0' ? rawGiftId : '';
        var giftKey = giftId ? 'gid:' + giftId : 'gname:' + giftName;

        var rawCoin = typeof obj.coin_type === 'string' ? obj.coin_type.trim().toLowerCase() : '';
        var numRaw = Number(
            obj.num !== undefined ? obj.num : obj.gift_num !== undefined ? obj.gift_num : obj.count
        );
        var comboNumRaw = Number(obj.combo_num);
        var totalCoinRaw = Number(obj.total_coin);
        var comboTotalCoinRaw = Number(obj.combo_total_coin);
        var unitPriceRaw = Number(obj.unit_price);
        var priceRaw = Number(obj.price);

        var num = Number.isFinite(numRaw) && numRaw > 0 ? Math.floor(numRaw) : 0;
        var comboNum = Number.isFinite(comboNumRaw) && comboNumRaw > 0 ? Math.floor(comboNumRaw) : 0;
        var totalCoin = Number.isFinite(totalCoinRaw) && totalCoinRaw > 0 ? totalCoinRaw : 0;
        var comboTotalCoin =
            Number.isFinite(comboTotalCoinRaw) && comboTotalCoinRaw > 0 ? comboTotalCoinRaw : 0;
        var unitPrice = Number.isFinite(unitPriceRaw) && unitPriceRaw > 0 ? unitPriceRaw : 0;
        var priceSeeds = Number.isFinite(priceRaw) && priceRaw > 0 ? priceRaw : 0;
        var rawStaySec = Number(obj.combo_stay_time);
        var comboStayTimeMs =
            Number.isFinite(rawStaySec) && rawStaySec > 0 ? Math.round(rawStaySec * 1000) : 0;

        var coinType;
        if (rawCoin === 'silver' || rawCoin === 'free') {
            coinType = 'silver';
        } else if (rawCoin === 'gold' || rawCoin === 'paid') {
            coinType = 'gold';
        } else {
            coinType =
                comboTotalCoin > 0 || totalCoin > 0 || priceSeeds > 0 || unitPrice > 0
                    ? 'gold'
                    : 'silver';
        }

        var mergeKey = batchComboId
            ? 'batch:' + batchComboId
            : 'combo:' + userKey + '|' + giftKey + '|' + coinType;

        return {
            giftName: giftName,
            giftId: giftId,
            batchComboId: batchComboId,
            coinType: coinType,
            isPaid: coinType === 'gold',
            num: num,
            comboNum: comboNum,
            totalCoin: totalCoin,
            comboTotalCoin: comboTotalCoin,
            unitPrice: unitPrice,
            priceSeeds: priceSeeds,
            comboStayTimeMs: comboStayTimeMs,
            mergeKey: mergeKey,
        };
    }

    /**
     * 创建礼物连击滑动窗口追踪器，负责主键归并、增量/累计防重算与过期定时器清理。
     * @param {GiftComboTrackerOptions} [options]
     */
    function createGiftComboTracker(options) {
        var opts = options || {};
        var rawWin = opts.windowMs === undefined ? 4000 : Number(opts.windowMs);
        var windowMs = Number.isFinite(rawWin) && rawWin > 0 ? rawWin : 4000;
        var useServerComboStayTime = Boolean(opts.useServerComboStayTime);
        var canMerge = typeof opts.canMerge === 'function' ? opts.canMerge : null;
        var onCreate = typeof opts.onCreate === 'function' ? opts.onCreate : null;
        var onUpdate = typeof opts.onUpdate === 'function' ? opts.onUpdate : null;
        var onExpire = typeof opts.onExpire === 'function' ? opts.onExpire : null;

        /** @type {Map<string, any>} */
        var active = new Map();

        function resolveEntryWindowMs(meta) {
            if (useServerComboStayTime && meta && meta.comboStayTimeMs > 0) {
                return meta.comboStayTimeMs;
            }
            return windowMs;
        }

        function computeDelta(entry, meta) {
            var deltaCount;
            if (meta.comboNum > 0) {
                if (!entry) {
                    deltaCount = Math.max(meta.comboNum, meta.num || 0);
                } else if (meta.comboNum > entry.lastComboNum) {
                    deltaCount = meta.comboNum - entry.lastComboNum;
                } else {
                    deltaCount = meta.num || meta.comboNum;
                }
            } else {
                deltaCount = meta.num || 1;
            }

            var deltaPriceSeeds = 0;
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
                        deltaPriceSeeds = meta.priceSeeds;
                    }
                } else if (meta.totalCoin > 0) {
                    deltaPriceSeeds = meta.totalCoin;
                } else if (meta.unitPrice > 0) {
                    deltaPriceSeeds = meta.unitPrice * deltaCount;
                } else {
                    deltaPriceSeeds = meta.priceSeeds;
                }
            }

            return {
                deltaCount: deltaCount,
                deltaPriceSeeds: deltaPriceSeeds,
                meta: meta,
            };
        }

        function expireEntry(entry, notify) {
            if (!entry) return;
            if (entry._timerId !== null) {
                clearTimeout(entry._timerId);
                entry._timerId = null;
            }
            if (active.get(entry.key) === entry) {
                active.delete(entry.key);
                if (notify !== false && onExpire) {
                    onExpire(entry);
                }
            }
        }

        function scheduleExpiry(entry, eventTimeMs, isSnapshot) {
            if (entry._timerId !== null) {
                clearTimeout(entry._timerId);
                entry._timerId = null;
            }
            var entryWindowMs = entry._windowMs > 0 ? entry._windowMs : windowMs;
            var delayMs = entryWindowMs;
            if (isSnapshot) {
                delayMs = Math.min(entryWindowMs, entryWindowMs - (Date.now() - eventTimeMs));
                if (!Number.isFinite(delayMs) || delayMs <= 0) {
                    return;
                }
            }
            entry._timerId = setTimeout(function () {
                entry._timerId = null;
                expireEntry(entry, true);
            }, delayMs);
        }

        return {
            push: function (item, pushOptions) {
                if (!item || typeof item !== 'object') return null;
                var callOpts = pushOptions || {};
                var isSnapshot = Boolean(callOpts.isSnapshot);
                var meta = formatGift(item);
                var entryWindowMs = resolveEntryWindowMs(meta);
                var rawTs = Number(item.timestamp);
                var eventTimeMs = Number.isFinite(rawTs) && rawTs > 0 ? rawTs : Date.now();

                var existing = active.get(meta.mergeKey);
                if (existing) {
                    var activeWindowMs = existing._windowMs > 0 ? existing._windowMs : entryWindowMs;
                    var staleSnapshot = isSnapshot && eventTimeMs - existing.lastEventTimeMs > activeWindowMs;
                    var blockedByCaller = canMerge ? !canMerge(existing, item, callOpts) : false;
                    if (staleSnapshot || blockedByCaller) {
                        expireEntry(existing, true);
                        existing = null;
                    }
                }

                if (existing) {
                    if (item.id !== undefined && item.id !== null) {
                        if (existing._seenIds.has(item.id)) {
                            return existing;
                        }
                        if (existing._seenIds.size > 2000) {
                            existing._seenIds.clear();
                        }
                        existing._seenIds.add(item.id);
                    }

                    var delta = computeDelta(existing, meta);
                    existing.totalCount += delta.deltaCount;
                    existing.totalPriceSeeds += delta.deltaPriceSeeds;
                    existing.lastEventTimeMs = eventTimeMs;
                    existing._windowMs = entryWindowMs;
                    if (meta.comboNum > 0) {
                        existing.lastComboNum = meta.comboNum;
                    }
                    if (meta.comboTotalCoin > 0) {
                        existing.lastComboTotalCoin = meta.comboTotalCoin;
                    }

                    if (onUpdate) {
                        onUpdate(existing, delta, item, callOpts);
                    }
                    if (active.get(existing.key) === existing) {
                        scheduleExpiry(existing, eventTimeMs, isSnapshot);
                    }
                    return existing;
                }

                var initial = computeDelta(null, meta);
                var seenIds = new Set();
                if (item.id !== undefined && item.id !== null) {
                    seenIds.add(item.id);
                }

                var entry = {
                    key: meta.mergeKey,
                    giftName: meta.giftName,
                    giftId: meta.giftId,
                    coinType: meta.coinType,
                    totalCount: initial.deltaCount,
                    totalPriceSeeds: initial.deltaPriceSeeds,
                    lastComboNum: meta.comboNum,
                    lastComboTotalCoin: meta.comboTotalCoin,
                    lastEventTimeMs: eventTimeMs,
                    data: null,
                    _seenIds: seenIds,
                    _windowMs: entryWindowMs,
                    _timerId: null,
                };

                active.set(meta.mergeKey, entry);
                if (onCreate) {
                    onCreate(entry, item, callOpts);
                }
                if (active.get(entry.key) === entry) {
                    scheduleExpiry(entry, eventTimeMs, isSnapshot);
                }
                return entry;
            },
            expire: function (key, expectedEntry) {
                var target = active.get(key);
                if (!target) return false;
                if (expectedEntry && target !== expectedEntry) return false;
                expireEntry(target, true);
                return true;
            },
            pruneExpiredSnapshots: function () {
                Array.from(active.values()).forEach(function (entry) {
                    if (entry._timerId === null) {
                        expireEntry(entry, true);
                    }
                });
            },
            clear: function () {
                Array.from(active.values()).forEach(function (entry) {
                    if (entry._timerId !== null) {
                        clearTimeout(entry._timerId);
                        entry._timerId = null;
                    }
                });
                active.clear();
            },
            size: function () {
                return active.size;
            },
        };
    }

    /**
     * 为列表容器创建自动跟随置底控制器，支持 `rAF` 帧末置底与手动上滑阅读历史保护。
     * @param {HTMLElement | null | undefined} containerEl
     * @param {AutoScrollOptions} [options]
     */
    function createAutoScroll(containerEl, options) {
        var opts = options || {};
        var rawThreshold = opts.thresholdPx === undefined ? 24 : Number(opts.thresholdPx);
        var thresholdPx = Number.isFinite(rawThreshold) && rawThreshold >= 0 ? rawThreshold : 24;
        var bindWindowResize = opts.bindWindowResize !== false;
        var onStateChange = typeof opts.onStateChange === 'function' ? opts.onStateChange : null;

        var following = true;
        var pendingRaf = null;
        var unlockRaf = null;
        var programmaticLock = false;
        var smoothScrolling = false;

        function setFollowing(next) {
            var normalized = Boolean(next);
            if (following === normalized) return;
            following = normalized;
            if (onStateChange) {
                onStateChange(following);
            }
        }

        function isNearBottom() {
            if (!containerEl) return true;
            var maxScrollTop = containerEl.scrollHeight - containerEl.clientHeight;
            if (maxScrollTop <= 0) return true;
            return maxScrollTop - containerEl.scrollTop <= thresholdPx;
        }

        function cancelPending() {
            if (pendingRaf !== null) {
                cancelAnimationFrame(pendingRaf);
                pendingRaf = null;
            }
            if (unlockRaf !== null) {
                cancelAnimationFrame(unlockRaf);
                unlockRaf = null;
            }
            programmaticLock = false;
        }

        function schedule(forceOptions) {
            if (!containerEl) return;
            var forceOpts =
                typeof forceOptions === 'boolean'
                    ? { force: forceOptions }
                    : forceOptions || {};
            if (forceOpts.force) {
                setFollowing(true);
            }
            if (!following) return;

            if (forceOpts.smooth && typeof containerEl.scrollTo === 'function') {
                cancelPending();
                smoothScrolling = true;
                containerEl.scrollTo({ top: containerEl.scrollHeight, behavior: 'smooth' });
                return;
            }

            if (pendingRaf !== null) return;
            pendingRaf = requestAnimationFrame(function () {
                pendingRaf = null;
                if (!containerEl || !following) return;
                smoothScrolling = false;
                programmaticLock = true;
                containerEl.scrollTop = containerEl.scrollHeight;
                if (unlockRaf !== null) {
                    cancelAnimationFrame(unlockRaf);
                }
                unlockRaf = requestAnimationFrame(function () {
                    unlockRaf = null;
                    programmaticLock = false;
                });
            });
        }

        function onWheel(e) {
            if (e && e.deltaY < 0) {
                smoothScrolling = false;
                cancelPending();
                setFollowing(false);
            } else if (e && e.deltaY > 0 && isNearBottom()) {
                smoothScrolling = false;
                setFollowing(true);
            }
        }

        function onTouchStart() {
            smoothScrolling = false;
            if (pendingRaf !== null) {
                cancelPending();
            }
        }

        function onScroll() {
            if (programmaticLock || pendingRaf !== null) return;
            if (smoothScrolling) {
                if (isNearBottom()) {
                    smoothScrolling = false;
                    setFollowing(true);
                }
                return;
            }
            setFollowing(isNearBottom());
        }

        function onWindowResize() {
            if (following) {
                schedule();
            }
        }

        if (containerEl && typeof containerEl.addEventListener === 'function') {
            containerEl.addEventListener('wheel', onWheel, { passive: true });
            containerEl.addEventListener('touchstart', onTouchStart, { passive: true });
            containerEl.addEventListener('scroll', onScroll, { passive: true });
        }
        if (bindWindowResize && typeof window !== 'undefined' && typeof window.addEventListener === 'function') {
            window.addEventListener('resize', onWindowResize);
        }

        return {
            schedule: schedule,
            isFollowing: function () {
                return following;
            },
            isNearBottom: isNearBottom,
            reset: function () {
                smoothScrolling = false;
                cancelPending();
                setFollowing(true);
            },
            destroy: function () {
                smoothScrolling = false;
                cancelPending();
                if (containerEl && typeof containerEl.removeEventListener === 'function') {
                    containerEl.removeEventListener('wheel', onWheel);
                    containerEl.removeEventListener('touchstart', onTouchStart);
                    containerEl.removeEventListener('scroll', onScroll);
                }
                if (
                    bindWindowResize &&
                    typeof window !== 'undefined' &&
                    typeof window.removeEventListener === 'function'
                ) {
                    window.removeEventListener('resize', onWindowResize);
                }
            },
        };
    }

    /**
     * 创建受控定时器回收池，便于插件在重载/快照重置时一次性释放全部待执行定时器。
     */
    function createTimerBag() {
        var ids = new Set();
        return {
            set: function (fn, delayMs) {
                if (typeof fn !== 'function') return null;
                var id = setTimeout(function () {
                    ids.delete(id);
                    fn();
                }, delayMs);
                ids.add(id);
                return id;
            },
            clear: function (id) {
                if (id === null || id === undefined) return;
                clearTimeout(id);
                ids.delete(id);
            },
            clearAll: function () {
                ids.forEach(function (id) {
                    clearTimeout(id);
                });
                ids.clear();
            },
            size: function () {
                return ids.size;
            },
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

    /** @type {Map<string, string>} */
    const EMOTS_SOURCE_CACHE = new Map();

    function getEmotsRegex(keys) {
        const sorted = keys.slice().sort(function (a, b) {
            return b.length - a.length;
        });
        const cacheKey = sorted.join('\u0000');
        let source = EMOTS_SOURCE_CACHE.get(cacheKey);
        if (!source) {
            source = sorted.map(escapeRegExp).join('|');
            if (EMOTS_SOURCE_CACHE.size >= 64) {
                const oldestKey = EMOTS_SOURCE_CACHE.keys().next().value;
                if (oldestKey !== undefined) {
                    EMOTS_SOURCE_CACHE.delete(oldestKey);
                }
            }
            EMOTS_SOURCE_CACHE.set(cacheKey, source);
        }
        return new RegExp(source, 'g');
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

        const pattern = getEmotsRegex(keys);
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

            this._hooksBound = false;
            this._bindLifecycleHooks();
        }

        /** @private */
        _bindLifecycleHooks() {
            if (
                this._hooksBound ||
                typeof window === 'undefined' ||
                typeof window.addEventListener !== 'function'
            ) {
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
            this._hooksBound = true;
        }

        /** @private */
        _unbindLifecycleHooks() {
            if (
                !this._hooksBound ||
                typeof window === 'undefined' ||
                typeof window.removeEventListener !== 'function'
            ) {
                return;
            }
            if (this._onWindowOnline) {
                window.removeEventListener('online', this._onWindowOnline);
            }
            if (this._onWindowPageHide) {
                window.removeEventListener('pagehide', this._onWindowPageHide);
            }
            this._hooksBound = false;
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
            this._unbindLifecycleHooks();
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
            this._bindLifecycleHooks();
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

    // =========================================================================
    // Layer 2 平台 SDK（基于 Layer 1 /api/v1/* REST + 懒连接单一 SSE）
    // =========================================================================

    /**
     * 平台 REST / 业务错误统一异常类。
     */
    class DamboError extends Error {
        /**
         * @param {string} message
         * @param {{ code?: string, status?: number, details?: any }=} opts
         */
        constructor(message, opts) {
            super(message || 'Dambo API error');
            this.name = 'DamboError';
            this.code = (opts && opts.code) || 'INTERNAL_ERROR';
            this.status = (opts && typeof opts.status === 'number') ? opts.status : 0;
            this.details = opts ? opts.details : undefined;
        }
    }

    function parsePluginContext() {
        if (typeof window === 'undefined' || !window.location || !window.location.pathname) {
            return { pluginName: '', mode: 'view' };
        }
        const rawSegments = window.location.pathname.split('/');
        const segments = [];
        for (let i = 0; i < rawSegments.length; i++) {
            if (rawSegments[i]) {
                segments.push(rawSegments[i]);
            }
        }
        if (segments.length === 0) {
            let rootPlugin = 'default';
            if (typeof document !== 'undefined' && typeof document.querySelector === 'function') {
                const baseEl = document.querySelector('base[href]');
                const baseHref = baseEl ? baseEl.getAttribute('href') || '' : '';
                const baseParts = baseHref.split('/').filter(Boolean);
                if (baseParts.length > 0) {
                    let candidate = baseParts[0];
                    try {
                        candidate = decodeURIComponent(candidate);
                    } catch (_) {
                        // keep raw segment
                    }
                    const lowerCandidate = candidate.toLowerCase();
                    if (
                        lowerCandidate !== 'sdk' &&
                        lowerCandidate !== 'api' &&
                        lowerCandidate !== 'ws'
                    ) {
                        rootPlugin = candidate;
                    }
                }
            }
            return { pluginName: rootPlugin, mode: 'view' };
        }
        let first = segments[0];
        try {
            first = decodeURIComponent(first);
        } catch (_) {
            // keep raw segment
        }
        const lowerFirst = first.toLowerCase();
        if (lowerFirst === 'sdk' || lowerFirst === 'api' || lowerFirst === 'ws') {
            return { pluginName: '', mode: 'view' };
        }
        const mode = (segments.length >= 2 && segments[1] === 'm') ? 'manage' : 'view';
        return { pluginName: first, mode: mode };
    }

    let metaCache = null;
    let metaInflight = null;

    function apiFetch(method, path, body, opts) {
        if (typeof fetch !== 'function') {
            return Promise.reject(new DamboError('Fetch API unavailable in this environment', {
                code: 'UNSUPPORTED_ENVIRONMENT',
                status: 0,
            }));
        }
        const ctx = parsePluginContext();
        const headers = {};
        if (!(opts && opts.skipPluginHeader) && ctx.pluginName) {
            headers['X-Dambo-Plugin'] = ctx.pluginName;
        }
        const init = {
            method: method,
            credentials: 'same-origin',
            headers: headers,
        };
        if (body !== undefined) {
            headers['Content-Type'] = 'application/json';
            init.body = JSON.stringify(body);
        }

        return fetch(path, init).then(function (res) {
            return res.text().then(function (rawText) {
                let parsed = null;
                if (rawText) {
                    try {
                        parsed = JSON.parse(rawText);
                    } catch (_) {
                        parsed = null;
                    }
                }
                if (!res.ok) {
                    const errObj = (parsed && parsed.error) || {};
                    throw new DamboError(
                        errObj.message || ('HTTP ' + res.status + ' calling ' + path),
                        {
                            code: errObj.code || 'HTTP_ERROR',
                            status: res.status,
                            details: errObj.details,
                        }
                    );
                }
                return parsed;
            });
        });
    }

    const sseManager = (function () {
        let nextListenerId = 1;
        const watchListeners = [];
        const topicListeners = [];
        const aclListeners = [];
        const ownerMap = new Map();

        let selfNamedomain = null;
        let es = null;
        let clientId = null;
        let connectTimer = null;
        let syncTimer = null;
        let openSignature = '';
        let connectedSignature = '';

        function totalListeners() {
            return watchListeners.length + topicListeners.length + aclListeners.length;
        }

        function clearTimers() {
            if (connectTimer !== null) {
                clearTimeout(connectTimer);
                connectTimer = null;
            }
            if (syncTimer !== null) {
                clearTimeout(syncTimer);
                syncTimer = null;
            }
        }

        function closeEventSource() {
            clearTimers();
            if (es !== null) {
                try {
                    es.close();
                } catch (_) {}
                es = null;
            }
            clientId = null;
            selfNamedomain = null;
            openSignature = '';
            connectedSignature = '';
            ownerMap.clear();
        }

        function buildDesiredSubscriptions() {
            const watches = [];
            const watchSeen = Object.create(null);
            for (let i = 0; i < watchListeners.length; i++) {
                const w = watchListeners[i];
                const key = w.store + '\0' + w.owner + '\0' + w.namespace + '\0' + (w.prefix || '');
                if (!watchSeen[key]) {
                    watchSeen[key] = true;
                    const item = {
                        store: w.store,
                        owner: w.owner,
                        namespace: w.namespace,
                    };
                    if (w.prefix) {
                        item.prefix = w.prefix;
                    }
                    watches.push(item);
                }
            }

            const topics = [];
            const topicSeen = Object.create(null);
            for (let j = 0; j < topicListeners.length; j++) {
                const t = topicListeners[j];
                const key = t.owner + '\0' + t.topic;
                if (!topicSeen[key]) {
                    topicSeen[key] = true;
                    topics.push({
                        owner: t.owner,
                        topic: t.topic,
                    });
                }
            }

            return { watches: watches, topics: topics };
        }

        function signatureOf(subs) {
            return JSON.stringify(subs);
        }

        function buildEventsUrl(subs) {
            const ctx = parsePluginContext();
            const params = [
                'plugin=' + encodeURIComponent(ctx.pluginName),
                'mode=' + encodeURIComponent(ctx.mode),
            ];
            if (subs.watches.length > 0) {
                params.push('watches=' + encodeURIComponent(JSON.stringify(subs.watches)));
            }
            if (subs.topics.length > 0) {
                params.push('topics=' + encodeURIComponent(JSON.stringify(subs.topics)));
            }
            return '/api/v1/events?' + params.join('&');
        }

        function updateOwnerMapFromAccepted(accepted) {
            if (!accepted) return;
            if (Array.isArray(accepted.watches)) {
                for (let i = 0; i < accepted.watches.length; i++) {
                    const w = accepted.watches[i];
                    if (w && w.requested_owner && w.owner) {
                        ownerMap.set(w.requested_owner, w.owner);
                        ownerMap.set(w.owner, w.owner);
                    }
                }
            }
            if (Array.isArray(accepted.topics)) {
                for (let j = 0; j < accepted.topics.length; j++) {
                    const t = accepted.topics[j];
                    if (t && t.requested_owner && t.owner) {
                        ownerMap.set(t.requested_owner, t.owner);
                        ownerMap.set(t.owner, t.owner);
                    }
                }
            }
        }

        function ownerMatches(requestedOwner, eventOwner) {
            if (requestedOwner === '@self') {
                return Boolean(selfNamedomain && eventOwner === selfNamedomain);
            }
            if (requestedOwner === eventOwner) {
                return true;
            }
            const mapped = ownerMap.get(requestedOwner);
            return Boolean(mapped && mapped === eventOwner);
        }

        function flushSync() {
            syncTimer = null;
            if (!es || !clientId || totalListeners() === 0) return;
            const subs = buildDesiredSubscriptions();
            const sig = signatureOf(subs);
            if (sig === connectedSignature) return;
            const targetClient = clientId;
            apiFetch('PUT', '/api/v1/events/' + encodeURIComponent(targetClient) + '/subscriptions', subs)
                .then(function (res) {
                    if (clientId !== targetClient) return;
                    connectedSignature = sig;
                    if (res && res.accepted) {
                        updateOwnerMapFromAccepted(res.accepted);
                    }
                })
                .catch(function (err) {
                    console.warn('[dambo-sdk] failed to sync SSE subscriptions:', err);
                });
        }

        function scheduleSync() {
            if (syncTimer !== null) return;
            syncTimer = setTimeout(flushSync, 10);
        }

        function openConnectionNow() {
            connectTimer = null;
            if (totalListeners() === 0 || es !== null) return;
            if (typeof EventSource !== 'function') {
                console.error('[dambo-sdk] EventSource is not supported in this browser');
                return;
            }
            const subs = buildDesiredSubscriptions();
            openSignature = signatureOf(subs);
            connectedSignature = '';
            const url = buildEventsUrl(subs);
            const source = new EventSource(url);
            es = source;

            source.addEventListener('connected', function (ev) {
                if (es !== source) return;
                try {
                    const data = JSON.parse(ev.data);
                    clientId = data.client_id || null;
                    selfNamedomain = data.namedomain || null;
                    if (selfNamedomain) {
                        ownerMap.set('@self', selfNamedomain);
                        if (data.plugin_name) ownerMap.set(data.plugin_name, selfNamedomain);
                    }
                    updateOwnerMapFromAccepted(data.accepted);
                    connectedSignature = openSignature;
                    const desiredNow = signatureOf(buildDesiredSubscriptions());
                    if (desiredNow !== connectedSignature) {
                        scheduleSync();
                    }
                } catch (err) {
                    console.error('[dambo-sdk] invalid connected payload:', err);
                }
            });

            source.addEventListener('data.changed', function (ev) {
                if (es !== source) return;
                let data;
                try {
                    data = JSON.parse(ev.data);
                } catch (err) {
                    console.error('[dambo-sdk] invalid data.changed payload:', err);
                    return;
                }
                if (
                    selfNamedomain &&
                    data.owner === selfNamedomain &&
                    data.store === 'persist' &&
                    data.namespace === 'config'
                ) {
                    metaCache = null;
                }
                const listeners = watchListeners.slice();
                for (let i = 0; i < listeners.length; i++) {
                    const sub = listeners[i];
                    if (
                        sub.store !== data.store ||
                        sub.namespace !== data.namespace ||
                        !ownerMatches(sub.owner, data.owner)
                    ) {
                        continue;
                    }
                    let scoped = data;
                    if (sub.prefix && data.op !== 'clear') {
                        const rawKeys = Array.isArray(data.keys) ? data.keys : [];
                        const matchedKeys = [];
                        for (let kIdx = 0; kIdx < rawKeys.length; kIdx++) {
                            if (rawKeys[kIdx].indexOf(sub.prefix) === 0) {
                                matchedKeys.push(rawKeys[kIdx]);
                            }
                        }
                        if (matchedKeys.length === 0) {
                            continue;
                        }
                        if (matchedKeys.length !== rawKeys.length) {
                            let matchedValues;
                            if (data.values && typeof data.values === 'object') {
                                matchedValues = {};
                                for (let mIdx = 0; mIdx < matchedKeys.length; mIdx++) {
                                    const mk = matchedKeys[mIdx];
                                    if (Object.prototype.hasOwnProperty.call(data.values, mk)) {
                                        matchedValues[mk] = data.values[mk];
                                    }
                                }
                            }
                            scoped = {
                                store: data.store,
                                owner: data.owner,
                                namespace: data.namespace,
                                op: data.op,
                                keys: matchedKeys,
                                by: data.by,
                                ts: data.ts,
                            };
                            if (matchedValues !== undefined) {
                                scoped.values = matchedValues;
                            }
                        }
                    }
                    try {
                        sub.cb(scoped);
                    } catch (cbErr) {
                        console.error('[dambo-sdk] watch listener error:', cbErr);
                    }
                }
            });

            source.addEventListener('topic.message', function (ev) {
                if (es !== source) return;
                let msg;
                try {
                    msg = JSON.parse(ev.data);
                } catch (err) {
                    console.error('[dambo-sdk] invalid topic.message payload:', err);
                    return;
                }
                const listeners = topicListeners.slice();
                for (let i = 0; i < listeners.length; i++) {
                    const sub = listeners[i];
                    if (sub.topic === msg.topic && ownerMatches(sub.owner, msg.owner)) {
                        try {
                            sub.cb(msg);
                        } catch (cbErr) {
                            console.error('[dambo-sdk] topic listener error:', cbErr);
                        }
                    }
                }
            });

            source.addEventListener('acl.changed', function (ev) {
                if (es !== source) return;
                let payload;
                try {
                    payload = JSON.parse(ev.data);
                } catch (err) {
                    console.error('[dambo-sdk] invalid acl.changed payload:', err);
                    return;
                }
                const listeners = aclListeners.slice();
                for (let i = 0; i < listeners.length; i++) {
                    try {
                        listeners[i].cb(payload);
                    } catch (cbErr) {
                        console.error('[dambo-sdk] acl listener error:', cbErr);
                    }
                }
            });

            source.onerror = function () {
                if (es !== source) return;
                const desiredNow = signatureOf(buildDesiredSubscriptions());
                if (source.readyState === 2 /* CLOSED */ || desiredNow !== openSignature) {
                    try {
                        source.close();
                    } catch (_) {}
                    es = null;
                    clientId = null;
                    if (totalListeners() > 0) {
                        scheduleConnect(source.readyState === 2 ? 1000 : 200);
                    }
                }
            };
        }

        function scheduleConnect(delayMs) {
            if (connectTimer !== null || es !== null || totalListeners() === 0) return;
            connectTimer = setTimeout(openConnectionNow, typeof delayMs === 'number' ? delayMs : 10);
        }

        function ensureConnectedAfterAdd(needsServerSync) {
            if (es === null) {
                scheduleConnect(10);
            } else if (clientId !== null && needsServerSync) {
                scheduleSync();
            }
        }

        function cleanupOrSyncAfterRemove(needsServerSync) {
            if (totalListeners() === 0) {
                closeEventSource();
            } else if (es !== null && clientId !== null && needsServerSync) {
                scheduleSync();
            }
        }

        return {
            addWatch: function (store, owner, namespace, prefix, cb) {
                if (typeof cb !== 'function') {
                    throw new TypeError('Dambo watch callback must be a function');
                }
                const id = nextListenerId++;
                watchListeners.push({
                    id: id,
                    store: store,
                    owner: owner,
                    namespace: namespace,
                    prefix: prefix || '',
                    cb: cb,
                });
                ensureConnectedAfterAdd(true);
                let removed = false;
                return function off() {
                    if (removed) return;
                    removed = true;
                    for (let i = 0; i < watchListeners.length; i++) {
                        if (watchListeners[i].id === id) {
                            watchListeners.splice(i, 1);
                            break;
                        }
                    }
                    cleanupOrSyncAfterRemove(true);
                };
            },
            addTopic: function (owner, topic, cb) {
                if (typeof cb !== 'function') {
                    throw new TypeError('Dambo topic subscribe callback must be a function');
                }
                const id = nextListenerId++;
                topicListeners.push({
                    id: id,
                    owner: owner,
                    topic: topic,
                    cb: cb,
                });
                ensureConnectedAfterAdd(true);
                let removed = false;
                return function off() {
                    if (removed) return;
                    removed = true;
                    for (let i = 0; i < topicListeners.length; i++) {
                        if (topicListeners[i].id === id) {
                            topicListeners.splice(i, 1);
                            break;
                        }
                    }
                    cleanupOrSyncAfterRemove(true);
                };
            },
            addAcl: function (cb) {
                if (typeof cb !== 'function') {
                    throw new TypeError('Dambo acl.onChange callback must be a function');
                }
                const id = nextListenerId++;
                aclListeners.push({
                    id: id,
                    cb: cb,
                });
                ensureConnectedAfterAdd(false);
                let removed = false;
                return function off() {
                    if (removed) return;
                    removed = true;
                    for (let i = 0; i < aclListeners.length; i++) {
                        if (aclListeners[i].id === id) {
                            aclListeners.splice(i, 1);
                            break;
                        }
                    }
                    cleanupOrSyncAfterRemove(false);
                };
            },
        };
    })();

    function createStoreHandle(storeName, ownerTarget) {
        const target = String(ownerTarget || '@self');
        const base =
            '/api/v1/data/' + encodeURIComponent(storeName) + '/' + encodeURIComponent(target);

        return {
            namespaces: function () {
                return apiFetch('GET', base + '/namespaces').then(function (d) {
                    return d && Array.isArray(d.namespaces) ? d.namespaces : [];
                });
            },
            get: function (ns, key) {
                const url =
                    base + '/ns/' + encodeURIComponent(ns) + '/keys/' + encodeURIComponent(key);
                return apiFetch('GET', url).then(
                    function (d) {
                        return d ? d.value : undefined;
                    },
                    function (err) {
                        if (err instanceof DamboError && err.code === 'KEY_NOT_FOUND') {
                            return undefined;
                        }
                        throw err;
                    }
                );
            },
            getEntry: function (ns, key) {
                const url =
                    base + '/ns/' + encodeURIComponent(ns) + '/keys/' + encodeURIComponent(key);
                return apiFetch('GET', url).catch(function (err) {
                    if (err instanceof DamboError && err.code === 'KEY_NOT_FOUND') {
                        return null;
                    }
                    throw err;
                });
            },
            set: function (ns, key, value, schema) {
                const body = { value: value };
                if (schema && typeof schema === 'object') {
                    if (schema.type !== undefined) body.type = schema.type;
                    if (schema.meta !== undefined) body.meta = schema.meta;
                }
                const url =
                    base + '/ns/' + encodeURIComponent(ns) + '/keys/' + encodeURIComponent(key);
                return apiFetch('PUT', url, body).then(function (res) {
                    if (storeName === 'persist' && ns === 'config' && target === '@self') {
                        metaCache = null;
                    }
                    return res;
                });
            },
            delete: function (ns, key) {
                const url =
                    base + '/ns/' + encodeURIComponent(ns) + '/keys/' + encodeURIComponent(key);
                return apiFetch('DELETE', url).then(function (d) {
                    if (storeName === 'persist' && ns === 'config' && target === '@self') {
                        metaCache = null;
                    }
                    return Boolean(d && d.deleted);
                });
            },
            list: function (ns, opts) {
                const o = opts || {};
                const params = [];
                if (typeof o.prefix === 'string' && o.prefix.length > 0) {
                    params.push('prefix=' + encodeURIComponent(o.prefix));
                }
                if (Array.isArray(o.keys) && o.keys.length > 0) {
                    params.push('keys=' + encodeURIComponent(o.keys.join(',')));
                }
                if (typeof o.limit === 'number' && o.limit >= 0) {
                    params.push('limit=' + encodeURIComponent(String(Math.floor(o.limit))));
                }
                const qs = params.length > 0 ? '?' + params.join('&') : '';
                const url = base + '/ns/' + encodeURIComponent(ns) + qs;
                return apiFetch('GET', url).then(function (d) {
                    const entries = d && d.entries && typeof d.entries === 'object' ? d.entries : {};
                    if (o.withSchema) {
                        return entries;
                    }
                    const unwrapped = {};
                    const ks = Object.keys(entries);
                    for (let i = 0; i < ks.length; i++) {
                        unwrapped[ks[i]] = entries[ks[i]].value;
                    }
                    return unwrapped;
                });
            },
            mset: function (ns, map) {
                const rawMap = map && typeof map === 'object' ? map : {};
                const entries = {};
                const ks = Object.keys(rawMap);
                for (let i = 0; i < ks.length; i++) {
                    entries[ks[i]] = { value: rawMap[ks[i]] };
                }
                const url = base + '/ns/' + encodeURIComponent(ns) + '/mset';
                return apiFetch('POST', url, { entries: entries }).then(function (d) {
                    if (storeName === 'persist' && ns === 'config' && target === '@self') {
                        metaCache = null;
                    }
                    return d && d.entries ? d.entries : {};
                });
            },
            clearNamespace: function (ns) {
                const url = base + '/ns/' + encodeURIComponent(ns);
                return apiFetch('DELETE', url).then(function (d) {
                    if (storeName === 'persist' && ns === 'config' && target === '@self') {
                        metaCache = null;
                    }
                    return d && typeof d.deleted === 'number' ? d.deleted : 0;
                });
            },
            watch: function (ns, cb, opts) {
                const prefix =
                    opts && typeof opts.prefix === 'string' && opts.prefix.length > 0
                        ? opts.prefix
                        : undefined;
                return sseManager.addWatch(storeName, target, ns, prefix, cb);
            },
        };
    }

    const pluginApi = {
        get name() {
            return parsePluginContext().pluginName;
        },
        get mode() {
            return parsePluginContext().mode;
        },
        meta: function (opts) {
            if (!(opts && opts.force)) {
                if (metaCache !== null) {
                    return Promise.resolve(metaCache);
                }
                if (metaInflight !== null) {
                    return metaInflight;
                }
            }
            const p = apiFetch('GET', '/api/v1/plugins/' + encodeURIComponent('@self')).then(
                function (meta) {
                    metaCache = meta;
                    metaInflight = null;
                    return meta;
                },
                function (err) {
                    metaInflight = null;
                    throw err;
                }
            );
            metaInflight = p;
            return p;
        },
        unlockManage: function (token) {
            return apiFetch(
                'POST',
                '/api/v1/auth/manage',
                { token: token == null ? '' : String(token) },
                { skipPluginHeader: true }
            );
        },
    };

    const pluginsApi = {
        list: function () {
            return apiFetch('GET', '/api/v1/plugins').then(function (d) {
                return d && Array.isArray(d.plugins) ? d.plugins : [];
            });
        },
        get: function (target) {
            return apiFetch('GET', '/api/v1/plugins/' + encodeURIComponent(String(target)));
        },
    };

    const storageSelf = createStoreHandle('persist', '@self');
    const memorySelf = createStoreHandle('memory', '@self');

    const storageApi = Object.assign({}, storageSelf, {
        of: function (target) {
            return createStoreHandle('persist', target);
        },
    });

    const memoryApi = Object.assign({}, memorySelf, {
        of: function (target) {
            return createStoreHandle('memory', target);
        },
    });

    const configApi = {
        get: function (key) {
            return pluginApi.meta().then(function (meta) {
                const cfg = (meta && meta.config) || {};
                if (key !== undefined && key !== null) {
                    const k = String(key);
                    return Object.prototype.hasOwnProperty.call(cfg, k) ? cfg[k].value : undefined;
                }
                const out = {};
                const ks = Object.keys(cfg);
                for (let i = 0; i < ks.length; i++) {
                    out[ks[i]] = cfg[ks[i]].value;
                }
                return out;
            });
        },
        set: function (keyOrMap, val) {
            if (keyOrMap && typeof keyOrMap === 'object' && !Array.isArray(keyOrMap)) {
                return storageSelf.mset('config', keyOrMap).then(function (res) {
                    metaCache = null;
                    return res;
                });
            }
            return storageSelf.set('config', String(keyOrMap), val).then(function (res) {
                metaCache = null;
                return res;
            });
        },
        reset: function (key) {
            if (key !== undefined && key !== null) {
                return storageSelf.delete('config', String(key)).then(function (res) {
                    metaCache = null;
                    return res;
                });
            }
            return storageSelf.clearNamespace('config').then(function (res) {
                metaCache = null;
                return res;
            });
        },
        schema: function () {
            return pluginApi.meta().then(function (meta) {
                return (meta && meta.config) || {};
            });
        },
        watch: function (cb, opts) {
            return storageSelf.watch(
                'config',
                function (ev) {
                    metaCache = null;
                    cb(ev);
                },
                opts
            );
        },
    };

    const topicsApi = {
        publish: function (name, payload, opts) {
            const o = opts || {};
            const body = { payload: payload };
            if (o.public !== undefined) body.public = Boolean(o.public);
            if (o.description !== undefined) body.description = o.description;
            return apiFetch('POST', '/api/v1/topics/' + encodeURIComponent(String(name)), body).then(
                function (res) {
                    metaCache = null;
                    return res;
                }
            );
        },
        configure: function (name, opts) {
            const o = opts || {};
            const body = { public: Boolean(o.public) };
            if (o.description !== undefined) body.description = o.description;
            return apiFetch('PUT', '/api/v1/topics/' + encodeURIComponent(String(name)), body).then(
                function (res) {
                    metaCache = null;
                    return res;
                }
            );
        },
        remove: function (name) {
            return apiFetch('DELETE', '/api/v1/topics/' + encodeURIComponent(String(name))).then(
                function (d) {
                    metaCache = null;
                    return Boolean(d && d.deleted);
                }
            );
        },
        list: function (target) {
            const p =
                !target || target === '@self' ? pluginApi.meta() : pluginsApi.get(target);
            return p.then(function (meta) {
                return meta && Array.isArray(meta.topics) ? meta.topics : [];
            });
        },
        subscribe: function (target, name, cb) {
            return sseManager.addTopic(String(target), String(name), cb);
        },
    };

    const aclApi = {
        check: function (target, resource) {
            const qs =
                '?target=' +
                encodeURIComponent(String(target)) +
                '&resource=' +
                encodeURIComponent(String(resource));
            return apiFetch('GET', '/api/v1/acl' + qs);
        },
        request: function (target, opts) {
            const o = opts || {};
            const body = {
                target: String(target),
                resource: o.resource,
                level: o.level,
            };
            if (o.reason !== undefined) body.reason = o.reason;
            return apiFetch('POST', '/api/v1/acl/requests', body);
        },
        list: function () {
            return apiFetch('GET', '/api/v1/acl');
        },
        set: function (requester, opts) {
            const o = opts || {};
            const body = {
                resource: o.resource,
                level: o.level,
            };
            if (o.note !== undefined) body.note = o.note;
            return apiFetch('PUT', '/api/v1/acl/grants/' + encodeURIComponent(String(requester)), body);
        },
        remove: function (requester, resource) {
            const qs = '?resource=' + encodeURIComponent(String(resource));
            return apiFetch(
                'DELETE',
                '/api/v1/acl/grants/' + encodeURIComponent(String(requester)) + qs
            ).then(function (d) {
                return Boolean(d && d.deleted);
            });
        },
        onChange: function (cb) {
            return sseManager.addAcl(cb);
        },
    };

    return {
        VERSION: '2.0.0',
        DamboClient: DamboClient,
        DamboError: DamboError,
        connect: connect,
        plugin: pluginApi,
        plugins: pluginsApi,
        config: configApi,
        storage: storageApi,
        memory: memoryApi,
        topics: topicsApi,
        acl: aclApi,
        utils: {
            DEFAULT_AVATAR_SVG: DEFAULT_AVATAR_SVG,
            normalizeUrl: normalizeUrl,
            isSuperChat: isSuperChat,
            isPaidEvent: isPaidEvent,
            formatPrice: formatPrice,
            formatGift: formatGift,
            guardLevelName: guardLevelName,
            formatGuard: formatGuard,
            formatInteract: formatInteract,
            superChatDurationMs: superChatDurationMs,
            remainingDurationMs: remainingDurationMs,
            createAvatar: createAvatar,
            createGiftComboTracker: createGiftComboTracker,
            createAutoScroll: createAutoScroll,
            createTimerBag: createTimerBag,
            formatRoomBadge: formatRoomBadge,
            renderContent: renderContent,
            getQueryParam: getQueryParam,
            getQueryBool: getQueryBool,
        },
    };
});
