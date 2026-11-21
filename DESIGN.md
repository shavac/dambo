# `dambo` — 产品需求规格与系统架构设计文档（PRD）

## 1. 项目概述与生态定位

**`dambo`**（Danmaku + Event + Bot）是本地 AI 直播助手生态中的**B站直播间弹幕采集、同构录制与智能事件整形节点**，使用 Rust 编写。在工程结构、配置规范与网络服务范式（基于 `tokio` + `axum`）上，与同生态语音识别节点 **`robabel`（默认端口 `3737`）** 保持高度对称。

### 三端职责边界
* **`robabel`（`:3737`）**：纯听觉感知节点，输出实时 ASR 语音流（`/ws`）与 OBS 透明语音字幕（`/`）。
* **`dambo`（`:3838`）**：纯互动感知与流量整形节点，采用「单点采集定序 -> 全量 `/ws` + 同构录制 `recorder` + 智能聚合 `/ws/event`」极简流水线，负责单直播间长连采集、全量事件同构落盘、可扩展 OBS 网页插件（`/`、`/{plugin}`）以及面向中控的事件聚合流（`/ws/event`）。
* **`robao`（中控大脑）**：有状态决策中枢，同时订阅 `3737/ws` 与 `3838/ws/event`，负责双流毫秒级时间对齐、观众生命周期状态机（进场/潜水/破冰/离场/回流）、长期记忆存储、防抢话仲裁与 LLM/TTS 回复生成。

> **架构红线**：`dambo` 保持**无状态边缘管道**定位，不在内部维护观众生命周期状态机，也不包含任何 LLM 推理逻辑；底层配置与传输协议统一以纯数字 `room_id` 作为房间唯一标识。

---

## 2. Workspace 目录与工程结构

采用虚拟 Cargo Workspace 组织，将核心库、CLI、桌面端前端、Tauri Rust 宿主与 OBS 网页主题平级拆分，消除路径嵌套冲突与文件监听干扰：

```text
dambo/
├── Cargo.toml                  # Virtual workspace root manifest (Edition 2024)
├── DESIGN.md                   # Product requirements & architecture design doc
├── etc/
│   └── config.toml             # Strictly read-only static configuration template
├── var/                        # Runtime artifacts (gitignored: state, auth session, logs, records)
│   ├── state.json              # Runtime state (current_room_id, locale, room_history[<=20], record_raw)
│   ├── session.json            # Persisted Bilibili auth cookies
│   ├── logs/                   # Rolling application logs
│   └── records/                # Isomorphic *.jsonl & optional *_raw.jsonl captures
├── dambo-core/                 # Core shared library (used by both CLI and Tauri UI)
│   ├── Cargo.toml              # Depends on axum (ws), notify, tokio, etc.
│   └── src/
│       ├── lib.rs              # DamboRuntime isomorphic pipeline dispatcher (read-only Arc<Config> + StateStore + PluginRegistry)
│       ├── config.rs           # Strictly read-only TOML configuration loading ([event], [server].state_file, [record].record_raw)
│       ├── state.rs            # Fault-tolerant atomic JSON persistence for var/state.json (StateStore)
│       ├── plugin.rs           # PluginRegistry: auto-discovery, plugin.json manifest & notify recursive hot-reload
│       ├── model.rs            # RoomInfoSnapshot, RawBiliEvent (seq, msg_type), SessionSummary, Channel A/B contracts
│       ├── auth.rs             # Bilibili cookie validation, QR generation, and polling FSM
│       ├── collector.rs        # Single-room collector, seq sequencing, guard dedup & PB decoder
│       ├── recorder.rs         # Isomorphic JSONL recorder, session_end summary & *_raw.jsonl sidecar
│       ├── hub.rs              # Channel A: Real-time broadcast & protected Snapshot ring buffer (/ws)
│       ├── event_engine.rs     # Channel B: 3-lane EventEngine & dynamic combo buckets (/ws/event)
│       └── server.rs           # Axum HTTP unified plugin routing (/, /{plugin}, /{plugin}/{*path}) & dual WS (/ws, /ws/event)
├── cli/                        # Headless CLI entrypoint ([[bin]] name = "dambo", supports --record-raw)
│   ├── Cargo.toml
│   └── src/main.rs
├── ui/                         # Tauri desktop frontend assets (control capsule, Plugin dropdown & QR login popup)
│   ├── index.html              # Main floating control capsule
│   ├── login.html              # Standalone QR code login sub-window
│   ├── style.css
│   └── app.js
├── src-tauri/                  # Tauri v2 Rust host & system tray (name = "dambo-ui")
│   ├── Cargo.toml
│   ├── tauri.conf.json         # Sets "frontendDist": "../ui", default width 780
│   ├── capabilities/default.json
│   ├── locales/                # Trilingual desktop host i18n dictionaries (zh-CN, ja, en)
│   │   ├── zh-CN.json
│   │   ├── ja.json
│   │   └── en.json
│   └── src/
│       ├── main.rs
│       ├── i18n.rs             # Embedded trilingual lookup, placeholder formatting & system locale detection
│       └── lib.rs
└── static/
    └── html/                   # HTTP server (:3838) self-contained web plugins (auto-discovered by PluginRegistry)
        ├── default/            # Default transparent OBS danmaku overlay (served at / and /default, skips like by default)
        │   ├── plugin.json
        │   ├── index.html
        │   ├── style.css
        │   └── danmu.js
        ├── list/               # Optional streamer web danmaku feed (served at /list, skips like by default)
        │   ├── plugin.json
        │   ├── index.html
        │   ├── style.css
        │   └── feed.js
        └── sc-ticker/          # OBS plugin showing high-priority SuperChat & Guard events only (served at /sc-ticker)
            ├── plugin.json
            ├── index.html
            ├── style.css
            └── ticker.js
```

