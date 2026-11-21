# dambo

[English](README.md) | [简体中文](README.md.zh)

**`dambo`** (*Danmaku + Event + Bot*) is a Bilibili live-room danmaku collector, isomorphic event recorder, OBS web-plugin server, and intelligent event-shaping edge node written in Rust.

Designed as a **stateless edge pipeline** alongside `robabel` (default port `3737`)—or as a standalone OBS danmaku & recording desktop app—`dambo` captures 100% of live room events to JSONL, hosts hot-reloaded browser plugins at `:3838`, and shapes high-volume danmaku storms into clean prioritized batches for downstream AI orchestration.

For full product requirements and architecture design notes, see [DESIGN.md](DESIGN.md).

---

## Overview & Ecosystem

In the three-node local AI livestream assistant architecture, `dambo` sits symmetrically alongside the audio perception node `robabel` and feeds the stateful central controller `robao`:

| Node | Default Port | Role | Outputs |
| --- | --- | --- | --- |
| **`robabel`** | `:3737` | Stateless audio perception node | Real-time ASR stream (`/ws`), OBS speech subtitles (`/`) |
| **`dambo`** | `:3838` | Stateless live-interaction & traffic-shaping node | OBS web plugins (`/` & `/{plugin}`), raw `/ws`, shaped event batch stream (`/ws/event`), isomorphic JSONL (`var/records/`) |
| **`robao`** | — | Stateful central brain | Subscribes to `3737/ws` & `3838/ws/event` for millisecond alignment, viewer lifecycle FSM, long-term memory, and LLM/TTS replies |

> **Design Boundary**: `dambo` intentionally stays **stateless** regarding viewer lifecycle state machines and contains no LLM inference logic. All configurations and wire protocols use numeric `room_id` as the canonical room identifier.

---

## Architecture

```text
Bilibili Live WebSocket (blivedm + Protobuf v2 + Brotli/Zlib)
                     │
                     ▼
           collector.rs (RawBiliEvent: seq + guard dedup + LRU face cache)
                     │
     ┌───────────────┼──────────────────────┬─────────────────────┐
     │ [record_raw]  │ (100% isomorphic)    │                     │
     ▼               ▼                      ▼                     ▼
*_raw.jsonl     recorder.rs       Channel A: StateHub   Channel B: EventEngine
(+pb_decoded)   var/records/      Ring buffer (300)     3 lanes + combo buckets
                (paid-only face)            │                     │
                        ┌───────────────────┼──────────┐          ▼
                        ▼                   ▼          ▼    ws://:3838/ws/event
                 GET / & /{plugin}   ws://:3838/ws  Tauri IPC (RoomInfo + EventBatch;
                 (PluginRegistry +   (Snapshot /    (room/auth/ paid-only face,
                  notify hot reload)  Event /        app/plugins/ backpressure-safe)
                                      RoomState)     i18n events)
```

---

## Features

- **Zero-Disruption Bilibili QR Auth (`auth.rs`)**:
  - Validates existing cookies silently on startup via `https://api.bilibili.com/x/web-interface/nav` (`dambo-ui` retries transient failures at `2s`, `5s`, and `10s`).
  - Pops up an always-on-top `320×380` SVG QR sign-in window in `dambo-ui` (or prints a Unicode QR code in the terminal for the `dambo` CLI), polling every 2 seconds and auto-refreshing on expiry.
  - Persists `SESSDATA`, `bili_jct`, and `DedeUserID` (along with cached `uname` and `face`) to `var/session.json`.

