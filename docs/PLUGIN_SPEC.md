# Dambo 插件技术契约与生成规范（AI 代码生成专用）

> **用途**：本文件配合 [`PLUGIN_GUIDE.md`](PLUGIN_GUIDE.md) 输出的《Dambo 插件需求确认单》使用。当用户上传本文件并回复 **`开始生成`** 时，AI 必须严格遵循本文档的目录规范、安全边界与 `/sdk/dambo.js` v2.0.0 接口，直接生成可放入 `static/html/<plugin-name>/` 即装即用的完整纯 Web 插件代码。

---

## 1. AI 生成总则与硬性不变量

1. **零构建纯 Web 运行环境**：插件直接运行在现代浏览器、OBS Studio 浏览器源与 Dambo 桌面透明悬浮窗中，**无 Node/npm 构建步骤**。**严禁**使用 `require()`、裸 `import`、TypeScript 语法或任何非标准浏览器全局对象；仅使用标准 Web API 与 `/sdk/dambo.js`。
2. **同步加载唯一 SDK 入口**：
   - 每个 `plugin.html` 与自定义 `manage.html` 必须在自有脚本之前通过普通同步标签加载 SDK：
     ```html
     <script src="/sdk/dambo.js"></script>
     ```
     **严禁**添加 `defer`、`async` 或 `type="module"`。
   - **所有能力统一走 `window.Dambo`**：弹幕事件流使用 `Dambo.connect()`，配置/存储/事件总线/权限使用 `Dambo.config`、`Dambo.storage`、`Dambo.memory`、`Dambo.topics`、`Dambo.acl`；**严禁**绕过 SDK 直连 `/ws`、`/ws/event` 或手拼 `fetch('/api/v1/...')`。
3. **本地资源必须用 `./` 相对路径 & 零硬编码主机端口**：
   - **除 SDK 外严禁以 `/` 开头引用本地文件**：插件自身的 `.css`、`.js`、`assets/`（包括 HTML `<link>` / `<script>` 与 JS 里的图片/音效路径）**必须**使用 `./` 开头的相对路径（`./plugin.css`、`./plugin.js`、`./manage.css`、`./manage.js`、`./assets/...`），配合 `<head>` 中的 `<base href="/{plugin-name}/">` 解析。
   - **严禁**写成 `/plugin.css`、`/plugin.js`、`/manage.js`、`/{plugin-name}/plugin.js` 或 `/static/html/{plugin-name}/...`（绝对路径会绕过 `<base href>`，直接请求根路径导致 `404` 加载失败）；**唯一允许以 `/` 开头加载的脚本只有 `<script src="/sdk/dambo.js"></script>`**。
   - **严禁**在任何 `.html` / `.js` / `.css` 中写死 `localhost`、`127.0.0.1` 或端口号（如 `3838`）；调用 `Dambo.connect()` 时省略 `url`（或传 `Dambo.utils.getQueryParam('ws') || undefined`），由 SDK 根据 `window.location` 自动推导。
4. **Referer 策略严格隔离（极易踩坑）**：
   - `plugin.html`（展示端）`<head>` **必须**包含 `<meta name="referrer" content="no-referrer">`，否则 B 站头像（`face`）与表情图片（`emoticon`/`emots`）会被 CDN 拦截返回 `403`。
   - 自定义 `manage.html`（管理端）**严禁**包含 `no-referrer`，因为管理态鉴权依赖同源 `Referer: /{plugin-name}/m`。
5. **严禁 `.innerHTML`（防 XSS）**：
   - **严禁**对用户可控字段（`uname`、`content`、`room_uname`、`room_title`、`medal_name`）或跨插件文本（`reason`、`note`、`key`、`value`、`description`）使用 `.innerHTML`。
   - 必须使用 `.textContent`、`document.createTextNode()`、`el.replaceChildren()`、`Dambo.utils.createAvatar()` 或 `Dambo.utils.renderContent(el, item)`。