---

## 3. 核心功能需求（PRD）

### 3.1 账号鉴权与原生扫码弹窗

* 启动无感验活：启动时读取 `session_file`，携带 Cookie 请求 `https://api.bilibili.com/x/web-interface/nav` 校验 `data.isLogin`；有效则直接进入工作态，零弹窗打扰。
* 原生置顶扫码子窗口（`dambo-ui`）：凭证缺失或过期时，后端动态创建独立置顶子窗口 `bili-qr-login`（320×380，加载 `ui/login.html`），请求 `passport.bilibili.com/x/passport-login/web/qrcode/generate` 生成矢量 SVG 二维码推送至前端；每 2 秒轮询状态（二维码过期自动刷新），手机确认后保存 `SESSDATA`、`bili_jct`、`DedeUserID` 至本地并自动销毁扫码子窗口。
* CLI 兜底（`dambo`）：无桌面环境时直接在终端打印 Unicode 字符二维码完成登录。
* 账号切换：支持在控制面板点击账号徽章或通过托盘菜单一键登出并重新触发扫码流程。

### 3.2 单房间采集、定序与大航海单源去重（Collector）

* **房间与主播完整元数据（`kind: "room_info"`）**：
  * 建连初始化阶段调用 B 站接口 `GET https://api.live.bilibili.com/room/v1/Room/get_info?room_id=<room_id>`（并补充获取主播昵称 `uname`），构建共享 `RoomInfoSnapshot`：
    * `room_id`（真实长号）、`short_id`（短号，无则为 `0`）、`uid`（主播 UID）、`uname`（主播昵称）
    * `title`（标题）、`area_name`、`parent_area_name`、`live_status`（`1` 直播中 / `0` 未开播 / `2` 轮播中）
    * `start_time`（本场开播 Unix 秒级时间戳；仅 `live_status == 1` 且 `live_time` 非 `"0000-00-00 00:00:00"` 时按 `Asia/Shanghai` UTC+8 解析填充）。
  * 在建连完成、收到 `LIVE` 开播或 `ROOM_CHANGE` 变更时同步更新该快照。