- **Single-Room Collector, Monotonic Sequencing & Guard Dedup (`collector.rs`)**:
  - Accepts pure numeric room IDs (automatically resolving Bilibili short IDs), full `https://live.bilibili.com/...` URLs, or formatted display labels, and fetches rich `RoomInfoSnapshot` metadata (`short_id`, `uid`, `uname`, `title`, `area_name`, `live_status`, UTC+8 `start_time`).
  - Assigns monotonic per-session sequence numbers (`seq = 0` for `room_info`, then `1, 2, 3...`), and resets cleanly on new live sessions while preserving `(room_id, start_time)` continuity across transient reconnects.
  - Eliminates duplicate Guard billing events directly at ingestion: skips `GUARD_BUY` (pre-warming avatar cache) and deduplicates `USER_TOAST_MSG` / `USER_TOAST_MSG_V2` (`guard_level > 0`) using a 256-entry LRU set keyed by `payflow_id`.
  - Extracts all 5 `INTERACT_WORD` / `INTERACT_WORD_V2` kinds (`enter`, `follow`, `share`, `special_follow`, `mutual_follow`), preserves `like` (`LIKE_INFO_V3_CLICK`) and `guard_enter` (`ENTRY_EFFECT`), decodes `SEND_GIFT` combo fields (`num`, `batch_combo_id`, `combo_stay_time`) and `COMBO_SEND` (`combo_end` internal settle signal), plus lifecycle/moderation commands (`LIVE`, `PREPARING` with `round == 1` loop marker, `ROOM_CHANGE`, `SUPER_CHAT_MESSAGE_DELETE`, `ROOM_SILENT_*`, `ROOM_BLOCK_MSG`, `WARNING`, `CUT_OFF`) and `reconnected`.
  - Maintains an in-memory LRU avatar cache (`uid -> face_url`, default `2000`) without making extra HTTP user-profile requests.
  - Optional sidecar raw packet capture (`[record].record_raw = true` or CLI `--record-raw`) writes `*_raw.jsonl` with Base64 `pb` / `dm_v2` expanded inline as `pb_decoded` / `dm_v2_decoded`.

- **Isomorphic JSONL Recorder & Continuity Protection (`recorder.rs`)**:
  - Subscribes directly to the unified `collector` stream (100% isomorphic with `/ws/event`, writing all events including `like` and free gifts into `var/records/`).
  - Writes `room_info` (`seq = 0`) as line 1, anchors `offset_ms` to `start_time * 1000` when joining an active stream, keeps the same `.jsonl` open across reconnects within the same `(room_id, start_time)` session, and appends a `session_end` summary (`chat_count`, `like_count`, `guard_count`, `gift_total_coin`, `superchat_total_coin`, `enter_count`, `follow_count`, `share_count`) before closing.
  - Applies **paid-only `face` policy** on `.jsonl` and `/ws/event`: retains `face` URLs only for `superchat`, `guard`, and paid `gift` (`price > 0`), omitting `face` on all non-paid interactions and free gifts.

- **Dual-Channel Axum Server, Auto-Discovered Plugins & Hot Reload (`server.rs`, `plugin.rs`, `static/html/`)**:
  - Scans 1st-level subdirectories under `config.server.html_dir` (`static/html/`) containing `index.html` (with valid URL-safe names excluding reserved routes like `ws`, `api`, `sdk`), reads optional `plugin.json` metadata (`title`, `description`, `obs.{width,height}`), sorts built-ins (`default`, `list`) first then alphabetical, and watches recursively via `notify` (~200ms debounce) for live hot reload without restart.
  - Serves `GET /` (`static/html/default/index.html`), `GET /{plugin}`, and `GET /{plugin}/{*path}` (strict `..` path-traversal rejection, fallback of root relative assets to `default/`, and `Cache-Control: no-store, no-cache, must-revalidate` headers).
  - Built-in plugins: **`default`** (`/`, `/default`, transparent OBS overlay with pinned SC/Guard bar & enter dock), **`list`** (`/list`, streamer web monitor with countdown bar, scroll lock & quick filters), and **`sc-ticker`** (`/sc-ticker`, minimal SC/Guard ticker overlay).
  - **Channel A (`/ws`)**: broadcasts full `face` URLs for visual plugins; retains only core visual kinds (`chat`, `gift`, `superchat`, `guard`) in the `Snapshot` ring buffer (`feed.max_history`, default `300`).
  - **Channel B (`/ws/event`)**: sends `RoomInfo` immediately upon connection, then routes events through 3 `EventEngine` lanes:
    - **0ms Passthrough**: lifecycle/moderation events, `superchat`, `guard`, all 5 interact kinds, `guard_enter`, non-combo gifts, and quality `chat` (`>= min_passthrough_chars`, default `4`).
    - **2.5s `Reservoir`**: aggregates `like`, emote-only chat, and short chat (normalized & deduplicated with `count` and `sample_users`).
    - **Dynamic Combo Gift Buckets**: merges gifts sharing `batch_combo_id` and settles immediately on `combo_end` or at `max_combo_wait_secs` (default `8s`).
    - Enforces bounded backpressure (`buffer_capacity`, default `512`) and paid-only `face` trimming.

