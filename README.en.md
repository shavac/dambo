# dambo

[简体中文](README.md) | [English](README.en.md)

> **Dambo — An Advanced Danmaku Platform for the AI Era.**

**dambo** is a lightweight, high-performance Bilibili live-room danmaku platform that brings together **live danmaku collection & desktop chat input**, **transparent OBS overlays & desktop floating widgets**, **lossless session recording**, and **AI-ready real-time event streams** out of the box.

Whether you want ultra-lightweight transparent overlays while gaming, a quick desktop chat box to interact with viewers, interactive chat mini-games like 24-Point Poker, AI-generated custom plugins, or structured WebSocket event feeds for bots and AI pipelines, **dambo** handles it smoothly.

- **AI Plugin Guide**: [PLUGIN_GUIDE.md](PLUGIN_GUIDE.md) (Chinese: [PLUGIN_GUIDE.zh.md](PLUGIN_GUIDE.zh.md)) — upload this file directly to an AI assistant and let it build or customize plugins for your stream.

---

## Key Capabilities & Advantages

- **High Performance & Low Resource Footprint**
  - Built on a native **Rust + Tokio + Tauri** async engine with zero-dependency Pure-Web plugins (no Electron runtime or frontend bundler overhead, bounded ring buffers, and traffic-shaping event streams), keeping CPU and memory usage minimal even during heavy chat bursts or gaming streams.

- **Desktop Control Capsule & System Tray (`dambo-ui`)**
  - Compact frameless floating toolbar: drag by the left grip handle, resize width from the right edge, switch **简体中文 / 日本語 / English** live, or control everything from the system tray.
  - Sign in via **Bilibili QR Code** and switch instantly between **My Room**, **Following Live**, **Recent Rooms**, or any pasted live URL.
- **Built-in Danmaku Chat Input Box**
  - Once signed in and connected to a room, click the **chat bubble button** on the control capsule to expand a matching full-width danmaku input bar directly underneath and send messages straight into the live chat.
- **OBS Browser Sources & Always-on-Top Desktop Overlays**
  - Copy any plugin URL into OBS Studio as a transparent Browser Source, or launch it directly as an always-on-top transparent desktop window; plugins hot-reload automatically (~200ms) with no restart needed.
- **Ready-to-Use Built-in Plugins**
  - Includes a transparent OBS danmaku overlay (with 4-second gift combo merging and auto-follow bottom scroll), an interactive moderator monitor feed, a SuperChat/Guard countdown ticker, and a live **24-Point Poker** danmaku game.
- **Lossless JSONL Recording & Dual WebSocket Streams**
  - Continuously records chat, gifts, SuperChats, guards, room interactions, and status transitions to `var/records/`, plus headless CLI mode (`dambo-cli`) and `/ws` + `/ws/event` feeds.

---

## Getting Started

### 1. Launch Dambo

- **Release Archive (Recommended)**: Download and extract the release bundle for your OS, then launch **`dambo-ui`** (or `Dambo.app` on macOS). Release archives already ship with a default `etc/config.toml` and all built-in plugins ready to go.
- **Headless CLI Mode**: Run **`dambo-cli`** in a terminal or server for background recording and WebSocket streaming (supports `-c` config path, `-r` room ID/URL, `--no-login`, and `--record-raw`).

<details>
<summary>Running from Source (Optional)</summary>

Requires Rust `1.90+` and Tauri v2 platform dependencies:

```bash
# Desktop Control Capsule
cargo run -p dambo-ui --release -- --config etc/config.toml

# Headless CLI Mode
cargo run -p dambo-cli --release -- --config etc/config.toml
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for testing and contribution conventions.

</details>

### 2. Sign In & Connect to a Room

1. **QR Sign In**: Click the account button on the left of the capsule and scan the QR code with the Bilibili mobile app (your session is persisted locally in `var/session.json`).
2. **Connect to a Live Room**:
   - Enter a **room ID**, **short ID**, or `https://live.bilibili.com/...` URL and press `Enter` (or click **Connect**);
   - Or open the dropdown menu to pick directly from **My Room**, **Following Live**, or **Recent Rooms**.

### 3. Send Danmaku with the Chat Input Box

When you are both **signed in to Bilibili** and **connected to a live room**, the **Send Danmaku** button on the capsule becomes active:

1. Click the **chat bubble button** to expand the full-width danmaku input bar directly below the capsule (up to `40` characters per message).
2. Type your message and press **`Enter`** (or click **Send**) to post it to the live room immediately—focus stays in the input box so you can reply continuously.
3. Switching focus to OBS, games, or other windows will **not** accidentally collapse the input box; to hide it, click the toggle button again, press **`Esc`**, or click any other control button on the capsule.

> If you are not signed in or not connected to a room, the button stays disabled with a tooltip explaining why.

### 4. Use Plugins in OBS or on Your Desktop

Open **Plugins (...)** on the capsule (or **Plugins** in the system tray menu):