6. **OBS 展示端与管理端职责分离**：
   - `plugin.html` 用于直播叠加，必须保持 `html, body { background: transparent !important; overflow: hidden; }`、**零点击自动启动**（不放设置按钮、弹窗或 `:hover` 菜单），且**避开右上角 `100px × 44px`**（桌面悬浮窗预览模式下宿主会在此处显示控制条）。
   - 如果需求只需要调整配置、查看数据或管理跨插件 ACL 权限，**无需编写 `manage.html`**——在 `plugin.json` 中声明 `defaults` 即可自动由宿主渲染内置通用管理页 `/{plugin-name}/m`。仅当需求包含定制交互按钮（如“手动加减分 / 抽签 / 切题 / 清零面板”）时才生成 `manage.html` + `manage.css` + `manage.js`。

---

## 2. 目录结构、路由与 `plugin.json` 规范

### 2.1 插件目录与识别规则
插件放在 `static/html/<plugin-name>/`（支持约 200ms 自动热重载）。`<plugin-name>` 必须匹配 `^[A-Za-z0-9_-]{1,64}$`，且不得与保留顶层路由冲突（`ws`、`api`、`sdk`、`admin`）。

```text
static/html/<plugin-name>/
├── plugin.html     # [必需] OBS 展示端（不再兼容旧 index.html），映射 GET /{plugin-name}/（默认挂件示例 http://localhost:3838/default/）
├── plugin.json     # [推荐] 清单、OBS 建议尺寸与强类型默认配置 defaults
├── plugin.css      # [可选] 展示端样式
├── plugin.js       # [可选] 展示端脚本
├── manage.html     # [可选] 自定义管理端，映射 GET /{plugin-name}/m（缺省由宿主自动渲染兜底管理页）
├── manage.css      # [可选] 管理端样式
├── manage.js       # [可选] 管理端脚本
└── assets/         # [可选] 本地图片/字体/音效等静态资源
```

### 2.2 `plugin.json` 与 `defaults` 强类型定义
`plugin.json` 运行期只读（**不包含 `permissions` 字段**，跨插件权限通过运行期 `Dambo.acl` 申请并在管理页审批）：

```json
{
  "plugin_id": "gift-goal",
  "version": "1.0.0",
  "name": "礼物电池目标进度条",
  "description": "统计直播间礼物并实时展示目标进度，支持管理页自定义目标与配色",
  "obs": {
    "width": 460,
    "height": 180
  },
  "defaults": {
    "goal_cny": {
      "type": "number",
      "value": 100,
      "meta": { "min": 1, "max": 100000, "step": 10, "format": "integer", "label": "目标金额（元）" }
    },
    "title": {
      "type": "string",
      "value": "今日电池目标",
      "meta": { "max_length": 40, "label": "进度条标题" }
    },
    "accent_color": {
      "type": "string",
      "value": "#38bdf8",
      "meta": { "format": "color", "label": "主题高亮色" }
    },
    "theme": {
      "type": "enum",
      "value": "dark",
      "meta": { "options": ["dark", "light", "transparent"], "label": "卡片风格" }
    },
    "show_recent": {
      "type": "bool",
      "value": true,
      "meta": { "label": "显示最近感谢" }
    }
  }
}
```

- `plugin_id`（默认 `<plugin-name>`）与 `version` 主版本号组成持久化归属域 `namedomain = "{plugin_id}@{major}"`；同一大版本下小升级自动延续持久化数据与 ACL。
- `defaults` 支持以下 **6 种强类型**（可用于 `defaults` 及 `storage`/`memory` 条目）：
  - `number`：支持 `meta.min`、`meta.max`、`meta.step`、`meta.format`（`"integer"` | `"slider"`）
  - `string`：支持 `meta.max_length`、`meta.placeholder`、`meta.format`（`"color"` | `"textarea"` | `"url"`）
  - `bool`：布尔开关
  - `enum`：**必须**声明 `meta.options`（`string[]` 或 `[{ "value": "...", "label": "..." }]`）
  - `list`：数组，支持 `meta.item`（嵌套类型定义）、`meta.max_items`
  - `struct`：对象，支持 `meta.fields`（按字段嵌套类型，支持 `optional: true` / `nullable: true`；缺省则为自由 JSON 对象）