- **Read-Only Static Config vs Atomic Runtime State (`etc/` & `var/`)**:
  - `etc/config.toml` is strictly read-only (`Arc<Config>`); `dambo` never writes back to `etc/config.toml`.
  - All mutable runtime files live under `var/` (gitignored): `var/state.json` (atomic temp-file write + rename via `StateStore`, persisting `current_room_id`, `locale`, `room_history[<=20]`, and `record_raw`), `var/session.json`, `var/logs/`, and `var/records/`.

- **Decoupled Tauri v2 Desktop Capsule, Host Trilingual i18n & Aligned Tray (`src-tauri/`, `ui/`)**:
  - Compact `780px` single-line always-on-top floating capsule + `320×380` QR popup with real-time **Simplified Chinese (`zh-CN`), Japanese (`ja`), and English (`en`)** switching (OS locale auto-detected on first launch; `static/html/` web plugins remain standalone).
  - Top-bar `Plugin (N) ▾` dropdown & System Tray `Plugin` submenu stay in live sync via `plugins-updated`: inspect metadata cards, **Open on Desktop** (frameless transparent always-on-top desktop overlay window), **Copy OBS URL**, or **Open Plugins Folder**.
  - Subscribes strictly to Tauri IPC (`room_state_changed`, `auth_state_changed`, `app_state_changed`, `plugins-updated`, `i18n-updated`) without touching `/ws`.

---

## Project Structure

```text
dambo/
├── Cargo.toml                  # Virtual Cargo workspace manifest (Edition 2024)
├── DESIGN.md                   # Architecture & PRD specification
├── etc/
│   └── config.toml             # Strictly read-only static configuration template
├── var/                        # Runtime artifacts (gitignored: state.json, session.json, logs/, records/)
├── dambo-core/                 # Shared Rust core library
│   └── src/
│       ├── lib.rs              # DamboRuntime pipeline orchestrator
│       ├── config.rs           # Strictly read-only TOML configuration loader
│       ├── state.rs            # Fault-tolerant atomic JSON persistence for var/state.json
│       ├── plugin.rs           # PluginRegistry: static/html scan, plugin.json & notify hot-reload
│       ├── model.rs            # RoomInfoSnapshot, RawBiliEvent, SessionSummary, Channel A/B contracts
│       ├── auth.rs             # Bilibili cookie validation, SVG/terminal QR login
│       ├── collector.rs        # blivedm & Protobuf v2 collector, seq sequencing, guard dedup & PB decoder
│       ├── recorder.rs         # Isomorphic JSONL recorder, session_end summary & *_raw.jsonl sidecar
│       ├── hub.rs              # Channel A ring buffer & Tauri watch state hub
│       ├── event_engine.rs     # Channel B 3-lane EventEngine & dynamic combo buckets
│       └── server.rs           # Axum unified plugin routing (/, /{plugin}, /{plugin}/{*path}) & dual WS
├── cli/                        # Headless CLI crate (`dambo-cli`, bin `dambo`)
│   └── src/main.rs
├── ui/                         # Tauri desktop frontend (`index.html`, `login.html`, `style.css`, `app.js`)
├── src-tauri/                  # Tauri v2 desktop host & system tray (`dambo-ui`)
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   ├── capabilities/default.json
│   ├── locales/                # Desktop host trilingual i18n dictionaries (`zh-CN.json`, `ja.json`, `en.json`)
│   └── src/
│       ├── main.rs
│       ├── i18n.rs
│       └── lib.rs
├── static/html/                # Auto-discovered OBS & web plugins
│   ├── default/                # Default transparent OBS danmaku overlay (`/` & `/default`)
│   ├── list/                   # Interactive web danmaku monitor (`/list`)
│   └── sc-ticker/              # SC & Guard ticker OBS overlay (`/sc-ticker`)
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
cargo run -p dambo-cli --release -- --config etc/config.toml
```

