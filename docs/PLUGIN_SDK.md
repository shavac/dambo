# Dambo 插件开发规范（`PLUGIN_SDK.md`）

> 本文档面向 Dambo（`dambo-core` + `dambo-ui`）Web / OBS 挂件插件开发者，所有路径、配置、SDK 接口与 WebSocket 数据模型均与当前后端源码严格对齐。

---

## 1. 架构与红线约束

### 1.1 目录结构与自动发现

Dambo 启动 HTTP 服务后，从静态配置 `[server].html_dir`（默认 `"static/html"`）自动扫描所有**一级子目录**：

```text
static/html/
├── default/                # 内置：默认透明 OBS 弹幕挂件（对应 GET / 与 GET /default）
├── list/                   # 内置：全量交互式弹幕监控（对应 GET /list）
├── sc-ticker/              # 内置 / 示例：SC & 大航海倒计时卡片（对应 GET /sc-ticker）
└── my-widget/              # 你的自定义插件（对应 GET /my-widget）
    ├── index.html          # 【必须】入口 HTML
    ├── plugin.json         # 【推荐】插件清单（标题、描述、OBS/悬浮窗尺寸）
    ├── style.css           # 可选资源
    └── app.js              # 可选资源
```

- **发现规则**（见 `dambo-core/src/plugin.rs`）：
  1. 目录内**必须存在 `index.html` 常规文件**，否则会被忽略。
  2. 目录名 `<plugin-id>` **仅允许 ASCII 字母、数字、`-`、`_`**（禁止点号 `.`、空格、斜杠等）。
  3. 以下保留路由名（不区分大小写）**严禁作为插件目录名**：
     `ws`、`api`、`sdk`、`static`、`assets`、`health`。
  4. `default` 与 `list` 标记为内置插件（`is_builtin = true`）并排在列表前两位，其余插件按名称字母序排列。
- **热监听（Hot Watch）**：`PluginRegistry` 使用 `notify` 递归监听 `html_dir`（约 `200ms` 防抖），新增/删除插件或修改 `plugin.json` 会实时刷新控制面板与系统托盘 `Plugins` 列表，无需重启主程序。

### 1.2 HTTP 路由与安全边界

由 `dambo-core/src/server.rs` 统一托管（默认地址 `http://localhost:3838`）：

| 路由 | 用途 |
| :--- | :--- |
| `GET /` | 服务 `static/html/default/index.html` |
| `GET /sdk/dambo.js` | 服务内置编译期嵌入的官方纯 Web SDK（`application/javascript; charset=utf-8`） |
| `GET /{plugin}` / `GET /{plugin}/` | 服务 `static/html/{plugin}/index.html` |
| `GET /{plugin}/{*path}` | 服务插件子目录静态资产（如 `style.css`、`app.js`、`fonts/foo.woff2` 等） |
| `GET /ws` | **通道 A**：面向视觉插件的实时 WebSocket 流 |
| `GET /ws/event` | **通道 B**：面向下游消费、统计与提醒的整形聚合 WebSocket 流 |

- **强制无缓存**：除 WebSocket 升级外，所有 HTTP 响应均附带 `Cache-Control: no-store, no-cache, must-revalidate` 与 `Pragma: no-cache`，方便在 OBS 或浏览器中修改文件后直接刷新生效。
- **路径穿越防护**：所有子路径均校验规范化前缀，包含 `..`、`\` 或 `\0` 的请求会直接返回 `404`。
- **绝对/相对路径建议**：为兼容通过 `GET /<plugin>` 与 `GET /<plugin>/`（末尾是否带 `/`）两种方式访问，引用同目录资源建议统一使用以插件名开头的绝对路径，例如 `<link rel="stylesheet" href="/my-widget/style.css">`，SDK 统一写为 `<script src="/sdk/dambo.js"></script>`。

### 1.3 纯 Web 红线与兼容性约束

1. **严禁耦合宿主 API**：插件代码必须 100% 基于标准 Web 技术（HTML / CSS / ES2020+ / WebSocket）。**严禁**出现 `window.__TAURI__`、`window.__TAURI_INTERNALS__` 或任何 Tauri 专属 API。同一个插件 URL 必须同时能在 **OBS Studio Browser Source**、**普通现代浏览器** 与 **Dambo 桌面透明悬浮窗** 中正常运作。
2. **B 站防盗链头部（必加）**：B 站头像（`face`）与表情 CDN 图片存在 Referer 校验，所有插件 `index.html` 的 `<head>` 中必须包含：
   ```html
   <meta name="referrer" content="no-referrer">
   ```
3. **透明背景与防滚动溢出**：用于 OBS 叠加或桌面悬浮的挂件，应设置：
   ```css
   html, body {
       width: 100%;
       height: 100%;
       margin: 0;
       padding: 0;
       background: transparent !important;
       overflow: hidden;
   }
   ```
4. **桌面悬浮窗控制栏安全区（重要）**：
   当用户在 Dambo 面板中点击「桌面弹窗 / Open Desktop Overlay」打开插件时，宿主会创建一个无边框透明置顶窗口（默认 `always_on_top: true`），并通过 Shadow DOM 注入最顶层悬浮控制条（`#dambo-overlay-host`，`z-index: 2147483647`）：
   - 宿主仅在窗口右上角（`top: 8px; right: 8px`，宽约 `90px`、高约 `34px`）渲染「拖动 / 图钉置顶 / 关闭」三按钮胶囊（鼠标悬停窗口或触屏点击时显示，支持 `Esc` 关闭）。
   - **设计约束**：请避免在插件页面**右上角 `100×44px` 区域**放置高频点击按钮或会被裁切的关键文字；纯展示类 OBS 叠加插件建议避免遮挡底层，对非交互容器设置 `pointer-events: none`（滚动监控类如 `list` 可保留正常滚动交互）。
