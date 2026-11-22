# dambo

[English](README.md) | [简体中文](README.zh.md)

基于 Rust 与 Tauri v2 构建的 B 站直播间弹幕采集器、热重载 OBS 网页插件宿主、无损事件录制工具与 AI 流量整形节点。

**dambo** 既可作为独立运行的透明 OBS 弹幕挂件与直播记录助手，也可通过 `/ws/event` 接口向各类下游机器人或 AI 管线输出整形后的实时互动数据流。

- **架构与产品设计**：[DESIGN.zh.md](DESIGN.zh.md)
- **插件开发规范**：[PLUGIN_GUIDE.zh.md](PLUGIN_GUIDE.zh.md)

---

## 核心特性

- **桌面悬浮控制舱与系统托盘（`dambo-ui`）**
  - 单行紧凑无边框悬浮控制栏，集成 B 站扫码登录、快速选房（我的直播间 / 关注开播中 / 历史房间 / 直播间链接解析）、托盘快捷操作以及 **简体中文 / 日本語 / English** 实时切换。
- **零重启热重载 OBS 与桌面悬浮插件**
  - 自动发现 `static/html/` 下的网页插件（约 200ms 防抖热更新），既可复制链接接入 OBS 浏览器源，也可直接在桌面弹出透明置顶悬浮窗。
- **全量无损 JSONL 录制**
  - 将直播间完整互动记录（弹幕、醒目留言、礼物、大航海、进房关注与房间状态变更）持久化至 `var/records/`，支持断线重连续写同一场次及旁路原始抓包（`--record-raw`）。
- **双通道 WebSocket 数据流**
  - **`/ws`（视觉直通流）**：向 OBS 与桌面挂件提供实时消息及历史快照（`Snapshot`、`Event`、`RoomState`）。
  - **`/ws/event`（AI 整形流）**：通过三车道分流合并重复短弹幕刷屏与礼物连击，同时零延迟直通醒目留言、大航海与开播状态事件。
- **无头 CLI 模式（`dambo-cli`）**
  - 无需桌面环境即可在终端或服务器独立运行完整采集、录制与 HTTP/WS 服务。

---

## 快速开始

### 1. 环境准备

- **Rust**：`1.90+`（Cargo Edition 2024）
- **macOS**：Xcode Command Line Tools 与 `cmake`（`brew install cmake`）
- **Windows**：MSVC 构建工具、WebView2（Windows 10/11 自带）以及 `nasm`
- **Linux**：Tauri v2 所需的 WebKit/GTK 依赖（`libwebkit2gtk-4.1-dev`、`libgtk-3-dev`、`libayatana-appindicator3-dev` 等）

### 2. 启动运行

**桌面控制舱（`dambo-ui`）**：
```bash
cargo run -p dambo-ui --release -- --config etc/config.toml
```

**无头命令行节点（`dambo-cli`）**：
```bash
cargo run -p dambo-cli --release -- --config etc/config.toml
```

<details>
<summary>CLI 常用参数（<code>dambo-cli</code>）</summary>

| 参数 | 默认值 | 说明 |
| --- | --- | --- |
| `-c, --config <CONFIG>` | `etc/config.toml` | 指定只读 TOML 配置文件路径 |
| `-r, --room <ROOM>` | *(读取状态/配置)* | 覆盖连接房间号、短号或 `https://live.bilibili.com/...` 链接 |
| `--no-login` | `false` | 未登录时跳过终端交互式扫码流程 |
| `--record-raw` | `false` | 启用旁路原始包抓取（输出 `*_raw.jsonl`） |

</details>

### 3. 连接直播间并接入 OBS

1. 按弹窗或终端提示使用哔哩哔哩 App 扫码登录（登录态保存在本地 `var/session.json`）。
2. 输入房间号或粘贴直播间链接，也可以直接在下拉面板选择**我的直播间**、**关注开播中**或**历史房间**。
3. 在控制舱或系统托盘打开 **插件（Plugins）** 菜单：
   - 点击 **复制 OBS URL**，在 OBS Studio 中新建「浏览器」源并填入地址（例如 `http://localhost:3838/default`）。
   - 或直接点击 **桌面打开**，将挂件作为透明置顶悬浮窗显示在桌面上。