Available CLI options:

| Flag | Default | Description |
| --- | --- | --- |
| `-c, --config <CONFIG>` | `etc/config.toml` | Path to the read-only TOML configuration file |
| `-r, --room <ROOM>` | *(from state/config)* | Override room ID, short ID, or `https://live.bilibili.com/...` URL |
| `--no-login` | `false` | Skip interactive terminal QR login when unauthenticated |
| `--record-raw` | `false` | Enable sidecar `*_raw.jsonl` capture with decoded `pb_decoded` / `dm_v2_decoded` |

### 2. Desktop Control Capsule (`dambo-ui`)

```bash
# Launch the floating control capsule & system tray app
cargo run -p dambo-ui --release -- --config etc/config.toml
```

`dambo-ui` supports `-c, --config <CONFIG>` (default `etc/config.toml`). Relative config paths are resolved automatically across the current working directory, executable ancestor directories (including macOS `Contents/Resources/`), and the repository root; relative runtime paths like `var/state.json` resolve against the parent project root.

### 3. Connect OBS & Plugins

1. Sign in via the QR prompt (or run CLI with `--no-login` for guest mode where supported by Bilibili).
2. Enter a room ID or paste a `https://live.bilibili.com/<id>` URL (or choose from **My Room** / **Following Live** / **Recent Rooms**).
3. Open the `Plugin` menu in the desktop capsule or System Tray to **Copy OBS URL** (`http://localhost:3838/` or `http://localhost:3838/<plugin>`) or click **Open on Desktop** for a transparent always-on-top desktop overlay window.
4. For a secondary monitor or mobile/tablet viewer on your LAN, set `[server].host = "0.0.0.0"` in `etc/config.toml` and open `http://<your-ip>:3838/list`.

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

## Configuration (`etc/config.toml` & `var/state.json`)

`etc/config.toml` is **read-only**; startup room priority is CLI `--room` > `var/state.json` (`current_room_id`) > `etc/config.toml` (`[bilibili].room_id`):

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
html_dir = "static/html"       # Plugin root directory watched by PluginRegistry
state_file = "var/state.json"  # Runtime persisted state (current_room_id, locale, room_history, record_raw)

[log]
level = "info"                 # off | error | warn | info | debug | trace (RUST_LOG also supported)
path = "var/logs"              # Desktop UI writes rotating logs here (5 MB x 5 files)

[record]
enabled = true                 # Enable isomorphic JSONL recording (shared with /ws/event source)
record_raw = false             # Sidecar raw Bilibili packet capture (*_raw.jsonl with pb_decoded/dm_v2_decoded)
dir = "var/records"
# Placeholders: {room_name} (sanitized streamer name), {room_id}, and chrono strftime specifiers
# Automatically anchors offset_ms and preserves open file across reconnects for the same live session
filename_pattern = "dambo_{room_name}_%Y%m%d_%H%M%S.jsonl"
```

---

## HTTP Endpoints & Data Protocols

### Server Routes (`http://<host>:<port>`)

| Endpoint | Protocol | Purpose |
| --- | --- | --- |
| `GET /` | HTTP | Serves `static/html/default/index.html` (default OBS overlay) |
| `GET /{plugin}` | HTTP | Serves `static/html/{plugin}/index.html` (`/default`, `/list`, `/sc-ticker`, and custom plugins) |
| `GET /{plugin}/{*path}` | HTTP | Serves plugin assets with `no-store` cache headers, traversal checks & `default/` fallback |
| `GET /ws` | WebSocket | **Channel A**: Real-time stream (`Snapshot`, `Event`, `RoomState`, full `face` URLs) |
| `GET /ws/event` | WebSocket | **Channel B**: Always-on shaped stream (`RoomInfo` first frame + `EventBatch`, paid-only `face`) |