* **场次内单调递增序号（`seq: u64`）**：
  * 在 `collector` 内部维护场次自增 `seq`：`room_info` 固定为 `0`，后续每条对外发布的标准事件依次递增（`1, 2, 3...`），新场次轮转时重置为 `0`。
* **大航海单源去重与零风控头像缓存**：
  * 废除独立过滤层，改为在采集归一化阶段直接完成单源防重：跳过冗余的 `GUARD_BUY` 指令（若其携带头像则先预热缓存），仅解析 `guard_level > 0` 的 `USER_TOAST_MSG` / `USER_TOAST_MSG_V2`（映射为 `kind: "guard"`），并使用容量 256 的内存 LRU 集合按 `payflow_id` 做幂等去重（严禁以 `price` 关联同一性）。
  * 维护容量 `avatar_cache_size`（默认 2,000）的内存 LRU 缓存 `uid -> face_url`，严禁反查 HTTP 用户接口；触发无头像互动时自动补全。
* **完整互动、连击与生命周期提取**：
  * 保留全部 `like`（点赞）与 `guard_enter`（`ENTRY_EFFECT`）；
  * 解析 `INTERACT_WORD` 与 `INTERACT_WORD_V2`（Protobuf 解包 `uid`、`uname`、`fans_medal`、`uinfo` 头像及原始 `msg_type`）并一一独立映射为标准 `kind`（同时保留原始 `msg_type` 字段）：
    * `msg_type == 1` -> `enter`（进房）
    * `msg_type == 2` -> `follow`（关注）
    * `msg_type == 3` -> `share`（分享直播间）
    * `msg_type == 4` -> `special_follow`（特别关注）
    * `msg_type == 5` -> `mutual_follow`（互粉）
  * 在 `SEND_GIFT` / `SEND_GIFT_V2` 提取 `num`、`batch_combo_id` 与 `combo_stay_time`；
  * 将 `COMBO_SEND` / `GIFT_COMBO` 转化为轻量内部结算信号 `kind: "combo_end"`（携带 `batch_combo_id`，不分配 `seq`，不进 `recorder` 与 `/ws`，仅投递 `EventEngine` 提前结束连击桶）；
  * 提取 `live`、`preparing`（区分 `round == 1` 的轮播状态）、`room_change`、`superchat_delete`（含 `ids`）、`room_silent`、`room_block`、`warning`、`cut_off`。
* **旁路原始抓包与 PB 自动解包（`record.record_raw`）**：
  * 仅当 `record.record_raw == true`（或 CLI `--record-raw`）时，将原始 B 站 JSON 节点中的 Base64 `"pb"` / `"dm_v2"` 自动解包还原为同级 `"pb_decoded"` / `"dm_v2_decoded"` 明文 JSON 写入 `*_raw.jsonl`；默认 `false` 时完全跳过解码与文件创建开销。

### 3.3 同构录制与场次连续性保护（Recorder）

* **同构事件流与付费头像保留策略**：`Recorder` 直接订阅 `collector` 产出的标准事件流写入主 `.jsonl`（完整保留 `like`、`chat`、`gift`、`superchat`、`guard`、`guard_enter`、`enter`、`follow`、`share`、`special_follow`、`mutual_follow` 及生命周期事件，不含内部信号 `combo_end`），确保录制文件与 `/ws/event` 源数据 100% 同构。其中 `face` 头像 URL **仅在付费事件（`superchat`、`guard` 以及 `price > 0` / `coin_type == "gold"` 的付费 `gift`）中保留落盘**，其余非付费互动与免费礼物（`price == 0`）一律清空 `face` 并在 JSON 序列化时自动省略。
* **首行元事件与 `offset_ms` 锚定**：
  * 新建或轮转文件首行固定写入 `kind: "room_info"`（`seq = 0`）；若建连时已处于直播中（`live_status == 1` 且 `start_time` 有效），则将 `offset_ms` 基准锚定为 `start_time * 1000`。