- 所有 `defaults` 项还支持 `meta.label` 与 `meta.description` 供内置管理页渲染人类可读表单。

---

## 3. 直播间事件流 SDK（`Dambo.connect` & `Dambo.utils`）

### 3.1 连接选项与事件生命周期
```javascript
const client = Dambo.connect({
    channel: 'feed',                 // 'feed' (对应 /ws，OBS 挂件首选) | 'event' (对应 /ws/event，聚合/结算流)
    url: Dambo.utils.getQueryParam('ws') || undefined,
    autoReconnect: true,             // 默认 true：带 ±15% 抖动的指数退避重连，切后台再唤醒自动恢复
    replaySnapshot: false,           // 默认 false；设为 true 时收到 Snapshot 会逐条回放至 'event' 和具体 kind 回调
    showLike: null,                  // 设为 false 自动忽略 kind==='like'；推荐留 null 并配合 config 动态判断
    onReset: (snapshot) => {},       // 每次收到 Snapshot 时、重放历史前触发，适合清理旧 DOM 与定时器
});
```

### 3.2 事件名与载荷结构
- **Channel A（`channel: 'feed'`）监听事件**：
  - `snapshot` (`SnapshotData`)：`{ room_id, room_uname, room_title, is_live, connected, items: SnapshotItem[] }`（仅在重连/切房/滞后恢复时触发；`items` 环形缓存保留最近 `chat`/`gift`/`superchat`/`guard`）
  - `room_state` (`RoomStateSnapshot`)：`{ room_id, room_uname, room_title, is_live, connected }`
  - `event` (`item, meta`) 与具体 `kind` 名回调：其中 `meta = { isSnapshot: boolean }`；`kind === 'super_chat'` 会同时触发 `'super_chat'` 与 `'superchat'`。
  - **支持的所有 `item.kind`**：`'chat'` | `'gift'` | `'superchat'` | `'super_chat'` | `'guard'` | `'guard_enter'` | `'enter'` | `'follow'` | `'special_follow'` | `'mutual_follow'` | `'share'` | `'like'` | `'live'` | `'preparing'` | `'room_change'` | `'room_silent'` | `'room_block'` | `'superchat_delete'` | `'warning'` | `'cut_off'` | `'reconnected'`
  - **`SnapshotItem` 核心字段**：
    - `id`: 环形缓冲区单调 ID（去重用）
    - `seq`: 场次序列号
    - `kind`: 事件类型
    - `msg_type?`: `INTERACT_WORD*` 子类型（1 进房, 2 关注, 3 分享, 4 特关, 5 互粉）
    - `uid`, `uname`, `face?`, `medal_name?`, `medal_level?`
    - `content`: 文本内容（如弹幕正文、`"小心心 x2"`、SC 内容等）
    - `emoticon?`: 大表情贴图 URL；`emots?`: 行内表情字典 `{ "[dog]": "https://..." }`
    - `price`: **金瓜子**数额（`1000` 金瓜子 = `¥1` 人民币；免费礼物/普通事件为 `0`）
    - `num?`: 礼物数量或大航海开通月数
    - `guard_level?`: `1` 总督, `2` 提督, `3` 舰长
    - `ids?`: 被撤回的 SC ID 数组（`kind === 'superchat_delete'`）
    - `batch_combo_id?`, `combo_stay_time?`: 原生礼物连击批次与停留秒数
    - `timestamp`: 13 位 Unix 毫秒时间戳