5. **XSS 安全**：严禁使用 `innerHTML` 拼接直播间昵称（`uname`）、弹幕文本（`content`）或房名，请使用 `textContent` / `document.createTextNode` 或直接使用 `Dambo.utils.renderContent()`。

---

## 2. `plugin.json` 清单规范

文件位于 `static/html/<plugin-id>/plugin.json`。该文件为可选元数据文件，用于决定面板卡片展示及桌面悬浮窗初始大小。

### 2.1 字段说明

| 字段路径 | 类型 | 必填 | 默认值 | 说明与校验规则 |
| :--- | :--- | :---: | :--- | :--- |
| `title` | `string` | 否 | 目录名 `<plugin-id>` | 插件显示名称；后端会执行 `trim()`，空白字符串自动忽略并回退目录名 |
| `description` | `string` | 否 | `null` | 插件简短说明；经 `trim()` 处理，空白字符串自动忽略 |
| `obs` | `object` | 否 | `null` | OBS / 桌面悬浮窗推荐视口尺寸对象 |
| `obs.width` | `integer` (`u32`) | 否 | 桌面默认 `420` | 建议视口宽度（像素）；仅当 `> 0` 时生效 |
| `obs.height` | `integer` (`u32`) | 否 | 桌面默认 `720` | 建议视口高度（像素）；仅当 `> 0` 时生效 |

- **容错降级策略**：缺失 `plugin.json`、读取失败或 JSON 语法错误时，插件**仍可正常发现与访问**；后端记录 `warn` 日志并按上述默认值安全降级。

### 2.2 标准示例

```json
{
  "title": "极简透明弹幕",
  "description": "支持表情渲染、高颜值文字描边与 SC/舰长置顶提示的透明叠加挂件。",
  "obs": {
    "width": 420,
    "height": 720
  }
}
```

---

## 3. `/sdk/dambo.js` API 参考

Dambo 后端内嵌了零依赖纯 Web SDK（源码位于 [`dambo-core/assets/dambo.js`](../dambo-core/assets/dambo.js)），运行时由 `GET /sdk/dambo.js` 提供，并在 [`docs/sdk/dambo.js`](sdk/dambo.js) 同步保留了一份完全一致的只读副本用于本地 IDE 的 JSDoc 类型提示。

> 注：内置示例插件（`default` / `list` / `sc-ticker`）展示了不引入 SDK、直接用原生 `new WebSocket(\`\${protocol}//\${location.host}/ws\`)` 建连的最底层做法；新开发的第三方插件推荐直接引入 `/sdk/dambo.js` 以获得自动指数退避重连、快照管理与安全表情渲染工具。

### 3.1 引入方式

```html
<script src="/sdk/dambo.js"></script>
<script>
  const client = Dambo.connect({ channel: 'feed', replaySnapshot: true });
</script>
```

### 3.2 `Dambo.connect(options)`

创建并立即打开一条 WebSocket 连接，返回 `DamboClient` 实例。

