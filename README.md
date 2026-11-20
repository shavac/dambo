# dambo

[English](README.md) | [简体中文](README.md.zh)

**`dambo`** (*Danmaku + Dam + Bot*) is a Bilibili live-room danmaku collector, lossless event recorder, OBS overlay server, and traffic-shaping edge node written in Rust.

Designed as a **stateless edge pipeline** alongside `robabel` (or as a standalone OBS danmaku & recording tool), `dambo` captures 100% of live room events to JSONL, renders browser overlays at `:3838`, and shapes high-volume danmaku storms into clean prioritized batches for downstream AI orchestration.

For full product requirements and design notes, see [DESIGN.md.zh](DESIGN.md.zh).

---

## Overview & Ecosystem

In the three-node local AI livestream assistant architecture, `dambo` sits symmetrically alongside the audio perception node `robabel` and feeds the stateful central controller `robao`:

| Node | Default Port | Role | Outputs |
| --- | --- | --- | --- |
| **`robabel`** | `:3737` | Stateless audio perception node | Real-time ASR stream (`/ws`), OBS speech subtitles (`/`) |
| **`dambo`** | `:3838` | Stateless live-interaction & traffic-shaping node | OBS danmaku overlay (`/`), web monitor (`/list`), raw `/ws`, dam batch stream (`/ws/dam`), lossless JSONL (`./rec/`) |
| **`robao`** | — | Stateful central brain | Subscribes to `3737/ws` & `3838/ws/dam` for millisecond alignment, viewer lifecycle FSM, long-term memory, and LLM/TTS replies |

> **Design Boundary**: `dambo` intentionally stays **stateless** regarding viewer lifecycle state machines and contains no LLM inference logic. All configurations and wire protocols use numeric `room_id` as the canonical room identifier.

---

## Architecture

```text
Bilibili Live WebSocket (blivedm + Protobuf v2 + Brotli/Zlib)
                     │
                     ▼
           collector.rs (RawBiliEvent)
                     │
        ┌────────────┴────────────────────┐
        │ Pre-Filter Tap (100% events)    │
        ▼                                 ▼
  recorder.rs                        filter.rs
  Lossless JSONL (./rec/)     LRU Avatar Cache (uid -> face)
  Auto-rotate on switch/LIVE  Drop like / enter / < min_gift_coin
                                          │
                        ┌─────────────────┴─────────────────┐
                        ▼                                   ▼
             Channel A: StateHub                 Channel B: DamEngine
             Ring buffer (300 items)             Reservoir (window_ms) + Spillway
                        │                                   │
         ┌──────────────┼──────────────┐                    ▼
         ▼              ▼              ▼               ws://:3838/ws/dam
   GET / & /list   ws://:3838/ws   Tauri IPC            (DamBatch for robao;
   (OBS / Web)     (Snapshot /     (room_state_changed   no face URLs,
                    Event /         & auth_state_changed; backpressure-safe)
                    RoomState)      never joins /ws)
```

---

## Features

- **Zero-Disruption Bilibili QR Auth (`auth.rs`)**:
  - Validates existing cookies silently on startup via `https://api.bilibili.com/x/web-interface/nav` (`dambo-ui` retries transient failures at `2s`, `5s`, and `10s`).
  - Pops up an always-on-top `320×380` SVG QR sign-in window in `dambo-ui` (or prints a Unicode QR code in the terminal for the `dambo` CLI), polling every 2 seconds and auto-refreshing on expiry.
  - Persists `SESSDATA`, `bili_jct`, and `DedeUserID` (along with cached `uname` and `face`) to `session_file`.