- **Channel B（`channel: 'event'`）监听事件**：
  - `room_info` (`RoomInfoEvent`)：完整房间信息（`room_id`, `short_id`, `title`, `area_name`, `live_status`, `start_time`）
  - `batch` (`EventBatchData`) / `batch_item` (`ev, batch`)：`priority` 为 `0`（周期聚合）、`1`（直通/连击结算）、`2`（关键状态/SC/上舰）；`ev` 包含 `count` 聚合数与 `sample_users`（最多 5 个代表昵称），且**仅付费事件保留 `face`**。

### 3.3 `Dambo.utils` 高频内置工具函数
生成插件时优先复用 SDK 内置工具，避免重复手写易错逻辑：

| 方法 | 用途与返回 |
| --- | --- |
| `Dambo.utils.isSuperChat(itemOrKind)` | 同时兼容 `'superchat'` 与 `'super_chat'` |
| `Dambo.utils.isPaidEvent(item)` | 判断 SC、大航海或 `price > 0` 的付费礼物 |
| `Dambo.utils.formatPrice(priceSeeds, symbol='¥')` | 金瓜子（`1000 = ¥1`）转格式化金额（如 `'¥30'`），`<= 0` 返回 `''` |
| `Dambo.utils.formatGift(item)` | 提取 `{ giftName, giftId, batchComboId, coinType: 'gold'|'silver', isPaid, num, comboNum, totalCoin, comboTotalCoin, unitPrice, priceSeeds, comboStayTimeMs, mergeKey }` |
| `Dambo.utils.formatGuard(item)` | 清洗 `<%...%>` 等冗余文案，返回 `{ guardLevel, guardName, badgeLabel, num, monthsSuffix, actionText, highlightText, summaryText }` |
| `Dambo.utils.formatInteract(item)` | 返回 `{ kind, msgType, guardLevel, guardName, actionText, summaryText }` |
| `Dambo.utils.createGiftComboTracker(opts)` | `4000ms` 滑动窗口连击归并器（`{ windowMs, useServerComboStayTime, canMerge, onCreate, onUpdate, onExpire }`），提供 `.push(item, { isSnapshot })`, `.expire(key)`, `.pruneExpiredSnapshots()`, `.clear()`, `.size()` |
| `Dambo.utils.createAutoScroll(containerEl, opts)` | 列表自动置底控制器（上滑历史自动锁屏保护），提供 `.schedule(forceOpts?)`, `.isFollowing()`, `.isNearBottom()`, `.reset()`, `.destroy()` |
| `Dambo.utils.createTimerBag()` | 受控定时器池，提供 `.set(fn, delayMs)`, `.clear(id)`, `.clearAll()`, `.size()` |
| `Dambo.utils.superChatDurationMs(item)` | 按金额返回推荐停留毫秒数（`guard`: 90s; `<100`: 60s; `>=100`: 120s; `>=500`: 180s; `>=1000`: 300s） |
| `Dambo.utils.remainingDurationMs(item, totalMs, opts)` | 计算快照回放剩余时长，`<= 0` 表示已过期 |
| `Dambo.utils.createAvatar(faceOrItem, className)` | 创建带懒加载、HTTPS 升级与内置 SVG 兜底头像的 `<img>` |
| `Dambo.utils.renderContent(parentEl, item, opts)` | 安全渲染正文、`emoticon` 大表情与 `emots` 行内表情（零 `innerHTML`） |
| `Dambo.utils.formatRoomBadge(roomState)` | 格式化房间状态标签文本 |
| `Dambo.utils.getQueryParam(name, fallback)` / `getQueryBool(name, fallback)` | 安全读取 URL Query 参数（常用于覆盖 `defaults`） |
| `Dambo.utils.normalizeUrl(url)` / `guardLevelName(level)` | 协议升级为 HTTPS / 等级转中文 |

---

## 4. 数据存储、配置同步、订阅源与权限控制（`Dambo` v2 Platform API）

