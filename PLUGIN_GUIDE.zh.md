# Dambo 插件架构与开发规范（`PLUGIN_GUIDE.zh.md`）

> **目标读者**：AI 编程助手（Claude Code / Cursor / Gemini CLI）。
> **核心目标**：仅凭本文档（无需查阅其他资料），即可零报错生成、校验并交付可用的 Dambo Web / OBS / 桌面悬浮窗插件。

---

## 1. 硬性约束与架构边界

1. **纯浏览器、无构建（No-Build）运行环境**：`static/html/<plugin-id>/` 下的所有插件均直接运行在标准浏览器环境中（现代浏览器、OBS 浏览器源以及 Dambo 桌面悬浮窗），**无任何构建步骤**。仅允许使用标准浏览器 Web API 与 `/sdk/dambo.js`。**严禁**使用 Node.js API、CommonJS `require()`、裸 `npm` 模块导入或任何非标准宿主全局对象。
2. **同步加载 SDK**：`/sdk/dambo.js` **必须**在插件脚本之前通过普通同步 `<script src="/sdk/dambo.js"></script>` 标签加载。**严禁**为 `/sdk/dambo.js` 添加 `defer`、`async` 或 `type="module"` 属性。
3. **零硬编码主机名与端口**：**严禁**在插件 HTML、CSS 或 JS 中硬编码任何主机名、IP 地址或端口号：
   - 统一使用 `/sdk/dambo.js` 引用 SDK，在 `<head>` 中加入 `<base href="/{plugin-id}/">`，并通过相对路径引用插件资源（`./plugin.css`、`./plugin.js`，以及可选放入 `./assets/` 子目录的图片、字体、音频等静态文件），确保无论访问 `GET /{plugin}`（无尾斜杠）还是 `GET /{plugin}/`（带尾斜杠）都能正确加载资源。
   - 调用 `Dambo.connect()` 时省略 `url`，由 SDK 基于 `window.location` 自动推导 WebSocket 地址（或支持通过 `url: Dambo.utils.getQueryParam('ws') || undefined` 按查询参数覆盖）。
   - 所有服务端监听地址与端口配置均统一位于宿主配置文件 `etc/config.toml` 的 `[server]` `host` 与 `port` 项。
4. **必需 `<meta name="referrer" content="no-referrer">`**：B 站头像（`face`）与表情（`emoticon`、`emots`）CDN 会校验并拒绝携带 `Referer` 的请求。每个插件 `index.html` 的 `<head>` 中**必须**包含 `<meta name="referrer" content="no-referrer">`。
5. **XSS 防护与 DOM 性能**：**严禁**将任何用户可控字段（`uname`、`content`、`room_uname`、`room_title`、`medal_name`）写入 `.innerHTML`。必须使用 `.textContent`、`document.createTextNode` 或 `Dambo.utils.renderContent(el, item, opts)`。高频弹幕流中优先使用 `.textContent` 而非 `.innerText`，避免触发布局重排。
6. **桌面悬浮窗控制栏安全区**：在桌面悬浮窗模式（`plugin-overlay-*`）下，宿主会在右上角（`top: 8px; right: 8px`）渲染悬浮控制条（`#dambo-overlay-host`，`z-index: 2147483647`），包含拖动、置顶图钉与关闭按钮（`26px` 按钮 + `4px` 间距 + `4px` 内边距）。请避免在**右上角 `100px × 44px`** 区域放置核心交互控件或不可裁切的关键文字。
7. **默认透明背景**：用于 OBS 叠加或桌面悬浮的挂件，请保持 `html, body { background: transparent !important; overflow: hidden; }`。

---

## 2. 插件目录结构与宿主自动发现契约

Dambo 会动态扫描 `static/html/`（可在 `etc/config.toml` 中通过 `[server].html_dir` 配置）并监听变更热重载（约 200ms 防抖）。新插件无需修改任何宿主文件，即可自动通过配置好的 `[server]` `host` 与 `port` 提供 HTTP 服务，并同步注册到桌面面板和系统托盘菜单中。

### 2.1 目录结构

```text
static/html/<plugin-id>/
├── index.html      # [必填] 必须是常规文件；用于触发自动发现
├── plugin.json     # [推荐] 元数据及 OBS / 桌面悬浮窗默认尺寸
├── plugin.css      # [可选] 插件样式表
├── plugin.js       # [可选] 基于 /sdk/dambo.js 的插件逻辑
└── assets/         # [可选] 静态资源目录（图片、字体、音频等），通过 ./assets/<file> 引用
```

### 2.2 自动发现、路由与清单规则