| 配置项 (`ConnectOptions`) | 类型 | 默认值 | 说明 |
| :--- | :--- | :--- | :--- |
| `channel` | `'feed' \| 'event'` | `'feed'` | `'feed'` 连接 `/ws`（视觉流）；`'event'` 连接 `/ws/event`（整形聚合与净流） |
| `url` | `string` | 自动推导 | 未传时按当前页面协议（`ws:` / `wss:`）和 `location.host` 自动拼接 `/ws` 或 `/ws/event` |
| `autoReconnect` | `boolean` | `true` | 非主动断开时是否开启指数退避重连 |
| `reconnectDelayMs` | `number` | `2000` | 首次重连延迟（毫秒，最小钳制为 `200`） |
| `maxReconnectDelayMs` | `number` | `10000` | 最大重连退避上限（毫秒，`delay = min(max, base * 2^(attempt - 1))`） |
| `replaySnapshot` | `boolean` | `false` | 仅对 `'feed'` 有效：设为 `true` 时，收到 `Snapshot` 会将历史 `items` 依次派发给 `event` 与具体 `kind` 监听，并传入 `{ isSnapshot: true }` |
| `showLike` | `boolean \| null` | `null` | 显式设为 `false` 时屏蔽 `kind === 'like'`；如需沿用内置插件 `?show_like=1` 约定，可传 `Dambo.utils.getQueryBool('show_like', false)` |

### 3.3 `DamboClient` 实例方法

| 方法签名 | 返回值 | 说明 |
| :--- | :--- | :--- |
| `on(eventName, handler)` | `() => void` | 注册监听，返回可直接调用的取消订阅函数 |
| `once(eventName, handler)` | `() => void` | 注册一次性监听，触发一次后自动移除 |
| `off(eventName, handler?)` | `void` | 移除指定回调；省略 `handler` 时清空该 `eventName` 的全部监听 |
| `disconnect()` / `close()` | `void` | 主动断开并停止重连定时器，状态切为 `'closed'` |
| `reconnect()` | `void` | 重置退避次数、关闭旧连接并立即重连 |
| `getState()` | `'connecting' \| 'open' \| 'reconnecting' \| 'closed'` | 返回当前连接生命周期状态 |
| `getRoomState()` | `RoomStateSnapshot \| null` | 获取最近收到的房间状态（仅 `'feed'` 通道维护） |
| `getSnapshot()` | `SnapshotData \| null` | 获取最近收到的完整快照（仅 `'feed'` 通道维护） |
| `getRoomInfo()` | `RoomInfoEvent \| null` | 获取最近收到的完整房间元数据（仅 `'event'` 通道维护） |
| `getUrl()` | `string` | 返回实际连接的 WebSocket URL |

### 3.4 可订阅事件一览

#### 生命周期与通配事件（双通道通用）

| 事件名 | 回调签名 | 说明 |
| :--- | :--- | :--- |
| `'open'` | `() => void` | WebSocket 建连成功 |
| `'close'` | `(closeEvent: CloseEvent) => void` | 连接关闭 |
| `'error'` | `(err: Event \| Error) => void` | 连接错误或 JSON 解析异常 |
| `'reconnect'` | `(info: { attempt: number, delayMs: number }) => void` | 触发自动重连调度时通知（第几次尝试及延迟毫秒数） |
| `'state'` | `(state: ConnectionState) => void` | 连接状态变更 |
| `'*'` | `(envelope: ChannelAMessage \| ChannelBMessage) => void` | 每一帧原始解析后的 `{ type, data }` 信封 |

#### 通道 A（`channel: 'feed'` -> `/ws`）专属事件

| 事件名 | 回调签名 | 说明 |
| :--- | :--- | :--- |
| `'snapshot'` | `(snapshot: SnapshotData) => void` | 建连首帧、切换房间或客户端落后恢复时下发 |
| `'room_state'` | `(state: RoomStateSnapshot) => void` | 收到 `Snapshot` 或 `RoomState` 时触发，适合更新标题/开播灯/主播名 |
| `'event'` | `(item: SnapshotItem, meta: { isSnapshot: boolean }) => void` | 捕获所有推送事件（`replaySnapshot: true` 时也包含历史条目） |
| `<kind>`（例如 `'chat'`、`'gift'`、`'superchat'`、`'guard'`、`'enter'` 等） | `(item: SnapshotItem, meta: { isSnapshot: boolean }) => void` | 按事件 `kind` 精确分发；收到兼容写法 `'super_chat'` 时会同时分发给 `'super_chat'` 与 `'superchat'` |

#### 通道 B（`channel: 'event'` -> `/ws/event`）专属事件