- **Single-Room Collector & Smart Switcher (`collector.rs`)**:
  - Accepts pure numeric room IDs (automatically resolving Bilibili short IDs), full `https://live.bilibili.com/...` URLs, or formatted display labels.
  - Decodes `LIVE`, `PREPARING`, `ROOM_CHANGE`, `ROOM_BLOCK_MSG`, `WARNING`, `CUT_OFF`, `DANMU_MSG` (including big stickers `emoticon` and inline `emots`), `SUPER_CHAT_MESSAGE` / `SUPER_CHAT_MESSAGE_JPN`, `SEND_GIFT` / `COMBO_SEND` / `GIFT_COMBO`, `GUARD_BUY` / `USER_TOAST_MSG` / `USER_TOAST_MSG_V2`, `INTERACT_WORD` / `INTERACT_WORD_V2` (`enter`, `follow`, `share`), and `LIKE_INFO_V3_CLICK` (`like`).
  - Reconnects automatically with exponential backoff capped at 60 seconds.
  - On-demand room dropdown lists **My Room**, **Following Live**, and **Recent Rooms** (up to 20, individually removable).

- **Pre-Filter Lossless JSONL Recorder (`recorder.rs`)**:
  - Mounted **before** the noise filter so 100% of room events—including likes and free gifts—are flushed line-by-line to JSONL.
  - Supports `{room_name}` (illegal cross-platform filename characters sanitized to `_`, falling back to `{room_id}`), `{room_id}`, and `chrono` `strftime` placeholders in `record.filename_pattern`.
  - Automatically rotates to a new file and resets `offset_ms = 0` both on room connect/switch and upon receiving a `LIVE` start signal (writing the `live` event as line 1).

- **Noise Filter & Zero-Risk Avatar Cache (`filter.rs`)**:
  - Never queries extra HTTP user profile endpoints. Instead, warms an in-memory LRU cache (`uid -> face_url`, default `2000` entries) directly from incoming events—even filtered ones such as free gifts—and backfills missing avatars onto `enter` events.
  - Configurable downstream filtering for `like`, `enter`, and `gift` below `min_gift_coin` (in gold seeds, `1000 = 1 CNY`).

- **Dual-Channel Axum Server & Built-in Themes (`server.rs`, `static/themes/`)**:
  - Injects `Cache-Control: no-store, no-cache, must-revalidate` and `Pragma: no-cache` on HTTP responses so OBS Browser Sources never serve stale CSS/JS.
  - **`default`**: Transparent OBS overlay featuring a pinned SuperChat/Guard bar (up to 4 cards), chat list (up to 60 items) with avatars, fan medals, big stickers, and inline emotes, plus a temporary bottom enter dock (`enter-dock`).
  - **`sc-ticker`**: Minimal OBS overlay showing timed ticker cards exclusively for `super_chat` (60s–300s by CNY tier) and `guard` (90s) events (up to 8 cards).
  - **`list` (`/list`)**: Standalone web monitor with a sticky SuperChat/Guard countdown bar, smart scroll-lock (`↓ N new messages`), and quick filters (`All`, `Chat`, `Gifts & SC`, `Medals`).
  - **Dam Engine (`/ws/dam`)**: Always-on reservoir (`window_ms`, `priority = 0`, chat deduplication + sample users) paired with a zero-delay spillway (`priority = 1` for gifts, `priority = 2` for SuperChat, Guard, and room status events) and tiered bounded backpressure protection (`buffer_capacity`, default `512`).

- **Decoupled Tauri v2 Desktop Capsule & System Tray (`src-tauri/`, `ui/`)**:
  - Compact `740×56` always-on-top floating capsule (expanding to `380px` height when the room dropdown opens) with tri-state status LED (`⚪` Disconnected, `🟡` Connected / Offline, `🟢` Live), runtime theme switching, one-click OBS URL copy, and a More (`...`) utility menu.
  - System tray left-click toggles capsule visibility; tooltip displays active room ID, streamer name, title, and live status; context menu provides quick links to overlays, logs, recordings, config, and re-login.
  - Subscribes strictly to Tauri IPC events (`room_state_changed`, `auth_state_changed`) without touching `/ws`, keeping desktop overhead near zero during danmaku storms.

---

## Project Structure