* **场次指纹与断线续写**：
  * 以 `(room_id, start_time)` 为场次指纹，网络闪断重连后若 `live_status == 1` 且 `start_time` 未变，则**继续追加原文件不切分**，仅发射一条 `kind: "reconnected"` 事件并保持 `seq` 单调递增。
* **场次结束汇总（`session_end`）**：
  * 收到 `preparing` 下播或进程退出（`SIGINT`/`SIGTERM`）前，向当前 `.jsonl` 末尾追加写入 `kind: "session_end"` 汇总元事件（统计 `chat_count`、`like_count`、`guard_count`、`gift_total_coin`、`superchat_total_coin`、`enter_count`、`follow_count`〔含普通关注/特关/互粉〕、`share_count`）并强制 `flush`。

### 3.4 基于 axum 的双通道输出架构（端口 3838）

`server.rs` 使用 `axum` 搭建异步 HTTP 与双 WebSocket 服务（共享 `Arc<AppState>`，注入只读 `Arc<Config>` 与热加载 `Arc<PluginRegistry>`），无独立 `filter.rs` 丢弃环节：

#### 通道 A：可扩展网页插件与实时直通流（`http://<host>:3838/`、`/{plugin}` & `ws://<host>:3838/ws`）

* **插件自动发现与 `notify` 热重载（`plugin.rs`）**：扫描 `config.server.html_dir`（默认 `static/html/`）的一级子目录；仅接纳包含 `index.html` 且目录名合法（仅含 ASCII 字母、数字、`-`、`_`，并排除 `ws`、`api`、`sdk` 等保留路由名）的插件；解析可选清单 `plugin.json`（支持 `title`、`description` 与 `obs.{width, height}`，缺失或格式错误时记录 `warn` 日志并安全回退）；按内置插件（`default`、`list`）优先、其余按目录名字母序排列；通过 `notify` 递归监听（约 200ms 防抖）文件与目录增删改，实时刷新内存注册表并广播更新通知，无需重启服务。
* `GET /`：统一提供默认插件 `static/html/default/index.html`。
* `GET /{plugin}` 与 `GET /{plugin}/`：返回对应插件目录 `static/html/{plugin}/index.html`（内置含 `/default`、`/list`、`/sc-ticker`，第三方放入目录即生效）。
* `GET /{plugin}/{*path}`：严格拦截 `..` 路径穿越；解析插件静态资产并兼容根路径相对引用向 `default/` 目录回退；统一注入 `Cache-Control: no-store, no-cache, must-revalidate` 响应头，防止 OBS 浏览器源缓存旧资产。
* `GET /ws`：实时向前端推送所有标准事件（**全量保留 `face` 头像供 OBS 视觉挂件渲染**，含 `like`、`enter`、`follow`、`share`、`special_follow`、`mutual_follow` 等；内置插件默认跳过 `like` 渲染，可通过 `?show_like=1` 开启），并在 `Snapshot` 环形缓冲区（`feed.max_history`，默认 300）中**仅保留核心视觉事件（`chat`、`gift`、`superchat`、`guard`）**，跳过高频 `enter` 与 `like`，防止刷新挂件时历史视觉消息被挤没。

#### 通道 B：robao 中控智能事件整形流（`ws://<host>:3838/ws/event`）