### 1. Isomorphic JSONL (`var/records/dambo_<room_name>_YYYYMMDD_HHMMSS.jsonl`)

Line 1 is always `kind: "room_info"` (`seq = 0`), followed by monotonically sequenced standard events and a terminal `kind: "session_end"` summary. `face` is retained **only on paid events** (`superchat`, `guard`, and paid `gift` with `price > 0`):

```json
{"seq":0,"offset_ms":0,"timestamp":1763280000000,"room_id":21452505,"room_uname":"StreamerA","kind":"room_info","uid":123456,"uname":"StreamerA","content":"Late Night Stream","price":0,"start_time":1763280000,"short_id":512,"title":"Late Night Stream","area_name":"Virtual","parent_area_name":"VTuber","live_status":1,"raw_cmd":"ROOM_INFO"}
{"seq":1,"offset_ms":1234,"timestamp":1763280001234,"room_id":21452505,"room_uname":"StreamerA","kind":"chat","uid":3829104,"uname":"Viewer1","medal_name":"Starlight","medal_level":12,"content":"Nice play!","dm_type":0,"price":0,"raw_cmd":"DANMU_MSG"}
{"seq":2,"offset_ms":2100,"timestamp":1763280002100,"room_id":21452505,"room_uname":"StreamerA","kind":"gift","uid":3829104,"uname":"Viewer1","face":"https://i0.hdslb.com/bfs/face/example.jpg","content":"Gift x3","price":300,"num":3,"batch_combo_id":"batch:gift:1","combo_stay_time":3,"raw_cmd":"SEND_GIFT"}
```

### 2. Channel A (`ws://<host>:3838/ws`)

Immediately upon connecting (or if a slow client lags behind), the server sends a `Snapshot` frame containing room metadata and recent core visual items (`chat`, `gift`, `superchat`, `guard`, up to `feed.max_history`, with full `face` URLs). Subsequent updates are pushed as `Event` or `RoomState` frames:

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
        "seq": 1,
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

### 3. Channel B (`ws://<host>:3838/ws/event`)

Sends `RoomInfo` immediately upon connection, followed by shaped `EventBatch` frames (`priority = 0` reservoir, `priority = 1` 0ms social/combo gifts/quality chat, `priority = 2` 0ms lifecycle/moderation/`superchat`/`guard`; `face` retained only for paid events):

```json
{
  "type": "EventBatch",
  "data": {
    "batch_id": 88,
    "room_id": 21452505,
    "room_uname": "StreamerA",
    "priority": 0,
    "timestamp": 1763280002500,
    "events": [
      {
        "seq": 14,
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
        "seq": 15,
        "kind": "like",
        "uid": 9918273,
        "uname": "FanD",
        "medal_level": 18,
        "content": "为主播点赞了",
        "count": 23,
        "sample_users": ["FanD"],
        "price": 0
      }
    ]
  }
}
```

---

## Custom Web Plugins (`static/html/`)

Any 1st-level directory under `static/html/<plugin-name>/` with a URL-safe name (ASCII alphanumeric/`-`/`_`, excluding reserved names like `ws`, `api`, `sdk`) and an `index.html` is **automatically discovered and hot-reloaded** via `notify`:

1. Create `static/html/my-plugin/index.html` (and optional `plugin.json`), referencing assets via `/my-plugin/...` (or relative `./...`):
   ```json
   {
     "title": "My Custom Overlay",
     "description": "Custom OBS widget or interactive overlay.",
     "obs": { "width": 420, "height": 720 }
   }
   ```
2. Add `<meta name="referrer" content="no-referrer">` in `<head>` so Bilibili CDN avatar and sticker URLs (`i0.hdslb.com`) load without HTTP `403`.
3. Connect to `ws://${location.host}/ws` and handle `Snapshot`, `Event`, and `RoomState` frames.
4. No restart is required: as soon as files appear under `static/html/`, the plugin shows up in both the desktop capsule's **Plugin** dropdown and the System Tray **Plugin** menu, ready for **Open on Desktop** or **Copy OBS URL** (`http://localhost:3838/my-plugin`).