### 4.1 冷热数据选型映射（对照《需求确认单》）
- **需求里“自动记住进度 / 设置 / 永久保留”** → 使用 `Dambo.config`（配置项，读写底层自动合并 `plugin.json` `defaults`）或 `Dambo.storage`（业务持久化命名空间，保存在 `var/dambo.redb`，重开/下播不丢）。
- **需求里“刷新 OBS 或重启后自动清零”** → 使用内存 JS 变量（仅单页面展示用）或 `Dambo.memory`（需要 `plugin.html` 与 `manage.html` 共享、但重启清零的状态）。
- **展示端 ↔ 管理端实时同步**：在展示端调用 `Dambo.config.watch(...)`、`Dambo.storage.watch(ns, cb, opts?)` 或 `Dambo.memory.watch(ns, cb, opts?)`；管理端写入后，服务端通过懒连接的单一 SSE（`/api/v1/events`）实时推送到所有打开的展示端与管理端。

### 4.2 核心 API 精确签名（`window.Dambo` v2.0.0）

```javascript
// 1. 当前插件上下文与发现
Dambo.plugin.name                            // 目录名 plugin_name
Dambo.plugin.mode                            // 'view' | 'manage'
await Dambo.plugin.meta({ force?: boolean }) // { plugin_name, plugin_id, version, namedomain, defaults, config, topics, ... }
await Dambo.plugins.list()
await Dambo.plugins.get(target)

// 2. 强类型配置（底层等价 persist 下的 "config" 命名空间，已合并 defaults）
await Dambo.config.get()                     // 返回扁平键值对象 { key: value, ... }
await Dambo.config.get('goal_cny')           // 返回单个配置值
await Dambo.config.set('goal_cny', 200)      // 修改单项并持久化
await Dambo.config.set({ goal_cny: 200, title: '新目标' }) // 原子批量修改
await Dambo.config.reset('goal_cny')         // 恢复单项默认（不传参数恢复全部 defaults）
await Dambo.config.schema()                  // 返回完整 { key: { type, value, meta, is_overridden } }
const offCfg = Dambo.config.watch((ev) => {})

// 3. 持久化存储 (Dambo.storage) 与 内存存储 (Dambo.memory) —— API 完全对称
await Dambo.storage.namespaces()             // string[]
await Dambo.storage.get(ns, key)             // KEY_NOT_FOUND 返回 undefined；其他错误抛 DamboError
await Dambo.storage.getEntry(ns, key)        // { key, type, value, meta? }；KEY_NOT_FOUND 返回 null
await Dambo.storage.set(ns, key, value, schema?) // 可选 schema: { type, meta? }（仅自身可定义 schema；顶层值严禁传 null）
await Dambo.storage.delete(ns, key)          // 返回 boolean（删除键请用 delete，不要写 null）
await Dambo.storage.list(ns, { prefix?, keys?, limit?, withSchema? }) // 默认返回 { key: value }；withSchema: true 返回完整信封
await Dambo.storage.mset(ns, { k1: v1, k2: v2 }) // 单次最多 256 个条目；persist 下单事务原子提交
await Dambo.storage.clearNamespace(ns)       // 返回删除条数
const offWatch = Dambo.storage.watch(ns, (ev) => {
    // ev: { store, owner, namespace, op: 'set'|'delete'|'mset'|'clear', keys: string[], values?: object, by, ts }
    // 注意：当次变更序列化 <= 16KB 时直接包含 ev.values[k]，可直接读取无需二次 get()
}, { prefix?: string })

// 跨插件操作句柄（target 可传 plugin_id / plugin_name / namedomain）
const otherStorage = Dambo.storage.of('target-plugin')
const otherMemory  = Dambo.memory.of('target-plugin')

// 4. 插件归属订阅源（一对多实时广播，不落盘历史消息，元数据持久化）
await Dambo.topics.publish(name, payload, { public?: boolean, description?: string }) // 默认 public: false
await Dambo.topics.configure(name, { public: boolean, description?: string })
await Dambo.topics.remove(name)
await Dambo.topics.list(target?)             // 默认 '@self'
const offTopic = Dambo.topics.subscribe(target, name, (msg) => {
    // msg: { owner, topic, payload, ts }
})

// 5. 统一跨插件权限控制（ACL）
const perm = await Dambo.acl.check(target, resource) // resource 示例: 'persist:scores' | 'memory:cmd' | 'topic:round' | 'persist:*'
await Dambo.acl.request(target, { resource, level: 'read'|'write'|'subscribe', reason })
const { incoming, outgoing } = await Dambo.acl.list()
await Dambo.acl.set(requesterNamedomain, { resource, level: 'read'|'write'|'subscribe'|'none', note? }) // 仅管理页 /m 可调
await Dambo.acl.remove(requesterNamedomain, resource) // 仅管理页 /m 可调
const offAcl = Dambo.acl.onChange((ev) => {})

// 6. 远程管理页解锁（仅当用户配置了 [server].admin_token 且从非本机访问 /{plugin}/m 遇 401 时调用）
await Dambo.plugin.unlockManage(token)
```