- **合法 `<plugin-id>`**：必须匹配 `^[A-Za-z0-9_-]+$`，且**不能**与保留顶层路由冲突（不区分大小写）：`ws`、`api`、`sdk`、`static`、`assets`、`health`。
- **`plugin.json` 结构**：
  ```json
  {
    "title": "插件显示名称",
    "description": "面板中展示的简短说明",
    "obs": {
      "width": 420,
      "height": 720
    }
  }
  ```
  - `obs.width` 与 `obs.height` 仅在 `> 0` 时生效；否则桌面悬浮窗默认使用 `420 × 720`。
  - 缺失或格式错误的 `plugin.json` 不会阻塞插件加载——宿主会记录警告日志并自动回退为目录名与默认尺寸。
- **HTTP 服务行为**：
  - 托管路由：`GET /{plugin}`、`GET /{plugin}/`、`GET /{plugin}/{*path}`、`GET /sdk/dambo.js`、`GET /ws`、`GET /ws/event`。
  - 所有 HTTP 响应均附带 `Cache-Control: no-store, no-cache, must-revalidate` 与 `Pragma: no-cache`。
  - 路径穿越（`..`、`\`、`\0`）统一返回 `404 Asset not found`。

---

## 3. WebSocket 通道与 SDK 契约（`/sdk/dambo.js`）

### 3.1 通道 A（`channel: 'feed'`，`/ws`）与 通道 B（`channel: 'event'`，`/ws/event`）

- **通道 A（`/ws` — 实时视觉叠加流）**：
  - 建连后立即推送一帧 `Snapshot`（当客户端消费落后于广播缓冲时也会自动重发 `Snapshot`）。
  - 绝不会下发 `"combo_end"`、`"room_info"` 或 `"session_end"`。
  - `Snapshot.items` 环形缓冲区仅保留 `"chat" | "gift" | "superchat" | "super_chat" | "guard"`。
  - 当 `[feed].show_enter == false` 时省略 `enter` 事件；当 `[feed].show_medal == false` 时省略 `medal_name` / `medal_level`。
- **通道 B（`/ws/event` — 整形聚合 / 连击结算批次流）**：
  - 建连后立即推送 `RoomInfo`，随后持续推送 `EventBatch` 帧（`priority: 0` = 蓄水池周期批，`1` = 直通信号 / 优质弹幕 / 礼物 / 连击结算，`2` = 关键状态 / SC / 大航海）。
  - 仅付费事件（`"superchat" | "super_chat" | "guard"`，或 `price > 0` 的 `"gift"`）保留 `face` 头像 URL。

### 3.2 类型定义（JSDoc）

> **事件类型判断辅助函数**：判断消息类型时，请优先调用 `Dambo.utils.isSuperChat(item)`（同时兼容 `'superchat'` 与 `'super_chat'`）和 `Dambo.utils.isPaidEvent(item)`（匹配醒目留言、大航海以及 `price > 0` 的付费礼物），避免手写字符串硬编码比对。

```javascript
/**
 * @typedef {Object} RoomStateSnapshot
 * @property {number} room_id
 * @property {string} room_uname
 * @property {string} room_title
 * @property {boolean} is_live
 * @property {boolean} connected
 */

/**
 * @typedef {Object} SnapshotItem
 * @property {number} id
 * @property {number} seq
 * @property {string} kind `'chat'` | `'gift'` | `'superchat'` | `'super_chat'` | `'guard'` | `'guard_enter'` | `'enter'` | `'follow'` | `'special_follow'` | `'mutual_follow'` | `'share'` | `'like'` | `'live'` | `'preparing'` | `'room_change'` | `'room_silent'` | `'room_block'` | `'superchat_delete'` | `'warning'` | `'cut_off'` | `'reconnected'`
 * @property {number=} msg_type
 * @property {number} uid
 * @property {string} uname
 * @property {string=} face
 * @property {string=} medal_name
 * @property {number=} medal_level
 * @property {string} content
 * @property {string=} emoticon 弹幕大表情图片 URL（当消息为表情贴图时存在）
 * @property {Object.<string, string>=} emots 行内文字表情映射表，键为带方括号的表情占位符（例如 `{"[dog]": "https://..."}`）
 * @property {number} price 金瓜子数额（`1000` = `¥1` 人民币；免费礼物/非付费事件为 `0`）
 * @property {number=} num 礼物数量或大航海开通月数
 * @property {number=} guard_level `1`=总督, `2`=提督, `3`=舰长
 * @property {number[]=} ids 被撤回的 SuperChat ID 列表（仅 `kind === 'superchat_delete'` 时存在）
 * @property {string=} batch_combo_id 原生礼物连击批次 ID（非连击时省略）
 * @property {number=} combo_stay_time 原生礼物连击停留秒数（`0` 或省略表示非连击）
 * @property {number} timestamp 13 位 Unix 毫秒时间戳
 */