```text
dambo/
├── Cargo.toml                  # Virtual Cargo workspace manifest (Edition 2024)
├── DESIGN.md.zh                # Architecture & PRD specification
├── dambo_config.toml           # Default runtime configuration
├── dambo-core/                 # Shared Rust core library
│   └── src/
│       ├── lib.rs              # DamboRuntime pipeline orchestrator
│       ├── config.rs           # TOML loading & comment-preserving persistence (toml_edit)
│       ├── model.rs            # RawBiliEvent, Channel A/B & IPC data contracts
│       ├── auth.rs             # Bilibili cookie validation, SVG/terminal QR login
│       ├── collector.rs        # blivedm & Protobuf v2 collector + backoff reconnect
│       ├── recorder.rs         # Pre-filter lossless JSONL recorder & rotation
│       ├── filter.rs           # Downstream noise filter & LRU avatar cache
│       ├── hub.rs              # Channel A ring buffer & Tauri watch state hub
│       ├── dam_engine.rs       # Channel B reservoir, spillway & backpressure queue
│       └── server.rs           # Axum HTTP static server & dual WS endpoints
├── cli/                        # Headless CLI crate (`dambo-cli`, bin `dambo`)
│   └── src/main.rs
├── ui/                         # Tauri desktop frontend (`index.html`, `login.html`, `style.css`, `app.js`)
├── src-tauri/                  # Tauri v2 desktop host & system tray (`dambo-ui`)
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   ├── capabilities/default.json
│   └── src/
│       ├── main.rs
│       └── lib.rs
├── static/themes/              # OBS overlays & web monitor assets
│   ├── default/                # Default transparent OBS danmaku overlay (`GET /`)
│   ├── sc-ticker/              # SC & Guard ticker OBS overlay
│   └── list/                   # Web danmaku monitor (`GET /list`)
├── scripts/
│   └── package_macos.sh        # Builds & ad-hoc signs `Dambo.app`
└── .github/workflows/
    └── build.yml               # macOS arm64 & Windows x64 CI + release packaging
```

---

## Quick Start

### Prerequisites