### 4.3 约束、配额与鉴权注意点
- **标识符字符集**：
  - `namespace` 与 `topic` 仅允许 `[A-Za-z0-9_.-]`，长度 `1..=128` 字节；
  - `key` 长度 `1..=128` 字节，禁止控制字符、空字节与 `/`；
  - **顶层值禁止写入 `null`**（`DELETE` / `delete()` 用于删除键）。
- **配额限制**：单条值 JSON 序列化 `<= 256 KB`；`mset` 单批最多 `256` 条；单 `namedomain` 在 `persist` 或 `memory` 各自上限 `10,000` 键 / `16 MB`；超限抛出 `Dambo.DamboError`（`code === 'QUOTA_EXCEEDED'`）。
- **管理态接口边界**：在普通展示页（`/{plugin-name}/`）中请勿自动调用管理态审批 API（`Dambo.acl.set` / `Dambo.acl.remove`），这些写操作仅应在管理页（`/{plugin-name}/m`）触发；普通 `Dambo.config.set` / `Dambo.storage.set` / `Dambo.memory.set` 在本机与授权浏览器均可正常使用（若遇 `UNAUTHORIZED`，自定义 `manage.html` 应提示输入 `admin_token` 调用 `Dambo.plugin.unlockManage(token)`）。
- **优雅错误与销毁清理**：除 `get()`/`getEntry()` 对 `KEY_NOT_FOUND` 返回 `undefined`/`null` 外，其余接口失败均拒绝并抛出 `Dambo.DamboError`（含 `code`、`status`、`message`）；当动态注销全部 `watch`/`subscribe`/`onChange` 返回的 `off()` 函数后，SDK 会自动关闭闲置的 SSE 连接。

---

## 5. 标准代码骨架与交付清单

### 5.1 标准代码骨架（含配置监听 + 持久进度 + 实时弹幕）

当用户确认“**使用默认管理页**”时，只需生成 `plugin.json`、`plugin.html`、`plugin.css`、`plugin.js`；若确认需要“**定制后台操控面板**”，再额外生成 `manage.html`、`manage.css`、`manage.js`。

`plugin.html` 标准头部与脚本加载顺序：
```html
<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <meta name="referrer" content="no-referrer" />
  <base href="/<plugin-name>/" />
  <title>插件标题</title>
  <link rel="stylesheet" href="./plugin.css" />
</head>
<body>
  <main id="app" class="app"></main>
  <script src="/sdk/dambo.js"></script>
  <script src="./plugin.js"></script>
</body>
</html>
```