4. *（可选）* 如需在局域网手机或平板浏览器查看 `/list` 弹幕监控，可将 `etc/config.toml` 中 `[server]` 的 `host` 改为 `"0.0.0.0"`。

---

## 内置插件与自定义开发

Dambo 在 `static/html/` 中内置了 3 个基于纯 Web SDK（`/sdk/dambo.js`）的开箱即用插件：

| 插件目录 | 访问路由 | 适用场景 |
| --- | --- | --- |
| **`default`** | `/` 或 `/default` | 透明 OBS 弹幕叠加挂件（含顶部 SC/舰长置顶栏与底部进房停靠条） |
| **`list`** | `/list` | 主播/场控网页端监控列表（支持分类筛选、向上翻看防滚屏与 SC 倒计时栏） |
| **`sc-ticker`** | `/sc-ticker` | 极简醒目留言（SC）与大航海倒计时卡片 |

### 开发自定义插件

只需在 `static/html/<plugin-id>/` 下按标准四件套放置文件（`index.html`、`plugin.json`、`plugin.css`、`plugin.js`，如有图片、字体或音频等静态文件也可放在同级 `assets/` 目录内），即可自动热加载上线并出现在控制舱和托盘菜单中，无需重启主程序。

完整的纯 Web SDK 接口定义、最小模板与自检清单请参阅 [PLUGIN_GUIDE.zh.md](PLUGIN_GUIDE.zh.md)。

---

## 配置与接口总览

Dambo 严格隔离静态配置与运行期状态：
- **`etc/config.toml`**（只读）：源码开发时可复制 `etc/config.sample.toml` 为 `etc/config.toml` 自定义配置（发行版已内置默认 `etc/config.toml`；若未提供则自动回退到内置默认值）。用于配置 `[server]` 监听地址与端口（默认 `localhost:3838`）、`[feed]` 历史缓冲长度与展示开关、`[event]` 流量整形阈值以及 `[record]` 录制文件命名模板（`dambo_{room_name}_%Y%m%d_%H%M%S.jsonl`）。
- **`var/`**（运行期产物，已 gitignore）：存放 `state.json`（当前房间、语言偏好、历史房间）、`session.json`、`logs/` 与 `records/`。

| 接口路径 | 用途说明 |
| --- | --- |
| `GET /` | 默认透明 OBS 弹幕挂件（`static/html/default/index.html`） |
| `GET /{plugin}` | 自动发现的插件入口页面 |
| `GET /sdk/dambo.js` | 内置零依赖纯 Web 插件 SDK |
| `GET /ws` | 通道 A WebSocket — 面向视觉挂件的实时流（`Snapshot`、`Event`、`RoomState`） |
| `GET /ws/event` | 通道 B WebSocket — 带优先级的整形事件批次流（`RoomInfo`、`EventBatch`） |

---

## 本地构建、测试与打包

```bash
# 运行工作区全部单元测试与集成测试
cargo test --workspace

# 运行严格静态检查
cargo clippy --workspace --all-targets -- -D warnings

# 构建并打包 macOS 应用（生成 ./Dambo.app）
chmod +x scripts/package_macos.sh
./scripts/package_macos.sh
```

`scripts/package_macos.sh` 支持 `--debug`、`--offline` 与 `--skip-build` 参数。跨平台 CI 构建与发版打包流程位于 [.github/workflows/build.yml](.github/workflows/build.yml)。

## 工作区目录结构

```text
dambo/
├── dambo-core/   # 核心库：弹幕采集、鉴权、无损录制、事件整形引擎、插件注册表、HTTP/WS 服务与内嵌 SDK
├── cli/          # 无头 CLI 入口（dambo-cli）
├── src-tauri/    # Tauri v2 桌面宿主、系统托盘与透明悬浮窗管理（dambo-ui）
├── ui/           # 桌面悬浮控制舱与扫码弹窗前端
├── static/html/  # 热加载 OBS / 网页插件目录（default、list、sc-ticker 及自定义插件）
├── etc/          # 只读配置模板（config.toml）
└── scripts/      # 本地打包脚本
```