| 事件名 | 回调签名 | 说明 |
| :--- | :--- | :--- |
| `'room_info'` | `(info: RoomInfoEvent) => void` | 建连首帧及切房重连后下发的完整房间元数据 |
| `'batch'` | `(batch: EventBatchData) => void` | 三通道整形或连击结算后下发的批次载荷 |
| `'batch_item'` | `(item: EventItem, batch: EventBatchData) => void` | 将 `batch.events` 逐条拆解派发，附带所属 `batch` 上下文 |

### 3.5 `Dambo.utils` 辅助工具函数

| 工具成员 | 签名 | 说明 |
| :--- | :--- | :--- |
| `DEFAULT_AVATAR_SVG` | `string` | 离线可用默认用户头像 Data URI |
| `isSuperChat` | `(itemOrKind) => boolean` | 判断是否醒目留言（同时识别 `'superchat'` 与 `'super_chat'`） |
| `isPaidEvent` | `(item) => boolean` | 是否付费事件（SC / 大航海 / `price > 0` 礼物） |
| `formatPrice` | `(priceSeeds, symbol = '¥') => string` | 将金瓜子（`1000 = ¥1`）转成金额文本（如 `¥30` / `¥5.2`），`<= 0` 返回 `''` |
| `guardLevelName` | `(level) => string` | `1` -> `'总督'`，`2` -> `'提督'`，`3` -> `'舰长'`，其余 `''` |
| `superChatDurationMs` | `(item) => number` | 返回 SC / 大航海标准置顶时长（舰长 `90000`；`>=1000` 元 `300000`；`>=500` 元 `180000`；`>=100` 元 `120000`；其余 `60000`） |
| `formatRoomBadge` | `(roomState) => string` | 格式化为 `房间号(主播): 标题`（附 `Offline` / `Disconnected` 状态后缀） |
| `renderContent` | `(parentEl, item, opts?) => void` | 安全渲染文本、B 站大表情 `emoticon`（类名默认 `dambo-sticker`）与行内 `[dog]` 表情 `emots`（类名默认 `dambo-emot`），图片加载失败自动降级纯文本，全程无 `innerHTML` |
| `getQueryParam` | `(name, fallback = null) => string \| null` | 读取页面 URL Query 参数 |
| `getQueryBool` | `(name, fallback = false) => boolean` | 解析 Query 布尔参数（支持 `1/0`、`true/false`、`yes/no`） |

---

## 4. 事件与数据载荷字典

### 4.1 通道 A（`/ws`）与通道 B（`/ws/event`）核心差异

| 维度 | 通道 A (`/ws`) | 通道 B (`/ws/event`) |
| :--- | :--- | :--- |
| **定位** | OBS / 桌面视觉渲染直连流 | TTS 播报 / AI 互动 / 数据分析聚合流 |
| **建连首帧** | `type: "Snapshot"`（房间状态 + 历史事件环形缓冲） | `type: "RoomInfo"`（完整直播间与分区元数据） |
| **消息节奏** | 单条无损实时直推，不合并短弹幕或连击 | 三通道整形：短弹幕/点赞在 `[event].window_ms`（默认 `2500ms`）内按蓄水池聚合并提供 `count` 与 `sample_users`；礼物按 `batch_combo_id` 与 `[event].max_combo_wait_secs`（默认 `8s`）合并结算 |
| **头像（`face`）保留** | **全量保留**（含 collector LRU 缓存回填的用户头像） | **仅付费事件保留**（`superchat`、`guard`、付费 `gift`）；`chat`、`enter`、`like`、关注分享及免费礼物一律省略 `face` |
| **历史回放缓冲区** | 环形队列仅存 `chat`、`gift`、`superchat`、`guard`（上限 `[feed].max_history`，默认 `300`）；接收滞后触发 `Lagged` 时服务端自动重发 `Snapshot` | 不重放历史；`priority: 2`（SC/大航海/状态）直通，`priority: 1`（质量弹幕/互动/连击）直通，`priority: 0`（蓄水池汇总）定时冲刷 |
| **服务端过滤开关** | 受 `[feed].show_enter`（是否推送 `enter`）与 `[feed].show_medal`（是否随事件下发勋章）影响；不推送 `combo_end`/`room_info`/`session_end` | 不受 `show_enter` / `show_medal` 展示开关抑制（事件内仅携带数值型 `medal_level`，缺省 `0`） |

---

### 4.2 通道 A（`/ws`）JSON 信封结构