展示端与配置/存储联动标准写法（`plugin.js`）：
```javascript
(function () {
    'use strict';

    let cfg = {};
    const timers = Dambo.utils.createTimerBag();

    function readConfigWithQueryOverrides(rawCfg) {
        const titleOverride = Dambo.utils.getQueryParam('title');
        return {
            ...rawCfg,
            title: titleOverride !== null ? titleOverride : rawCfg.title,
        };
    }

    async function loadAndApplyConfig() {
        try {
            const raw = await Dambo.config.get();
            cfg = readConfigWithQueryOverrides(raw);
            render();
        } catch (err) {
            console.warn('[<plugin-name>] config load fallback:', err);
        }
    }

    // 1. 监听管理页修改实时热更新配置
    Dambo.config.watch(loadAndApplyConfig);

    // 2. 若需从持久化/内存命名空间跨页面实时同步进度：
    Dambo.storage.watch('state', (ev) => {
        if (ev.values && Object.prototype.hasOwnProperty.call(ev.values, 'total_seeds')) {
            totalSeeds = Number(ev.values.total_seeds || 0);
            render();
        } else {
            syncStateFromStore();
        }
    });

    // 3. 连接直播间事件流（注意 onReset/snapshot 时重置瞬态缓存）
    const client = Dambo.connect({
        channel: 'feed',
        url: Dambo.utils.getQueryParam('ws') || undefined,
        onReset: () => {
            timers.clearAll();
        },
    });

    client.on('gift', (item, meta) => {
        if (meta && meta.isSnapshot) return; // 历史回放通常不重复累加进度，视具体需求决定
        const g = Dambo.utils.formatGift(item);
        if (!g.isPaid) return;
        // 安全更新 UI & 按需写入 storage/memory
    });

    loadAndApplyConfig();
})();
```

> 若自定义 `manage.html`：`<head>` 中**不要**写 `<meta name="referrer" content="no-referrer">`；访问 `/{plugin-name}/m` 时相对路径 `./manage.css` 与 `./manage.js` 会自然解析为 `/{plugin-name}/manage.css` 与 `/{plugin-name}/manage.js`（也可同样加上 `<base href="/<plugin-name>/">`），脚本加载顺序仍为先 `<script src="/sdk/dambo.js"></script>` 后 `<script src="./manage.js"></script>`。

### 5.2 AI 交付前自检与给用户的操作提示

生成完整文件后，AI 应自行核对以下 5 项再输出给用户：
1. 确认所有文件完整输出，目录名 `<plugin-name>` 仅含 `[A-Za-z0-9_-]` 且不为 `ws`、`api`、`sdk`、`admin`。
2. 确认 `plugin.html` 含 `<meta name="referrer" content="no-referrer">` 与 `<base href="/<plugin-name>/">`，且自定义 `manage.html`（如有）**不含** `no-referrer`。
3. 确认全部 `.html` / `.js` 中**零 `.innerHTML`**、**零硬编码主机端口**、**`<script src="/sdk/dambo.js"></script>` 同步加载且在插件脚本之前**，且除 `/sdk/dambo.js` 外，所有插件本地 `.css`/`.js`/静态资源均为 `./...` 相对路径（不存在 `/plugin.js`、`/manage.js`、`/static/html/...` 等绝对路径）。
4. 确认 `plugin.json` JSON 语法合法，未包含 `permissions` 字段，且展示端保持透明无遮挡、避开右上角 `100×44px`。
5. 在回答最后附上清晰的 3 步使用指引：
   - 在 Dambo 控制舱点击 **插件 -> 打开插件目录**（即 `static/html/`），新建 `<plugin-name>` 文件夹并将生成的全部文件放入其中；
   - 无需重启 Dambo（约 200ms 自动识别），点击 **桌面打开** 或 **复制 OBS URL**（格式为 `http://localhost:3838/<plugin-name>/`，内置默认挂件示例为 `http://localhost:3838/default/`）添加到 OBS「浏览器」源；
   - 在统一控制面板 `http://localhost:3838/` 点击 **设置 / 玩法**（开发者视图下点击 **管理**，或直接访问 `http://localhost:<port>/<plugin-name>/m`）即可实时调整配置、审批跨插件权限或操作自定义管理面板。