* **建连首帧**：每当客户端连接 `/ws/event` 时，`server.rs` 立即下发当前最新的 `ChannelBMessage::RoomInfo` 元事件作为首帧。
* **`EventEngine` 三车道智能分流与连击桶**：
  1. **零延迟直通车道（0ms）**：
     * 关键状态与高优互动（`priority = 2`）：`live`、`preparing`、`room_change`、`room_silent`、`superchat_delete`、`room_block`、`warning`、`cut_off`、`reconnected`、`superchat`、`guard`；
     * 社交互动、优质文本弹幕与非连击礼物（`priority = 1`）：`enter`、`follow`、`share`、`special_follow`、`mutual_follow`、`guard_enter`、非连击 `gift`（`combo_stay_time == 0 || batch_combo_id == None`），以及 `dm_type == 0` 且剥离 `[表情名]` 与空白后的有效字符数 `>= min_passthrough_chars`（默认 4）的 `chat`（上述 5 类 `INTERACT_WORD*` 互动均保持单条原始颗粒度直通，不合并）。
  2. **高频噪音与短句聚合车道（`window_ms = 2500ms` `Reservoir`，`priority = 0`）**：
     * `like` 按类型合并计数；
     * 纯表情弹幕（`dm_type == 1` 或剥离 `[xxx]` 后为空）及超短弹幕（有效字符数 `< min_passthrough_chars`）经大小写与连续重复字符归一化后合并计数，到期输出 `EventBatch`。
  3. **动态礼物连击聚合桶（按 `batch_combo_id` 聚合，`priority = 1`）**：
     * 当 `combo_stay_time > 0` 且 `batch_combo_id` 有效时，以 `batch_combo_id` 为键累加 `num` 与总价值，截止时间设为 `min(now + combo_stay_time, first_seen_at + max_combo_wait_secs)`；收到匹配的 `combo_end` 信号时提前刷出并销毁桶，到达截止时间未结束时自动兜底刷出。
* **背压保护与头像精简**：有界队列 `buffer_capacity = 512`，队满优先淘汰低价值 `like` / 无牌 `enter`，其次短弹幕，绝不丢弃关键状态/SC/上舰/连击结算信号；输出至 `/ws/event` 时，**仅付费事件（`superchat`、`guard` 及 `price > 0` 的付费 `gift`）保留 `face` 头像 URL**，非付费事件与免费礼物一律剥离 `face`，保留 `seq`、`msg_type`、`uid`、`uname`、`medal_level` 等结构化字段。

---

## 4. 界面与交互设计（`dambo-ui`）

1. 主控制浮窗（780px 单行紧凑悬浮舱）：
   * 左侧拖拽手柄 + 登录账号徽章（点击登出重扫码）+ 房间选择与三态指示灯（⚪未连 / 🟡未开播 / 🟢直播中）+ 紧凑三语切换器（`中文`/`日本語`/`EN`）+ **`Plugin (N) ▾` 下拉面板** + 更多菜单。
   * **插件下拉卡片（`Plugin`）**：基于 `PluginRegistry` 与 `plugins-updated` 事件热同步展示所有已发现插件（标题、路由 `/{name}`、简介、OBS 建议尺寸），每张卡片提供「桌面打开」（以独立无边框、透明、始终置顶桌面挂件窗口打开 `http://127.0.0.1:<port>/{name}`）与「复制 OBS URL」（复制后显示 1.5s 绿色已复制反馈），底部提供「打开插件目录」一键调起系统文件管理器。
   * **三语宿主国际化（`i18n`）**：主面板、扫码子窗口及系统托盘全量支持简中（`zh-CN`）、日语（`ja`）与英语（`en`）实时切换（内置嵌入式词典，`static/html/` 网页插件独立保持原样）；首次未持久化语言时自动探测系统语言并规范化回退。
   * 通信架构解耦：`dambo-ui` 不连接 `/ws`，全面使用 Tauri 原生 IPC（`room_state_changed`、`auth_state_changed`、`app_state_changed`、`plugins-updated`、`i18n-updated`）。