所有帧结构均对应 `dambo-core/src/model.rs` 中的 `ChannelAMessage`（`#[serde(tag = "type", content = "data")]`）：

#### 1) `Snapshot` 帧（建连首帧 / 切房重置 / 客户端滞后恢复）

```json
{
  "type": "Snapshot",
  "data": {
    "room_id": 21452505,
    "room_uname": "主播A",
    "room_title": "今晚歌回",
    "is_live": true,
    "connected": true,
    "items": [
      {
        "id": 1,
        "seq": 12,
        "kind": "chat",
        "uid": 10001,
        "uname": "观众甲",
        "face": "https://i0.hdslb.com/bfs/face/example.jpg",
        "medal_name": "牌子",
        "medal_level": 18,
        "content": "好听！[dog]",
        "emots": { "[dog]": "https://i0.hdslb.com/bfs/emote/example.png" },
        "price": 0,
        "timestamp": 1763280001000
      }
    ]
  }
}
```

#### 2) `Event` 帧（单条实时推送 `SnapshotItem`）

| 字段名 | 类型 | 必然存在 | 说明 |
| :--- | :--- | :---: | :--- |
| `id` | `number` (`u64`) | 是 | 环形 Hub 自增内部 ID |
| `seq` | `number` (`u64`) | 是 | 会话单调序列号 |
| `kind` | `string` | 是 | 事件分类（见 4.4 节字典） |
| `msg_type` | `number` (`u32`) | 否 | 仅 `INTERACT_WORD*` 原生互动出现：`1`=进房 `2`=关注 `3`=分享 `4`=特别关注 `5`=互粉 |
| `uid` | `number` (`u64`) | 是 | 发送者 UID（系统消息多为 `0`） |
| `uname` | `string` | 是 | 发送者昵称 |
| `face` | `string` | 否 | 头像 URL（Channel A 保留全量可用头像；缺失时字段省略） |
| `medal_name` | `string` | 否 | 粉丝勋章名称（受 `[feed].show_medal` 控制） |
| `medal_level` | `number` (`u32`) | 否 | 粉丝勋章等级 |
| `content` | `string` | 是 | 消息文本、礼物描述（`"小心心 x2"`）或系统提示 |
| `emoticon` | `string` | 否 | 弹幕大表情 URL（`DANMU_MSG` 为表情贴图时存在） |
| `emots` | `Record<string, string>` | 否 | 弹幕行内小表情替换映射表（`"[表情名]": "URL"`） |
| `price` | `number` (`u64`) | 是 | 金额（单位：金瓜子，`1000 = ¥1`；免费事件或系统事件为 `0`） |
| `num` | `number` (`u32`) | 否 | 礼物数量或大航海开通月数 |
| `guard_level` | `number` (`u8`) | 否 | 大航海等级：`1`=总督，`2`=提督，`3`=舰长 |
| `ids` | `number[]` | 否 | 被撤回的 SuperChat ID 列表（仅 `superchat_delete`） |
| `timestamp` | `number` (`u64`) | 是 | 13 位 Unix 毫秒时间戳 |

#### 3) `RoomState` 帧（房间状态、开播/下播、标题动态变更）

```json
{
  "type": "RoomState",
  "data": {
    "room_id": 21452505,
    "room_uname": "主播A",
    "room_title": "新标题",
    "is_live": true,
    "connected": true
  }
}
```

---

### 4.3 通道 B（`/ws/event`）JSON 信封结构

对应 `dambo-core/src/model.rs` 中的 `ChannelBMessage`：

#### 1) `RoomInfo` 帧（建连首帧 / 新场次开播元数据）

```json
{
  "type": "RoomInfo",
  "data": {
    "seq": 0,
    "offset_ms": 0,
    "timestamp": 1763280000000,
    "room_id": 21452505,
    "room_uname": "主播A",
    "kind": "room_info",
    "uid": 888,
    "uname": "主播A",
    "content": "今晚歌回",
    "short_id": 512,
    "title": "今晚歌回",
    "area_name": "虚拟日常",
    "parent_area_name": "虚拟主播",
    "live_status": 1,
    "start_time": 1763279000,
    "raw_cmd": "ROOM_INFO"
  }
}
```

#### 2) `EventBatch` 帧（批次事件 `EventBatchData` + `EventItem`）