/**
 * @typedef {Object} SnapshotData
 * @property {number} room_id
 * @property {string} room_uname
 * @property {string} room_title
 * @property {boolean} is_live
 * @property {boolean} connected
 * @property {SnapshotItem[]} items
 */

/**
 * @typedef {{type: 'Snapshot', data: SnapshotData} | {type: 'Event', data: SnapshotItem} | {type: 'RoomState', data: RoomStateSnapshot}} ChannelAMessage
 */

/**
 * @typedef {Object} RoomInfoEvent
 * @property {number} seq 固定为 `0`
 * @property {number} offset_ms 固定为 `0`
 * @property {number} timestamp
 * @property {number} room_id
 * @property {string} room_uname
 * @property {string} kind 固定为 `"room_info"`
 * @property {number} uid
 * @property {string} uname
 * @property {string} content
 * @property {number=} short_id
 * @property {string=} title
 * @property {string=} area_name
 * @property {string=} parent_area_name
 * @property {number=} live_status `1` = 直播中, `0` = 未开播, `2` = 轮播中
 * @property {number=} start_time Unix 秒级时间戳（未开播时为 `undefined`）
 * @property {string} raw_cmd 固定为 `"ROOM_INFO"`
 */

/**
 * @typedef {Object} EventItem
 * @property {number} seq
 * @property {string} kind
 * @property {number=} msg_type
 * @property {number} uid
 * @property {string} uname
 * @property {string=} face 仅付费事件保留（`Dambo.utils.isPaidEvent(item)`）
 * @property {number} medal_level
 * @property {string} content
 * @property {number} count 聚合计数 / 连击结算总数 / 单条为 `1`
 * @property {string[]} sample_users 聚合桶内最多 5 个去重代表昵称
 * @property {number} price 金瓜子数额（`1000` = `¥1` 人民币）
 * @property {number=} guard_level
 * @property {string=} batch_combo_id
 * @property {number[]=} ids
 */

/**
 * @typedef {Object} EventBatchData
 * @property {number} batch_id
 * @property {number} room_id
 * @property {string} room_uname
 * @property {0|1|2} priority
 * @property {number} timestamp
 * @property {EventItem[]} events
 */

/**
 * @typedef {{type: 'RoomInfo', data: RoomInfoEvent} | {type: 'EventBatch', data: EventBatchData}} ChannelBMessage
 */

/**
 * @typedef {'connecting' | 'open' | 'reconnecting' | 'closed'} ConnectionState
 */

/**
 * @typedef {Object} EventMeta
 * @property {boolean} isSnapshot
 * @property {EventBatchData=} batch 仅在 `channel: 'event'` 模式下附带
 */

/**
 * @typedef {Object} ConnectOptions
 * @property {'feed' | 'event'=} channel 默认 `'feed'`
 * @property {string=} url 省略时自动基于 `window.location` 推导（或传 `Dambo.utils.getQueryParam('ws') || undefined`）
 * @property {boolean=} autoReconnect 默认 `true`
 * @property {number=} reconnectDelayMs 默认 `2000`（下限钳制 `200`）
 * @property {number=} maxReconnectDelayMs 默认 `10000`
 * @property {number=} connectTimeoutMs 默认 `8000`（`0` 表示禁用建连超时看门狗）
 * @property {boolean=} replaySnapshot 默认 `false`
 * @property {boolean|null=} showLike 默认 `null`（设为 `false` 时自动屏蔽 `kind === 'like'`）
 * @property {((snapshot: SnapshotData) => void)=} onReset 收到 `'Snapshot'` 时在重放历史前触发
 */

/**
 * @typedef {Object} RenderContentOptions
 * @property {string=} stickerClass 默认 `'dambo-sticker'`
 * @property {string=} emotClass 默认 `'dambo-emot'`
 */

/**
 * @typedef {Object} GuardFormattedInfo
 * @property {number} guardLevel `1`=总督, `2`=提督, `3`=舰长, `0`=未知
 * @property {string} guardName `'总督'` | `'提督'` | `'舰长'` | `'大航海'`
 * @property {string} badgeLabel `'总督'` | `'提督'` | `'舰长'` | `'上舰'`
 * @property {number} num
 * @property {string} monthsSuffix 当 `num > 1` 时为 `' ×N个月'`，否则为 `''`
 * @property {string} actionText 例如 `'开通了'` | `'自动续费了'` | `'续费了'` | `''`
 * @property {string} highlightText 例如 `'舰长'` | `'3个月提督'` | `'舰长 ×2个月'`
 * @property {string} summaryText `actionText + highlightText`
 */