2. 系统托盘（Tray Icon）：
   * 左键切换浮窗显隐；右键菜单全量对齐三语 i18n，内嵌 **Recent Rooms** 历史房间快捷子菜单（直接读取 `var/state.json` 中的 `room_history` 展示房间号/主播名/标题并一键切换）、**Plugin** 子菜单（内置优先 + 分割线 + 自定义插件，每项包含「桌面打开」与「复制 OBS URL」，底部「打开插件目录...」）、**Language** 三语切换子菜单（`简体中文` / `日本語` / `English` 单选联动）、`Record Raw Packets (*_raw.jsonl)` 运行时抓包开关，以及配置/录制/日志目录、重新登录与退出入口；所有状态变更均通过 IPC 双向同步并原子写入 `var/state.json`。

---

## 5. 静态配置与运行时状态分离（`etc/config.toml` & `var/state.json`）

* **职责边界**：
  * `etc/config.toml`：**全程只读（`Arc<Config>`）**，仅承载静态服务端口、日志级别、录制目录模板、`state_file` 路径及可选静态兜底 `room_id`；`etc/` 目录下仅存放只读配置，程序绝不回写 `etc/config.toml`。
  * `var/state.json`（由 `[server].state_file` 指定，支持相对/绝对路径）：**运行时状态唯一持久化文件（`StateStore`）**，与 `var/session.json`、`var/logs`、`var/records` 统一归口在 `var/` 目录并整体纳入 `.gitignore`；缺失或 JSON 损坏时记录 `warn` 日志并安全回退默认空状态，不阻断启动；任何变更均自动创建父目录后通过同目录临时文件 + `fs::rename` 原子替换。
* **启动房间优先级**：CLI `--room` 覆盖 > `var/state.json` 的 `current_room_id` > 只读 `etc/config.toml` 的 `[bilibili].room_id`。

### 5.1 静态配置文件模板（`etc/config.toml`）

```toml
[bilibili]
room_id = 0                    # Optional static fallback room ID; runtime room & history live in state_file
session_file = "var/session.json"
avatar_cache_size = 2000       # In-memory LRU cache capacity for uid -> face_url

# Channel A: Real-time feed configuration (serves / and /{plugin} web overlays)
[feed]
max_history = 300              # Max core visual items retained in Snapshot ring buffer
show_enter = true              # Forward enter events over /ws
show_medal = true              # Attach fan medal badge and level

# Channel B: robao central controller shaped event stream (ws://localhost:3838/ws/event)
[event]
window_ms = 2500               # Reservoir batching window in milliseconds
dedup_enabled = true           # Deduplicate and normalize short/emote chat messages within window
buffer_capacity = 512          # Bounded ingress queue capacity for backpressure protection
min_passthrough_chars = 4      # Effective char count threshold for 0ms quality-chat passthrough
max_combo_wait_secs = 8        # Max wait cap in seconds for dynamic batch_combo_id gift aggregation

[server]
host = "localhost"             # Change to "0.0.0.0" for LAN access (e.g., iPad/mobile /list)
port = 3838                    # Adjacent to robabel's port 3737
html_dir = "static/html"
state_file = "var/state.json"  # Runtime persisted state (current_room_id, locale, room_history, record_raw)

[log]
level = "info"
path = "var/logs"

[record]
enabled = true                 # Enable isomorphic JSONL recording (shared with /ws/event source)
record_raw = false             # Sidecar raw Bilibili packet capture (*_raw.jsonl with pb_decoded/dm_v2_decoded)
dir = "var/records"
# Placeholders: {room_name} (sanitized streamer name), {room_id}, and chrono strftime specifiers
# Anchors offset_ms to live start_time*1000 and preserves open file across reconnects for the same live session
filename_pattern = "dambo_{room_name}_%Y%m%d_%H%M%S.jsonl"
```

### 5.2 运行时持久化状态 Schema（`var/state.json`）

```json
{
  "current_room_id": 21452505,
  "locale": "zh-CN",
  "room_history": [
    {
      "room_id": 21452505,
      "uname": "主播A",
      "title": "深夜杂谈",
      "last_used_at": 1763280005
    }
  ],
  "record_raw": false
}
```