```json
{
  "type": "EventBatch",
  "data": {
    "batch_id": 7,
    "room_id": 21452505,
    "room_uname": "主播A",
    "priority": 1,
    "timestamp": 1763280002500,
    "events": [
      {
        "seq": 15,
        "kind": "gift",
        "uid": 88,
        "uname": "连击侠",
        "face": "https://i0.hdslb.com/bfs/face/paid.jpg",
        "medal_level": 12,
        "content": "小心心 x5",
        "count": 5,
        "sample_users": ["连击侠"],
        "price": 500,
        "batch_combo_id": "combo:88"
      }
    ]
  }
}
```

`EventItem` 字段要点：
- `priority`：`0` 蓄水池周期批、`1` 直通/连击/社交互动、`2` 醒目留言/大航海/关键状态变化。
- `count`：同一短弹幕/点赞在蓄水池中合并条数，或礼物连击结算总数量，直通事件固定 `1`。
- `sample_users`：聚合事件内最多 5 个不重复触发者昵称。
- `medal_level`：数值型，未佩戴为 `0`（聚合时取最高等级）。
- `face`：仅当事件属于 `superchat` / `guard` / 付费 `gift`（`price > 0`）时出现。

---

### 4.4 全量 `kind` 事件分类字典

下列所有 `kind` 均直接提取自 `collector.rs`、`hub.rs`、`event_engine.rs` 与 `recorder.rs`：

| `kind` | 来源 B 站 Cmd / 节点 | `/ws` | `/ws/event` | 说明与关键字段 |
| :--- | :--- | :---: | :---: | :--- |
| `chat` | `DANMU_MSG` | ✅ | ✅ | 普通弹幕或表情贴图弹幕（`content`、`emoticon`、`emots`） |
| `superchat` | `SUPER_CHAT_MESSAGE`（兼容 `super_chat`） | ✅ | ✅ | 醒目留言；`price` 单位金瓜子（`¥` × 1000） |
| `gift` | `SEND_GIFT` / `SEND_GIFT_V2` | ✅ | ✅ | 礼物；`content` 为 `"名称 x数量"`，`price` 为总金瓜子（银瓜子/免费礼物为 `0`），`num` 为数量；`/ws/event` 会按 `batch_combo_id` 合并连击 |
| `guard` | `USER_TOAST_MSG` / `USER_TOAST_MSG_V2`（已自动对冗余 `GUARD_BUY` 去重） | ✅ | ✅ | 开通大航海；`guard_level`（`1`=总督、`2`=提督、`3`=舰长）、`num`（月数）、`price` |
| `guard_enter` | `ENTRY_EFFECT` | ✅ | ✅ | 舰长/贵族进场特效；`guard_level` 为特权等级，`content` 为清洗后的进场欢迎词 |
| `enter` | `INTERACT_WORD` / `INTERACT_WORD_V2` (`msg_type=1`) | ⚙️ | ✅ | 观众进入直播间；`/ws` 受 `[feed].show_enter` 开关控制且不入环形历史 |
| `follow` | `INTERACT_WORD` / `INTERACT_WORD_V2` (`msg_type=2`) | ✅ | ✅ | 关注主播（`/ws` 不入历史，只实时推） |
| `share` | `INTERACT_WORD` / `INTERACT_WORD_V2` (`msg_type=3`) | ✅ | ✅ | 分享直播间（`/ws` 不入历史，只实时推） |
| `special_follow` | `INTERACT_WORD` / `INTERACT_WORD_V2` (`msg_type=4`) | ✅ | ✅ | 特别关注主播（`/ws` 不入历史，只实时推） |
| `mutual_follow` | `INTERACT_WORD` / `INTERACT_WORD_V2` (`msg_type=5`) | ✅ | ✅ | 与主播互粉（`/ws` 不入历史，只实时推） |
| `like` | `LIKE_INFO_V3_CLICK` | ✅ | ✅ | 观众点赞；高频事件，建议插件提供过滤开关（如 `?show_like=1`） |
| `live` | `LIVE` | ✅ | ✅ | 开播通知（同时刷新 `RoomState.is_live = true`） |
| `preparing` | `PREPARING` | ✅ | ✅ | 下播/进入轮播通知（同时刷新 `RoomState.is_live = false`） |
| `room_change` | `ROOM_CHANGE` | ✅ | ✅ | 房间标题或分区变更（同时刷新 `RoomState.room_title`） |
| `room_silent` | `ROOM_SILENT_ON` / `ROOM_SILENT_OFF` | ✅ | ✅ | 房间全局禁言开启/关闭状态变更 |
| `room_block` | `ROOM_BLOCK_MSG` | ✅ | ✅ | 观众被禁言通知（携带 `uid`、`uname`） |
| `superchat_delete` | `SUPER_CHAT_MESSAGE_DELETE` | ✅ | ✅ | 撤回 SC；`ids` 为待移除的 SC ID 数组 |
| `warning` | `WARNING` | ✅ | ✅ | 超管警告 |
| `cut_off` | `CUT_OFF` | ✅ | ✅ | 超管切断直播 |
| `reconnected` | 采集器内部状态 | ✅ | ✅ | 同场次采集器断线重连恢复信号 |
| `room_info` | 房间接口元数据 | ❌ | ✅ | 仅在 `/ws/event` 首帧与换房时作为 `RoomInfo` 下发（`/ws` 使用 `Snapshot` 与 `RoomState`） |
| `combo_end` / `session_end` | 内部连击结算 / 录制结束 | ❌ | ❌ | 仅后端内部与主录制 `.jsonl` 使用，**不会**通过 `/ws` 或 `/ws/event` 下发 |