- **Use in OBS Studio**: Click **Copy OBS URL**, create a **Browser** source in OBS, paste the URL, and set the recommended width/height from the table below for transparent overlay rendering.
- **Float on Desktop**: Click **Open on Desktop** to pop out any plugin as a transparent desktop window. Hover near the top-right corner of the overlay to drag it, toggle **Always on Top**, or close it (or press `Esc`).
- **View on LAN Devices**: To access `/list` from a phone or tablet on the same network, set `host = "0.0.0.0"` under `[server]` in `etc/config.toml`.

---

## Built-in Plugins

Dambo includes four ready-to-use web plugins out of the box:

| Plugin | Route | Recommended OBS Size | Use Case |
| --- | --- | --- | --- |
| **`default`** | `/` or `/default` | `420 × 720` | **Primary OBS Transparent Overlay**: pinned SuperChat/Guard bar, ephemeral enter dock, 4s sliding gift combo merging with pulse feedback, and auto-follow bottom scroll |
| **`list`** | `/list` | `480 × 800` | **Streamer / Moderator Monitor**: category filters (All / Chat / Gift & SC / Medal), manual scroll-up pause with unread counter, and sticky SC header |
| **`sc-ticker`** | `/sc-ticker` | `420 × 320` | **SC & Guard Countdown Cards**: compact corner widget focused purely on SuperChats and Guard subscriptions |
| **`game24`** | `/game24` | `560 × 420` | **24-Point Poker Chat Game**: deals 4 cards (A–K) each round for viewers to solve via danmaku math expressions (e.g. `(3+3)*(8-4)`), with live scoring & leaderboard; defaults to `300s` per round and supports `?timeout=300&solvable_only=false&settle_time=3.5&skip_time=2.5` |

---

## Build Custom Plugins with AI (`PLUGIN_GUIDE`)

Want a custom theme, giveaway spinner, song-request board, or chat mini-game? **No manual coding required**:

1. Upload **[PLUGIN_GUIDE.md](PLUGIN_GUIDE.md)** (or Chinese **[PLUGIN_GUIDE.zh.md](PLUGIN_GUIDE.zh.md)**) directly to your preferred AI assistant.
2. Describe what you want in plain language (for example: *"Using the uploaded PLUGIN_GUIDE.md, build a minimalist cyberpunk gift-alert banner plugin"*) and tell the AI to **follow the guide strictly**.
3. Click **Plugins -> Open Plugins Folder** in Dambo (`static/html/`), create a new folder for your plugin, and place the AI-generated files inside (`index.html`, `plugin.json`, `plugin.css`, `plugin.js`, plus an optional `assets/` folder for images, fonts, or audio).
4. Dambo hot-reloads the folder in ~200ms—your custom plugin appears right away in the UI and tray menu without restarting.

---

## Recording, Settings & Endpoints

### Quick Actions (`...` Menu & System Tray)

- **Always on Top**: Pin the floating control capsule above other windows.
- **Record Raw Packets (`*_raw.jsonl`)**: Capture sidecar raw protocol packets alongside normalized `.jsonl` recordings.
- **Open Config File / Recordings Folder / Logs Folder**: Jump straight to your files on disk.

### Configuration & Runtime Data

- **`etc/config.toml`** (read-only; when running from source, copy [etc/config.sample.toml](etc/config.sample.toml) to `etc/config.toml` to customize):
  - `[server]`: defaults to `localhost:3838` (change `host = "0.0.0.0"` for LAN access).
  - `[feed]`: controls snapshot history depth (`max_history`), room-enter alerts (`show_enter`), and fan medals (`show_medal`).
  - `[record]`: controls lossless JSONL recording and filename patterns (saved under `var/records/`).
- **`var/`** (runtime data): stores your saved login session (`session.json`), room history & language preference (`state.json`), logs (`logs/`), and recordings (`records/`).

### HTTP & WebSocket Endpoints

| Endpoint | Purpose |
| --- | --- |
| `GET /` or `GET /{plugin}` | Open any built-in or custom plugin page in a browser / OBS |
| `GET /sdk/dambo.js` | Embedded zero-dependency Pure-Web Plugin SDK |
| `GET /ws` | Channel A WebSocket — real-time danmaku & overlay event feed (`Snapshot`, `Event`, `RoomState`) |
| `GET /ws/event` | Channel B WebSocket — shaped, combo-settled event batches for bots, TTS & AI pipelines |

---

## Code Signing Policy & Privacy

> Free code signing provided by [SignPath.io](https://signpath.io/), certificate by [SignPath Foundation](https://signpath.org/).

- **Signed Artifacts**: Windows release executables (`dambo-ui.exe` and `dambo-cli.exe`) built by official GitHub-hosted Actions workflows are submitted to SignPath.io for code signing. Static web plugins and user configuration files remain plain-text.
- **Team Roles**: Repository maintainers act as committers and reviewers; release signing requests are approved by SignPath project administrators and restricted to builds originating directly from this public repository's GitHub Actions workflows.
- **Privacy Policy**: Dambo runs entirely locally and **does not collect, track, or transmit any hidden telemetry, analytics, or crash reports**. Network requests are made solely to official Bilibili APIs when the user explicitly chooses to sign in or connect to a live room, and all session cookies and recordings remain strictly on local disk (`var/`). See [SECURITY.md](SECURITY.md) for vulnerability reporting.