* `locale` 为可选字段（支持规范化至 `zh-CN`、`ja`、`en`），未设置时自动按系统语言探测并回退至 `en`；
* `room_history` 每次连接或收到 `room_info` / `room_change` 时自动去重、更新主播昵称/房间标题并置顶，上限保留最近 `20` 条。

---

## 6. 数据契约（JSONL 录制 & WebSocket 协议）

### 6.1 本地全量录制格式

#### 主事件文件（`var/records/dambo_主播A_YYYYMMDD_HHMMSS.jsonl`，受 `[record].enabled` 控制）

首行写入 `room_info`（`seq = 0`），中间按 `seq` 单调递增记录 100% 标准事件（含 `live`、`preparing`、`room_change`、`room_silent`、`superchat_delete`、`room_block`、`warning`、`cut_off`、`reconnected`、`chat`、`superchat`、`gift`、`guard`、`guard_enter`、`enter`、`follow`、`share`、`special_follow`、`mutual_follow`、`like`），结束时写入 `session_end` 汇总；其中 `face` 仅在付费事件（付费 `gift`、`superchat`、`guard`）中写入，其余事件自动省略：

```json
{"seq":0,"offset_ms":5000,"timestamp":1763280005000,"room_id":21452505,"room_uname":"主播A","kind":"room_info","uid":123456,"uname":"主播A","content":"深夜杂谈","price":0,"start_time":1763280000,"short_id":512,"title":"深夜杂谈","area_name":"虚拟日常","parent_area_name":"虚拟主播","live_status":1,"raw_cmd":"ROOM_INFO"}
{"seq":1,"offset_ms":6234,"timestamp":1763280006234,"room_id":21452505,"room_uname":"主播A","kind":"chat","uid":3829104,"uname":"观众甲","medal_name":"星光","medal_level":12,"content":"这波操作太细节了","dm_type":0,"price":0,"raw_cmd":"DANMU_MSG"}
{"seq":2,"offset_ms":7100,"timestamp":1763280007100,"room_id":21452505,"room_uname":"主播A","kind":"gift","uid":3829104,"uname":"观众甲","face":"https://i0.hdslb.com/bfs/face/example.jpg","content":"辣条 x3","price":300,"num":3,"batch_combo_id":"batch:gift:1","combo_stay_time":3,"raw_cmd":"SEND_GIFT"}
{"seq":3,"offset_ms":8200,"timestamp":1763280008200,"room_id":21452505,"room_uname":"主播A","kind":"guard","uid":31839477,"uname":"舰长粉","face":"https://i0.hdslb.com/bfs/face/guard.jpg","content":"在主播的直播间开通了舰长","price":168000,"num":1,"guard_level":3,"payflow_id":"2611210800123456","start_time":1763280008,"raw_cmd":"USER_TOAST_MSG"}
{"seq":4,"offset_ms":8900,"timestamp":1763280008900,"room_id":21452505,"room_uname":"主播A","kind":"special_follow","msg_type":4,"uid":9918273,"uname":"老粉丁","content":"特别关注了直播间","price":0,"raw_cmd":"INTERACT_WORD_V2"}
{"seq":5,"offset_ms":9420,"timestamp":1763280009420,"room_id":21452505,"room_uname":"主播A","kind":"like","uid":9918273,"uname":"老粉丁","content":"为主播点赞了","price":0,"raw_cmd":"LIKE_INFO_V3_CLICK"}
{"seq":6,"offset_ms":15000,"timestamp":1763280015000,"room_id":21452505,"room_uname":"主播A","kind":"session_end","uid":0,"uname":"","content":"场次结束汇总","price":0,"summary":{"chat_count":1,"like_count":1,"guard_count":1,"gift_total_coin":300,"superchat_total_coin":0,"enter_count":0,"follow_count":1,"share_count":0},"raw_cmd":"SESSION_END"}
```