---

## 5. 最小可运行模板（Minimal Starter）

新建目录 `static/html/my-widget/`，放入以下两个文件即可自动上线并出现在面板和托盘菜单中：

### 5.1 `static/html/my-widget/plugin.json`

```json
{
  "title": "My Overlay",
  "description": "使用 /sdk/dambo.js 的最小透明弹幕/礼物/SC 挂件模板。",
  "obs": {
    "width": 400,
    "height": 640
  }
}
```

### 5.2 `static/html/my-widget/index.html`

```html
<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <meta name="referrer" content="no-referrer">
  <title>My Dambo Overlay</title>
  <style>
    * { box-sizing: border-box; margin: 0; padding: 0; }
    html, body {
      width: 100%;
      height: 100%;
      background: transparent !important;
      overflow: hidden;
      font-family: -apple-system, BlinkMacSystemFont, "PingFang SC", "Microsoft YaHei", sans-serif;
      color: #ffffff;
    }
    .stage {
      width: 100%;
      height: 100%;
      /* 右上角预留 100x44px 桌面悬浮控制条安全区，内容流自然下移避让 */
      padding: 48px 12px 12px;
      display: flex;
      flex-direction: column;
      justify-content: flex-end;
      align-items: flex-start;
      gap: 8px;
      pointer-events: none;
    }
    .msg {
      display: inline-flex;
      align-items: center;
      gap: 6px;
      max-width: 100%;
      padding: 6px 10px;
      border-radius: 8px;
      background: rgba(15, 23, 42, 0.72);
      border: 1px solid rgba(255, 255, 255, 0.14);
      font-size: 14px;
      line-height: 1.4;
      text-shadow: 0 1px 2px rgba(0, 0, 0, 0.85);
      word-break: break-word;
    }
    .msg.paid {
      background: linear-gradient(135deg, rgba(185, 28, 28, 0.88), rgba(124, 58, 237, 0.84));
      border-color: rgba(254, 240, 138, 0.55);
    }
    .avatar {
      width: 20px;
      height: 20px;
      border-radius: 50%;
      object-fit: cover;
      flex-shrink: 0;
    }
    .uname { color: #7dd3fc; font-weight: 600; flex-shrink: 0; }
    .price { color: #fde047; font-weight: 700; flex-shrink: 0; }
    .dambo-sticker { height: 42px; vertical-align: middle; }
    .dambo-emot { height: 20px; vertical-align: middle; }
  </style>
</head>
<body>
  <div id="stage" class="stage"></div>

  <script src="/sdk/dambo.js"></script>
  <script>
    const stage = document.getElementById('stage');
    const MAX_ITEMS = 30;

    function pushMessage(item) {
      const row = document.createElement('div');
      const paid = Dambo.utils.isPaidEvent(item);
      row.className = paid ? 'msg paid' : 'msg';

      const avatar = document.createElement('img');
      avatar.className = 'avatar';
      avatar.src = item.face || Dambo.utils.DEFAULT_AVATAR_SVG;
      avatar.onerror = () => { avatar.src = Dambo.utils.DEFAULT_AVATAR_SVG; };
      row.appendChild(avatar);

      const uname = document.createElement('span');
      uname.className = 'uname';
      uname.textContent = item.uname || '观众';
      row.appendChild(uname);

      const body = document.createElement('span');
      Dambo.utils.renderContent(body, item);
      row.appendChild(body);

      const priceText = Dambo.utils.formatPrice(item.price);
      if (priceText) {
        const badge = document.createElement('span');
        badge.className = 'price';
        badge.textContent = priceText;
        row.appendChild(badge);
      }

      stage.appendChild(row);
      while (stage.children.length > MAX_ITEMS) {
        stage.removeChild(stage.firstChild);
      }
    }

    const client = Dambo.connect({
      channel: 'feed',
      replaySnapshot: true,
      showLike: Dambo.utils.getQueryBool('show_like', false),
    });

    client.on('snapshot', () => {
      stage.textContent = '';
    });

    client.on('chat', pushMessage);
    client.on('gift', pushMessage);
    client.on('superchat', pushMessage);
    client.on('guard', pushMessage);
  </script>
</body>
</html>
```