- **Rust toolchain**: Rust **1.90+** (Cargo Edition 2024).
- **macOS**: Xcode Command Line Tools and `cmake` (`brew install cmake` if missing).
- **Windows**: MSVC build tools, WebView2 (included on Windows 10/11), and `nasm` (required by `aws-lc-sys`).
- **Linux**: Tauri v2 system WebKit/GTK dependencies (`libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `libayatana-appindicator3-dev`, etc.).

### 1. Headless CLI (`dambo`)

```bash
# Build & run from the workspace root
cargo run -p dambo-cli --release -- --config dambo_config.toml
```

Available CLI options:

| Flag | Default | Description |
| --- | --- | --- |
| `-c, --config <CONFIG>` | `dambo_config.toml` | Path to the TOML configuration file |
| `-r, --room <ROOM>` | *(from config)* | Override room ID, short ID, or `https://live.bilibili.com/...` URL |
| `--no-login` | `false` | Skip interactive terminal QR login when unauthenticated |

### 2. Desktop Control Capsule (`dambo-ui`)

```bash
# Launch the floating control capsule & system tray app
cargo run -p dambo-ui --release -- --config dambo_config.toml
```

`dambo-ui` supports `-c, --config <CONFIG>` (default `dambo_config.toml`). Relative config paths are resolved automatically across the current working directory, executable ancestor directories (including macOS `Contents/Resources/`), and the repository root.

### 3. Connect OBS & Web Monitor

1. Sign in via the QR prompt (or run with `--no-login` for guest mode where supported by Bilibili).
2. Enter a room ID or paste a `https://live.bilibili.com/<id>` URL (or choose from **My Room** / **Following Live** / **Recent Rooms**).
3. In **OBS Studio**, add a **Browser Source** pointing to `http://localhost:3838/` (transparent background is built into `default` and `sc-ticker`).
4. For a secondary monitor or mobile/tablet viewer on your LAN, set `[server].host = "0.0.0.0"` in `dambo_config.toml` and open `http://<your-ip>:3838/list`.

### 4. Testing & Packaging

```bash
# Run all workspace unit & integration tests
cargo test --workspace

# Package macOS app bundle (./Dambo.app)
chmod +x scripts/package_macos.sh
./scripts/package_macos.sh
```

`scripts/package_macos.sh` accepts `--debug`, `--offline`, and `--skip-build`. GitHub Actions (`.github/workflows/build.yml`) builds release archives for `macos-latest` (`dambo-macos-arm64.zip`) and `windows-latest` (`dambo-windows-x64.zip`) on push, and publishes GitHub Releases automatically for `v*` tags.

---

## Configuration (`dambo_config.toml`)

Below is the default configuration schema (note that switching rooms or changing OBS themes from the UI/CLI updates `dambo_config.toml` in place via `toml_edit` while preserving your comments):

```toml
[bilibili]
room_id = 0                    # Target live room ID (numeric; 0 = disconnected; auto-resolves short IDs)
history_rooms = []             # Recently connected numeric room IDs (up to 20)
session_file = "./dambo_session.json"
min_gift_coin = 1000           # Real-time channel gift threshold in gold seeds (1000 = 1 CNY; does not affect recorder)
ignore_enter = false           # Drop room-enter events in real-time channels (default: false)
ignore_like = true             # Drop like events in real-time channels (default: true; does not affect recorder)
avatar_cache_size = 2000       # In-memory LRU cache capacity for uid -> face_url

# Channel A: Real-time feed configuration (serves / OBS overlay and /list web feed)
[feed]
max_history = 300              # Max items retained in Snapshot ring buffer
show_enter = true              # Render viewer enter notifications in frontend themes
show_medal = true              # Render fan medal badge and level

# Channel B: robao central controller dam stream (ws://localhost:3838/ws/dam)
[dam]
window_ms = 2500               # Reservoir batching window in milliseconds
dedup_enabled = true           # Deduplicate and count repeated chat messages within window
buffer_capacity = 512          # Bounded ingress queue capacity for backpressure protection

[server]
host = "localhost"             # Set to "0.0.0.0" for LAN access (e.g., iPad/mobile /list)
port = 3838                    # Adjacent to robabel's default port 3737
html_dir = "static/themes"     # Directory containing OBS & web themes
theme = "default"              # Active OBS overlay theme served at GET /

[log]
level = "info"                 # off | error | warn | info | debug | trace (RUST_LOG also supported)
path = "./logs"                # Desktop UI writes rotating logs here (5 MB x 5 files)

[record]
enabled = true                 # Enable lossless JSONL recording (tapped pre-filter)
dir = "./rec"
# Placeholders: {room_name} (sanitized streamer name), {room_id}, and chrono strftime specifiers
# Automatically rotates to a new file and resets offset_ms on room switch or LIVE signal
filename_pattern = "dambo_{room_name}_%Y%m%d_%H%M%S.jsonl"
```

---

## HTTP Endpoints & Data Protocols

### Server Routes (`http://<host>:<port>`)

| Endpoint | Protocol | Purpose |
| --- | --- | --- |
| `GET /` | HTTP | Serves `static/themes/<theme>/index.html` (active OBS overlay; hot-switchable) |
| `GET /list` | HTTP | Serves `static/themes/list/index.html` (interactive web danmaku monitor) |
| `GET /static/themes/*` | HTTP | Serves theme CSS/JS/assets with `Cache-Control: no-store, no-cache, must-revalidate` |
| `GET /ws` | WebSocket | **Channel A**: Real-time stream (`Snapshot`, `Event`, `RoomState`) for `/` & `/list` |
| `GET /ws/dam` | WebSocket | **Channel B**: Always-on shaped `DamBatch` stream for `robao` |

### 1. Local Lossless JSONL (`./rec/dambo_<room_name>_YYYYMMDD_HHMMSS.jsonl`)

Every raw event—before downstream filtering—is appended as one JSON object per line (`RawBiliEvent`). Optional fields (`face`, `medal_name`, `medal_level`, `emoticon`, `emots`) are omitted when absent:

```json
{"offset_ms":0,"timestamp":1763280000000,"room_id":21452505,"room_uname":"StreamerA","kind":"live","uid":0,"uname":"","content":"直播开始","price":0,"raw_cmd":"LIVE"}
{"offset_ms":1234,"timestamp":1763280001234,"room_id":21452505,"room_uname":"StreamerA","kind":"chat","uid":3829104,"uname":"Viewer1","face":"https://i0.hdslb.com/bfs/face/example.jpg","medal_name":"Starlight","medal_level":12,"content":"Nice play!","price":0,"raw_cmd":"DANMU_MSG"}
{"offset_ms":5420,"timestamp":1763280005420,"room_id":21452505,"room_uname":"StreamerA","kind":"like","uid":9918273,"uname":"FanD","content":"为主播点赞了","price":0,"raw_cmd":"LIKE_INFO_V3_CLICK"}
```

Supported `kind` values: `live`, `preparing`, `room_change`, `room_block`, `warning`, `cut_off`, `chat`, `super_chat`, `gift`, `guard`, `enter`, `follow`, `share`, `like`.

### 2. Channel A (`ws://<host>:3838/ws`)

Immediately upon connecting (or if a slow client lags behind), the server sends a `Snapshot` frame containing room metadata and recent history (up to `feed.max_history`). Subsequent updates are pushed as `Event` or `RoomState` frames:

```json
{
  "type": "Snapshot",
  "data": {
    "room_id": 21452505,
    "room_uname": "StreamerA",
    "room_title": "Late Night Stream",
    "is_live": true,
    "connected": true,
    "items": [
      {
        "id": 1024,
        "kind": "chat",
        "uid": 3829104,
        "uname": "Viewer1",
        "face": "https://i0.hdslb.com/bfs/face/example.jpg",
        "medal_name": "Starlight",
        "medal_level": 12,
        "content": "Nice play!",
        "price": 0,
        "timestamp": 1763280001234
      }
    ]
  }
}
```

Live updates use the same `{ "type": "...", "data": ... }` envelope:
- `Event`: single `SnapshotItem`
- `RoomState`: `{ "room_id", "room_uname", "room_title", "is_live", "connected" }`

### 3. Channel B (`ws://<host>:3838/ws/dam`)

Pushes `DamBatch` payloads with `face` URLs stripped:
- `priority = 0`: Periodic reservoir flush (`window_ms`), with repeated `chat` messages deduplicated (`count`, `sample_users` up to 5) and `enter` events merged per user.
- `priority = 1`: Immediate gift spillway.
- `priority = 2`: Immediate critical spillway (`super_chat`, `guard`, `live`, `preparing`, `room_change`, `warning`, `cut_off`).

When the bounded ingress queue (`buffer_capacity`) saturates during danmaku storms, `DamEngine` drops events in strict tier order—no-medal `enter` first, then `chat`, then `medal enter`/`follow`/`share`/`room_block`, then `gift`—and **never** drops tier-4 `super_chat`, `guard`, or room status events.

```json
{
  "type": "DamBatch",
  "data": {
    "batch_id": 88,
    "room_id": 21452505,
    "room_uname": "StreamerA",
    "priority": 0,
    "timestamp": 1763280002500,
    "events": [
      {
        "kind": "chat",
        "uid": 3829104,
        "uname": "Viewer1",
        "medal_level": 12,
        "content": "666",
        "count": 14,
        "sample_users": ["Viewer1", "Viewer2", "Viewer3"],
        "price": 0
      },
      {
        "kind": "enter",
        "uid": 9918273,
        "uname": "FanD",
        "medal_level": 18,
        "content": "",
        "count": 1,
        "sample_users": ["FanD"],
        "price": 0
      }
    ]
  }
}
```

---

## Custom OBS Themes

Any directory under `static/themes/<theme-name>/` containing an `index.html` (except `list`, which is reserved for `GET /list`) is automatically discovered by `dambo` and selectable from the capsule UI or `[server].theme` in `dambo_config.toml`.

1. Create `static/themes/my-theme/index.html`, referencing assets via `/static/themes/my-theme/...`.
2. Add `<meta name="referrer" content="no-referrer">` in `<head>` so Bilibili CDN avatar and sticker URLs (`i0.hdslb.com`) load without HTTP `403`.
3. Connect to `ws://${location.host}/ws` and handle `Snapshot`, `Event`, and `RoomState` frames.
4. Select `my-theme` in the capsule UI (or edit `theme = "my-theme"`) and refresh the OBS Browser Source—the server's `no-store` headers ensure instant updates.