/**
 * @typedef {Object} InteractFormattedInfo
 * @property {string} kind `'enter'` | `'follow'` | `'share'` | `'special_follow'` | `'mutual_follow'` | `'guard_enter'`
 * @property {number} msgType 原始 `msg_type`（`1`~`5`，未知时为 `0`）
 * @property {number} guardLevel `1`=总督, `2`=提督, `3`=舰长, `0`=非大航海
 * @property {string} guardName `'总督'` | `'提督'` | `'舰长'` | `''`
 * @property {string} actionText 标准化中文动作短语（如 `'进入了直播间'`、`'关注了直播间'`）
 * @property {string} summaryText 清洗后的展示文本或兜底 `actionText`
 */

/**
 * @typedef {Object} RemainingDurationOptions
 * @property {boolean=} isSnapshot 默认 `true`（`false` 时直接返回 `totalMs`）
 * @property {number=} minMs 默认 `0`
 * @property {number=} now 默认 `Date.now()`
 */

/**
 * @typedef {Object} GiftFormattedInfo
 * @property {string} giftName 标准化礼物基础名称（自动剥离末尾 `xN` / `×N` 后缀，兜底 `'礼物'`）
 * @property {string} giftId 礼物 ID 字符串（无有效 ID 时为 `''`）
 * @property {string} batchComboId 原生连击批次 ID（无有效 ID 时为 `''`）
 * @property {'gold' | 'silver'} coinType 币种分类（`'gold'`=付费金瓜子, `'silver'`=免费银瓜子）
 * @property {boolean} isPaid 是否为付费礼物（`coinType === 'gold'`）
 * @property {number} num 单次增量数量（缺省为 `0`）
 * @property {number} comboNum 原生连击累计数量（缺省为 `0`）
 * @property {number} totalCoin 单次总金瓜子数（缺省为 `0`）
 * @property {number} comboTotalCoin 原生连击累计金瓜子数（缺省为 `0`）
 * @property {number} unitPrice 礼物单价（缺省为 `0`）
 * @property {number} priceSeeds 事件 `price` 字段（金瓜子，`0` 表示免费）
 * @property {number} comboStayTimeMs 服务端连击停留窗口（毫秒，缺省为 `0`）
 * @property {string} mergeKey 推荐归并主键（优先 `'batch:<id>'`，否则 `'combo:<user>|<gift>|<coinType>'`）
 */

/**
 * @typedef {Object} GiftComboEntry
 * @property {string} key 归并主键
 * @property {string} giftName 礼物基础名称
 * @property {string} giftId 礼物 ID
 * @property {'gold' | 'silver'} coinType 币种分类
 * @property {number} totalCount 累计送出总数量
 * @property {number} totalPriceSeeds 累计价值（金瓜子，`'silver'` 恒为 `0`）
 * @property {number} lastComboNum 最近一次观测到的 `combo_num`
 * @property {number} lastComboTotalCoin 最近一次观测到的 `combo_total_coin`
 * @property {number} lastEventTimeMs 最近一次命中事件时间戳（毫秒）
 * @property {*} data 插件自定义的附属状态（如 DOM 引用等）
 */

/**
 * @typedef {Object} GiftComboTrackerOptions
 * @property {number=} windowMs 滑动合并时间窗口（毫秒），默认 `4000`
 * @property {boolean=} useServerComboStayTime 为 `true` 且事件包含 `combo_stay_time > 0` 时采用服务端连击停留窗口，默认 `false`
 * @property {((entry: GiftComboEntry, item: Object, opts: Object) => boolean)=} canMerge 合并前自定义校验钩子
 * @property {((entry: GiftComboEntry, item: Object, opts: Object) => void)=} onCreate 新建连击条目回调
 * @property {((entry: GiftComboEntry, delta: {deltaCount: number, deltaPriceSeeds: number, meta: GiftFormattedInfo}, item: Object, opts: Object) => void)=} onUpdate 连击刷新增量回调
 * @property {((entry: GiftComboEntry) => void)=} onExpire 超时或显式调用 `expire()` 清理时回调
 */

/**
 * @typedef {Object} AutoScrollOptions
 * @property {number=} thresholdPx 判定接近底部的像素阈值，默认 `24`
 * @property {boolean=} bindWindowResize 是否在处于跟随状态时监听 `window.resize` 自动贴底，默认 `true`
 * @property {((following: boolean) => void)=} onStateChange 自动跟随状态切换回调
 */