---

## 6. 本地调试与打包自检清单

### 6.1 本地调试流程

1. **启动宿主服务**：
   - 运行桌面端（`dambo-ui`）或 CLI（`dambo`），默认在 `http://localhost:3838` 监听（由 `etc/config.toml` 的 `[server].host` / `[server].port` 与 `[server].html_dir` 决定）。
2. **定位或创建插件目录**：
   - 桌面端控制面板点击 **Plugins ▾ -> 打开插件目录**（`open_plugins_dir`），或系统托盘进入 **Plugins -> 打开插件目录**，即会直接打开 `static/html/`。
   - 在其中创建 `<plugin-id>/index.html` 与 `plugin.json`，约 `200ms` 后面板与托盘列表自动出现你的插件条目。
3. **三端联动调试**：
   - **桌面透明悬浮窗调试**：在面板 Plugins 下拉中点击插件卡片的「打开桌面端」，即可直接验证透明背景、文字描边对比度、`obs.width`/`obs.height` 尺寸以及右上角控制栏是否遮挡 UI；修改 HTML/CSS/JS 后如需重载可关闭再打开窗口。
   - **浏览器 + DevTools 调试**：复制面板上的 URL（例如 `http://localhost:3838/my-widget`）在 Chrome/Edge 打开，可借助 DevTools 检查控制台报错与 `/ws` / `/ws/event` 的实时帧，后端已强制 `no-store`，`Cmd/Ctrl + R` 即可拉取最新代码。
   - **OBS Browser Source 调试**：OBS 中添加「浏览器源」，填入插件 URL、设置与 `plugin.json` 一致的 `obs.width × obs.height`，并保持「自定义 CSS」为空（透明背景由插件自身 CSS 控制）。

### 6.2 上线 / 分发前自检清单

- [ ] **目录命名合规**：`<plugin-id>` 仅含 `[A-Za-z0-9_-]`，未占用保留名称（`ws`/`api`/`sdk`/`static`/`assets`/`health`），根目录下包含 `index.html`。
- [ ] **纯 Web 零 Tauri 依赖**：全目录检索确认没有任何 `window.__TAURI__` / `window.__TAURI_INTERNALS__` 调用，在普通 Chrome 中可独立打开并运行。
- [ ] **已包含防盗链元标签**：`<head>` 中存在 `<meta name="referrer" content="no-referrer">`，主播与观众头像及 B 站表情加载正常（`onerror` 兜底不留破图图标）。
- [ ] **无 XSS 拼接**：未用 `innerHTML` 渲染用户昵称 `uname`、弹幕内容 `content` 或直播间标题。
- [ ] **`plugin.json` 完整合法**：JSON 可正常解析，`title`、`description` 准确，`obs.width` / `obs.height` 为正整数。
- [ ] **透明与亮暗背景可读性**：开启透明背景（`background: transparent !important`）后，在白色高亮画面与全黑游戏画面下均有描边或半透明底衬保障可读性。
- [ ] **悬浮安全区避让**：打开桌面悬浮窗并悬停鼠标显现右上角胶囊栏后，插件自身内容未与右上角 `100×44px` 控制区重叠冲突。
- [ ] **兼容切房与断线重连**：在切换房间（收到新 `Snapshot`）或后台重启后，插件能清理旧状态或自动重连恢复，无定时器泄漏。
- [ ] **兼容性事件处理**：同时识别 `superchat` 与 `super_chat`，对高频 `like` 默认按需渲染，未依赖仅写盘不上行 `/ws` 的事件（`combo_end` / `session_end`）。
- [ ] **资产路径与分包检查**：仅引用插件自身相对/同前缀资源或 `/sdk/dambo.js`，未依赖宿主内部 `ui/` 目录；确认插件目录已完整拷贝至发布包 `static/html/<plugin-id>/` 下。