#### 旁路原始抓包文件（`var/records/dambo_主播A_YYYYMMDD_HHMMSS_raw.jsonl`，受 `[record].record_raw` 或 `--record-raw` 独立控制）

完整保存 B 站下发的原始 JSON 包；当原始节点中包含 `"pb"` 或 `"dm_v2"` Base64 字段时，自动解包并在同级附加 `"pb_decoded"` / `"dm_v2_decoded"` 明文 JSON 节点：

```json
{"cmd":"INTERACT_WORD_V2","data":{"pb":"...","pb_decoded":{"1":9918273,"2":"老粉丁","5":1,"9":{"2":18,"3":"星光"}}}}
```

### 6.2 通道 A：`ws://<host>:3838/ws`（面向 `/` 与 `/{plugin}` 网页插件；`dambo-ui` 解耦走原生 IPC）

连接成功立即推送首帧 `Snapshot`（`items` 仅包含核心视觉事件 `chat`、`gift`、`superchat`、`guard`），后续每次收到任意标准事件或房间状态变化时实时广播 `Event` / `RoomState`：

```json
{
  "type": "Snapshot",
  "data": {
    "room_id": 21452505,
    "room_uname": "主播A",
    "room_title": "深夜杂谈",
    "is_live": true,
    "connected": true,
    "items": [
      {
        "id": 1024,
        "seq": 1,
        "kind": "chat",
        "uid": 3829104,
        "uname": "观众甲",
        "face": "https://i0.hdslb.com/bfs/face/example.jpg",
        "medal_name": "星光",
        "medal_level": 12,
        "content": "这波操作太细节了",
        "price": 0,
        "timestamp": 1763280006234
      }
    ]
  }
}
```

### 6.3 通道 B：`ws://<host>:3838/ws/event`（面向 `robao` 中控大脑）

客户端连入立即下发 `RoomInfo` 首帧，后续按 0ms 直通、2.5s `Reservoir` 聚合与 `batch_combo_id` 动态连击结算推送 `EventBatch`：

```json
{
  "type": "RoomInfo",
  "data": {
    "seq": 0,
    "offset_ms": 0,
    "timestamp": 1763280005000,
    "room_id": 21452505,
    "room_uname": "主播A",
    "kind": "room_info",
    "uid": 123456,
    "uname": "主播A",
    "content": "深夜杂谈",
    "price": 0,
    "short_id": 512,
    "title": "深夜杂谈",
    "area_name": "虚拟日常",
    "parent_area_name": "虚拟主播",
    "live_status": 1,
    "start_time": 1763280000,
    "raw_cmd": "ROOM_INFO"
  }
}
```

```json
{
  "type": "EventBatch",
  "data": {
    "batch_id": 88,
    "room_id": 21452505,
    "room_uname": "主播A",
    "priority": 0,
    "timestamp": 1763280007500,
    "events": [
      {
        "seq": 14,
        "kind": "chat",
        "uid": 3829104,
        "uname": "观众甲",
        "medal_level": 12,
        "content": "666",
        "count": 14,
        "sample_users": ["观众甲", "观众乙", "观众丙"],
        "price": 0
      },
      {
        "seq": 15,
        "kind": "like",
        "uid": 9918273,
        "uname": "老粉丁",
        "medal_level": 18,
        "content": "为主播点赞了",
        "count": 23,
        "sample_users": ["老粉丁"],
        "price": 0
      }
    ]
  }
}
```

```json
{
  "type": "EventBatch",
  "data": {
    "batch_id": 89,
    "room_id": 21452505,
    "room_uname": "主播A",
    "priority": 1,
    "timestamp": 1763280008100,
    "events": [
      {
        "seq": 16,
        "kind": "special_follow",
        "msg_type": 4,
        "uid": 9918273,
        "uname": "老粉丁",
        "medal_level": 18,
        "content": "特别关注了直播间",
        "count": 1,
        "sample_users": ["老粉丁"],
        "price": 0
      }
    ]
  }
}
```