```

### 3.3 全局导出对象与公开签名

```javascript
// 挂载于 window.Dambo（需通过 <script src="/sdk/dambo.js"></script> 同步加载）
{
    VERSION: '1.1.0',
    DamboClient: class DamboClient {
        /** @param {ConnectOptions} [options] */
        constructor(options);
        /** @param {string} eventName @param {Function} handler @returns {() => void} */
        on(eventName, handler);
        /** @param {string} eventName @param {Function} handler @returns {() => void} */
        once(eventName, handler);
        /** @param {string} eventName @param {Function} [handler] @returns {void} */
        off(eventName, handler);
        /** @returns {void} */
        disconnect();
        /** @returns {void} */
        close();
        /** @returns {void} */
        reconnect();
        /** @returns {ConnectionState} */
        getState();
        /** @returns {RoomStateSnapshot | null} */
        getRoomState();
        /** @returns {SnapshotData | null} */
        getSnapshot();
        /** @returns {RoomInfoEvent | null} */
        getRoomInfo();
        /** @returns {string} */
        getUrl();
    },
    /** @param {ConnectOptions} [options] @returns {DamboClient} */
    connect(options),
    utils: {
        DEFAULT_AVATAR_SVG: 'data:image/svg+xml;utf8,<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="%2364748b"><circle cx="12" cy="8" r="4"/><path d="M4 20c0-4 4-6 8-6s8 2 8 6"/></svg>',
        /** @param {string | null | undefined} url @returns {string} 将 http:// 或 // 升级为 https:// */
        normalizeUrl(url),
        /** @param {string | SnapshotItem | EventItem | null | undefined} itemOrKind @returns {boolean} 匹配 'superchat' 或 'super_chat' */
        isSuperChat(itemOrKind),
        /** @param {SnapshotItem | EventItem | null | undefined} item @returns {boolean} 匹配 SC、大航海或 price > 0 的礼物 */
        isPaidEvent(item),
        /**
         * 将金瓜子（1000 = 1 元）转换为格式化金额字符串（如 '¥30'、'¥5.2'）。
         * 当 `priceSeeds <= 0` 或非有效数字时返回空字符串 `''`。
         * @param {number | null | undefined} priceSeeds
         * @param {string} [currencySymbol='¥']
         * @returns {string}
         */
        formatPrice(priceSeeds, currencySymbol),
        /** @param {SnapshotItem | EventItem | Object | null | undefined} item @returns {GiftFormattedInfo} */
        formatGift(item),
        /** @param {number | null | undefined} level @returns {string} 1->'总督', 2->'提督', 3->'舰长', 其余 '' */
        guardLevelName(level),
        /** @param {SnapshotItem | EventItem | null | undefined} item @returns {GuardFormattedInfo} */
        formatGuard(item),
        /** @param {SnapshotItem | EventItem | null | undefined} item @returns {InteractFormattedInfo} */
        formatInteract(item),
        /** @param {SnapshotItem | EventItem | null | undefined} item @returns {number} 推荐置顶时长（毫秒） */
        superChatDurationMs(item),
        /** @param {SnapshotItem | EventItem | {timestamp?: number} | null | undefined} item @param {number} totalMs @param {RemainingDurationOptions | number} [options] @returns {number} 剩余有效时长（毫秒，过期返回 0） */
        remainingDurationMs(item, totalMs, options),
        /** @param {string | SnapshotItem | EventItem | null | undefined} faceOrItem @param {string} [className='dambo-avatar'] @returns {HTMLImageElement} */
        createAvatar(faceOrItem, className),
        /** @param {GiftComboTrackerOptions} [options] @returns {{push(item, opts?): GiftComboEntry|null, expire(key, expectedEntry?): boolean, pruneExpiredSnapshots(): void, clear(): void, size(): number}} */
        createGiftComboTracker(options),
        /** @param {HTMLElement | null | undefined} containerEl @param {AutoScrollOptions} [options] @returns {{schedule(forceOpts?): void, isFollowing(): boolean, isNearBottom(): boolean, reset(): void, destroy(): void}} */
        createAutoScroll(containerEl, options),
        /** @returns {{set(fn, delayMs): number|null, clear(id): void, clearAll(): void, size(): number}} */
        createTimerBag(),
        /** @param {RoomStateSnapshot | SnapshotData | null | undefined} roomState @returns {string} */
        formatRoomBadge(roomState),
        /** @param {HTMLElement} parentEl @param {SnapshotItem | EventItem | {content?: string, emoticon?: string, emots?: Object.<string, string>}} item @param {RenderContentOptions} [options] @returns {void} */
        renderContent(parentEl, item, options),
        /** @param {string} name @param {string|null} [fallback=null] @returns {string|null} */
        getQueryParam(name, fallback),
        /** @param {string} name @param {boolean} [fallback=false] @returns {boolean} */
        getQueryBool(name, fallback),
    }
}
```

### 3.4 SDK 事件名与错误 / 重连机制

- **生命周期事件**：`'open' ()`、`'close' (CloseEvent)`、`'error' (Event | Error)`、`'reconnect' ({ attempt, delayMs })`、`'state' (ConnectionState)`、`'reset' (SnapshotData)`、`'*' (envelope)`。
- **通道 A（`channel: 'feed'`）**：`'reset' (SnapshotData)`、`'snapshot' (SnapshotData)`、`'room_state' (RoomStateSnapshot)`、`'event' (SnapshotItem, EventMeta)`、`<kind>`（精确匹配 `item.kind`；`'super_chat'` 会同时触发 `'super_chat'` 与 `'superchat'`）。
- **通道 B（`channel: 'event'`）**：`'room_info' (RoomInfoEvent)`、`'batch' (EventBatchData)`、`'batch_item' (EventItem, EventBatchData)`、`'event' (EventItem, EventMeta)`、`<kind>`（精确匹配 `item.kind`；`'super_chat'` 会同时触发 `'super_chat'` 与 `'superchat'`）。
- **错误与重连**：JSON 解析失败仅触发 `'error'` 而不崩溃；监听器异常按回调隔离互不影响；Socket 异常或 `connectTimeoutMs` 看门狗超时会关闭连接并触发带 `±15%` 随机抖动的指数退避重连：`clamp(baseDelay ± 15%, 200, maxReconnectDelayMs)`。浏览器触发 `'online'` 时会立即唤醒重连，触发 `'pagehide'` 时会干净关闭底层连接。

---

## 4. 完整端到端最小模板（`static/html/minimal-demo/`）

新建插件时请直接以以下 3 个文件作为基础脚手架：

### 4.1 `static/html/minimal-demo/plugin.json`

```json
{
  "title": "Minimal Demo Overlay",
  "description": "End-to-end minimal Dambo plugin using /sdk/dambo.js",
  "obs": {
    "width": 400,
    "height": 600
  }
}
```

### 4.2 `static/html/minimal-demo/index.html`

```html
<!DOCTYPE html>
<html lang="zh-CN">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <meta name="referrer" content="no-referrer">
    <title>Minimal Demo Overlay</title>
    <style>
        * { box-sizing: border-box; margin: 0; padding: 0; }
        html, body {
            width: 100%;
            height: 100%;
            background: transparent !important;
            overflow: hidden;
            font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", sans-serif;
            color: #ffffff;
        }
        .shell {
            display: flex;
            flex-direction: column;
            width: 100%;
            height: 100%;
            /* 右上角预留 100x44px 桌面悬浮控制条安全区 */
            padding: 48px 12px 12px;
            gap: 8px;
        }
        .status {
            font-size: 12px;
            color: #94a3b8;
            text-shadow: 0 1px 2px rgba(0, 0, 0, 0.8);
            white-space: nowrap;
            overflow: hidden;
            text-overflow: ellipsis;
        }
        .feed {
            flex: 1 1 auto;
            min-height: 0;
            display: flex;
            flex-direction: column;
            justify-content: flex-end;
            gap: 6px;
            overflow: hidden;
        }
        .row {
            display: flex;
            align-items: center;
            gap: 6px;
            padding: 6px 10px;
            border-radius: 8px;
            background: rgba(18, 20, 26, 0.75);
            border: 1px solid rgba(255, 255, 255, 0.12);
            font-size: 14px;
            line-height: 1.4;
            word-break: break-word;
        }
        .row.paid {
            border-left: 3px solid #f87171;
        }
        .avatar {
            width: 20px;
            height: 20px;
            border-radius: 50%;
            object-fit: cover;
            flex-shrink: 0;
        }
        .name { color: #7dd3fc; font-weight: 600; flex-shrink: 0; }
        .price { color: #fde047; font-weight: 700; margin-left: auto; flex-shrink: 0; }
        .dambo-sticker { height: 36px; vertical-align: middle; }
        .dambo-emot { height: 18px; vertical-align: middle; }
    </style>
</head>
<body>
    <div class="shell">
        <div id="status" class="status">Connecting...</div>
        <div id="feed" class="feed"></div>
    </div>
    <script src="/sdk/dambo.js"></script>
    <script src="./plugin.js"></script>
</body>
</html>
```

### 4.3 `static/html/minimal-demo/plugin.js`

```javascript
(function () {
    'use strict';

    const statusEl = document.getElementById('status');
    const feedEl = document.getElementById('feed');
    const MAX_ITEMS = 40;

    function renderRow(item) {
        if (!item) return;

        const row = document.createElement('div');
        row.className = Dambo.utils.isPaidEvent(item) ? 'row paid' : 'row';

        row.appendChild(Dambo.utils.createAvatar(item, 'avatar'));

        const name = document.createElement('span');
        name.className = 'name';
        name.textContent = item.uname || 'Viewer';
        row.appendChild(name);

        const body = document.createElement('span');
        if (item.kind === 'guard') {
            body.textContent = Dambo.utils.formatGuard(item).summaryText;
        } else {
            Dambo.utils.renderContent(body, item);
        }
        row.appendChild(body);

        const priceText = Dambo.utils.formatPrice(item.price);
        if (priceText) {
            const price = document.createElement('span');
            price.className = 'price';
            price.textContent = priceText;
            row.appendChild(price);
        }

        feedEl.appendChild(row);
        while (feedEl.children.length > MAX_ITEMS) {
            feedEl.removeChild(feedEl.firstChild);
        }
    }

    const client = Dambo.connect({
        channel: 'feed',
        url: Dambo.utils.getQueryParam('ws') || undefined,
        replaySnapshot: false,
        showLike: Dambo.utils.getQueryBool('show_like', false),
    });

    client.on('room_state', (roomState) => {
        statusEl.textContent = Dambo.utils.formatRoomBadge(roomState);
    });

    client.on('snapshot', (snapshotData) => {
        feedEl.textContent = '';
        const items = Array.isArray(snapshotData?.items) ? snapshotData.items.slice(-MAX_ITEMS) : [];
        items.forEach(renderRow);
    });

    client.on('chat', renderRow);
    client.on('gift', renderRow);
    client.on('superchat', renderRow);
    client.on('guard', renderRow);
})();
```

---

## 5. 参考插件生产级设计模式

开发功能完善的挂件（SC/舰长置顶栏、进房停靠区、可滚动历史或 Channel B 聚合消费端）时，请遵循以下模式：

1. **贴底可滚动消息流 CSS**：
   ```css
   .danmaku-container {
       flex: 1 1 auto;
       min-height: 0;
       display: flex;
       flex-direction: column;
       overflow-y: auto;
       overflow-x: hidden;
       scrollbar-width: none;
   }
   .danmaku-container > :first-child {
       margin-top: auto;
   }
   ```
   每次 `container.appendChild(div)` 或原地刷新礼物连击后，裁剪旧节点（`while (container.children.length > MAX_ITEMS) container.removeChild(container.firstChild);`），并交由 `const autoScroll = Dambo.utils.createAutoScroll(container, { thresholdPx: 24 });` 调用 `autoScroll.schedule()` 在 `requestAnimationFrame` 帧末同步置底，同时自动保留用户上滑阅读历史保护。

2. **快照重置、定时器回收池与置顶卡片去重**：
   - 使用 `const timerBag = Dambo.utils.createTimerBag()` 统一管理淡出与 TTL 定时器；在 `'snapshot'`（或 `'reset'`）回调中调用 `timerBag.clearAll()`、`giftCombos.clear()`、`autoScroll.reset()`、重置去重集合并通过 `el.textContent = ''` 清空容器。
   - 使用 `item.id != null ? 'id:' + item.id : item.kind + ':' + (item.uid || 0) + ':' + (item.timestamp || 0) + ':' + (item.content || '')` 对置顶 SC/舰长卡片去重，避免重连快照重复插入。

3. **快照回放剩余寿命（TTL）计算**：
   - **置顶 SC / 舰长**：计算 `totalMs = Dambo.utils.superChatDurationMs(item)` 与 `remainingMs = Dambo.utils.remainingDurationMs(item, totalMs, { isSnapshot: Boolean(meta?.isSnapshot), minMs: 3000 })`；若 `remainingMs <= 0` 则跳过。
   - **瞬态进房停靠（`ENTER_TTL_MS = 3000`）**：计算 `delayMs = Dambo.utils.remainingDurationMs(item, ENTER_TTL_MS, { isSnapshot: Boolean(meta?.isSnapshot) })`；若 `delayMs <= 0` 则跳过。

4. **上舰、礼物与互动字段标准化 / 连击滑动归并**：
   - **上舰（`kind === 'guard'`）**：统一调用 `const g = Dambo.utils.formatGuard(item)`，自动剥离 `<%...%>` 标记、开头重复 `${item.uname}` 及 `在主播(的)房间/直播间` 前缀，返回 `{ guardName, badgeLabel, actionText, highlightText, summaryText }`。
   - **互动（`enter` / `follow` / `share` / `special_follow` / `mutual_follow` / `guard_enter`）**：统一调用 `const info = Dambo.utils.formatInteract(item)`，标准化 `msg_type`、`guardLevel`/`guardName` 与非空兜底动作文案（`summaryText`，避免 `enter` 等事件正文为空时出现空白行）。
   - **礼物（`kind === 'gift'`）**：使用 `Dambo.utils.formatGift(item)` 提取标准化字段，或使用 `const giftCombos = Dambo.utils.createGiftComboTracker({ windowMs: 4000, useServerComboStayTime: true, onCreate, onUpdate, onExpire })` 基于 `batch_combo_id` / `combo_id` 或 `uid/uname + gift_id/gift_name + coin_type` 在滑动窗口（优先采用服务端 `combo_stay_time`，否则回退 4 秒）内完成去重、免费/付费隔离与增量/累计双模式计数；快照回放结束后调用 `giftCombos.pruneExpiredSnapshots()` 清理历史过期记录。

5. **Channel B（`/ws/event`）聚合事件消费模式**：
   适用于仪表盘、语音播报（TTS）、提醒或高并发弹幕摘要场景，直接消费蓄水池聚合计数（`ev.count`）与代表用户列表（`ev.sample_users`）：
   ```javascript
   (function () {
       'use strict';

       const headerEl = document.getElementById('room-header');
       const listEl = document.getElementById('batch-list');
       const MAX_ITEMS = 60;

       const client = Dambo.connect({
           channel: 'event',
           url: Dambo.utils.getQueryParam('ws') || undefined,
           showLike: Dambo.utils.getQueryBool('show_like', false),
       });

       client.on('room_info', (info) => {
           const status = info.live_status === 1 ? 'LIVE' : 'OFFLINE';
           headerEl.textContent = `${info.room_uname || info.room_id} · ${info.title || ''} [${status}]`;
       });

       client.on('batch', (batch) => {
           if (!Array.isArray(batch?.events)) return;
           for (const ev of batch.events) {
               const row = document.createElement('div');
               row.className = `batch-row priority-${batch.priority} kind-${ev.kind}`;

               const users = Array.isArray(ev.sample_users) && ev.sample_users.length > 0
                   ? ev.sample_users.join('、')
                   : (ev.uname || 'Viewer');
               const countBadge = ev.count > 1 ? ` ×${ev.count}` : '';
               const priceText = Dambo.utils.formatPrice(ev.price);
               const summary = ev.kind === 'guard'
                   ? Dambo.utils.formatGuard(ev).summaryText
                   : ev.content;

               row.textContent = `[P${batch.priority}] ${users}: ${summary}${countBadge}${priceText ? ' (' + priceText + ')' : ''}`;
               listEl.appendChild(row);
               while (listEl.children.length > MAX_ITEMS) {
                   listEl.removeChild(listEl.firstChild);
               }
           }
       });
   })();
   ```

---

## 6. Agent 自检清单（Agent Self-Verification Checklist）

在 `static/html/<plugin-id>/` 下生成或修改插件后，AI Agent **必须**在交付前完成以下静态审计与动态探活：

### 6.1 静态审计

1. **HTML `<head>` 与同步 SDK 标签**：
   - 确认 `static/html/<plugin-id>/index.html` 的 `<head>` 中包含 `<meta name="referrer" content="no-referrer">`。
   - 确认 `<script src="/sdk/dambo.js"></script>` 位于插件脚本**之前**，且**未**添加 `defer`、`async` 或 `type="module"` 属性。
2. **禁用模式扫描**：
   - 扫描 `static/html/<plugin-id>/` 下所有 `.html`、`.js` 和 `.css` 文件，确认不存在：
     - `.innerHTML`
     - `require(` 或裸 `import ` 语句
     - 硬编码的本地回环主机名、IP 字面量或固定端口号
   - 确认 SDK 使用 `/sdk/dambo.js`，本地插件资源通过 `<base href="/<plugin-id>/">` 配合相对路径（`./plugin.css`、`./plugin.js`、`./assets/...`）或以 `/<plugin-id>/...` 引用。
3. **Manifest 与 JS 语法校验**：
   ```bash
   python3 -m json.tool static/html/<plugin-id>/plugin.json >/dev/null
   node --check static/html/<plugin-id>/*.js
   ```

### 6.2 动态路由探活

从当前生效的 Dambo 配置文件（`etc/config.toml`）中动态读取 `[server]` `host` 与 `port`（禁止写死端口），在服务运行时发起 HTTP 探活：

```bash
# 1. 从 etc/config.toml 动态提取配置的 host 与 port
SERVER_HOST="$(awk '/^\[server\]/{f=1;next} /^\[/{f=0} f && /^[[:space:]]*host[[:space:]]*=/{sub(/^[^"]*"/,"");sub(/".*$/,"");print;exit}' etc/config.toml)"
SERVER_PORT="$(awk '/^\[server\]/{f=1;next} /^\[/{f=0} f && /^[[:space:]]*port[[:space:]]*=/{sub(/^[^=]*=[[:space:]]*/,"");sub(/[[:space:]#].*$/,"");print;exit}' etc/config.toml)"

# 2. 验证 GET /<plugin-id>/ 与 GET /sdk/dambo.js 均返回 200 OK 且带有 Cache-Control: no-store
curl -sS -D - -o /dev/null "http://${SERVER_HOST}:${SERVER_PORT}/<plugin-id>/"
curl -sS -D - -o /dev/null "http://${SERVER_HOST}:${SERVER_PORT}/sdk/dambo.js"
```
