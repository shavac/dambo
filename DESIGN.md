# `dambo` — Product Requirements & System Architecture Design (PRD)

## 1. Project Overview & Ecosystem Positioning

**`dambo`** (Danmaku + Dam + Bot) is the **Bilibili live room danmaku perception and traffic-shaping node** in the local AI livestream assistant ecosystem, written in Rust. In project layout, configuration conventions, and network service patterns (built on `tokio` + `axum`), it stays highly symmetric with its sibling speech-recognition node **`robabel` (default port `3737`)**.

### Three-Node Responsibility Boundaries
* **`robabel` (`:3737`)**: Pure auditory perception node, outputting real-time ASR speech streams (`/ws`) and transparent OBS speech subtitles (`/`).
* **`dambo` (`:3838`)**: Pure interaction perception and traffic-shaping node, responsible for persistent single-room Bilibili live stream collection, lossless full-event recording, noise filtering, transparent OBS danmaku overlay (`/`), optional web danmaku feed (`/list`), and the "dam" aggregated stream for the central controller (`/ws/dam`).
* **`robao` (Central Controller)**: Stateful decision hub that subscribes to both `3737/ws` and `3838/ws/dam`, handling millisecond-level dual-stream time alignment, viewer lifecycle state machines (enter / lurk / ice-break / leave / return), long-term memory storage, anti-interruption arbitration, and LLM/TTS response generation.

> **Architectural Red Line**: `dambo` remains a **stateless edge pipeline**—it never maintains viewer lifecycle state machines internally and contains zero LLM inference logic. Underlying configuration and transport protocols uniformly use numeric `room_id` as the unique room identifier.

---

## 2. Workspace Layout & Project Structure

Organized as a virtual Cargo Workspace, separating the core library, CLI, desktop frontend, Tauri Rust host, and OBS web themes as siblings to avoid nested path conflicts and file-watcher interference:

```text
dambo/
├── Cargo.toml                  # Virtual workspace root manifest (Edition 2024)
├── DESIGN.md                   # Product requirements & architecture design doc
├── dambo_config.toml           # Default runtime configuration file
├── dambo-core/                 # Core shared library (used by both CLI and Tauri UI)
│   ├── Cargo.toml              # Depends on axum (ws), tower-http (fs), tokio, etc.
│   └── src/
│       ├── lib.rs
│       ├── config.rs           # Configuration loading and hot-write persistence
│       ├── auth.rs             # Bilibili cookie validation, QR generation, and polling FSM
│       ├── collector.rs        # Single-room live stream collector & blivedm anti-corruption layer
│       ├── recorder.rs         # Lossless JSONL event recorder (templated naming & auto-rotation)
│       ├── filter.rs           # Ingress noise filtering & lightweight LRU avatar cache
│       ├── hub.rs              # Channel A: Real-time state hub & ring buffer (for /ws)
│       ├── dam_engine.rs       # Channel B: Dam batch aggregation & spillway engine (for /ws/dam)
│       └── server.rs           # Axum HTTP static serving (/, /list) & dual WS endpoints (/ws, /ws/dam)
├── cli/                        # Headless CLI entrypoint ([[bin]] name = "dambo-cli")
│   ├── Cargo.toml
│   └── src/main.rs
├── ui/                         # Tauri desktop frontend assets (control capsule & QR login window)
│   ├── index.html              # Main floating control capsule
│   ├── login.html              # Standalone QR code login sub-window
│   ├── style.css
│   └── app.js
├── src-tauri/                  # Tauri v2 Rust host & system tray (name = "dambo-ui")
│   ├── Cargo.toml
│   ├── tauri.conf.json         # Sets "frontendDist": "../ui"
│   ├── capabilities/default.json
│   └── src/
│       ├── main.rs
│       └── lib.rs
└── static/
    └── themes/                 # HTTP server (:3838) OBS overlay & /list web themes
        ├── default/            # Default transparent OBS danmaku overlay (served at /)
        │   ├── index.html
        │   ├── style.css
        │   └── danmu.js
        ├── sc-ticker/          # OBS theme showing high-priority SuperChat & Guard events only
        └── list/               # Optional streamer web danmaku feed (served at /list)
            ├── index.html
            ├── style.css
            └── feed.js
```

---

## 3. Core Functional Requirements (PRD)

### 3.1 Account Authentication & Native QR Login Window

- **Silent Startup Session Check**: Reads `dambo_session.json` on startup and requests `https://api.bilibili.com/x/web-interface/nav` with cookies to verify `data.isLogin`; if valid, enters working state directly with zero popup interruption.
- **Native Always-On-Top QR Sub-Window (`dambo-ui`)**: When credentials are missing or expired, the backend dynamically creates an independent always-on-top sub-window `bili-qr-login` (`320×380`, loading `ui/login.html`), requests `passport.bilibili.com/x/passport-login/web/qrcode/generate` to render a vector SVG QR code, and polls status every 2 seconds (auto-refreshing expired QR codes). Once confirmed on mobile, it persists `SESSDATA`, `bili_jct`, and `DedeUserID` locally and destroys the QR sub-window.
- **CLI Fallback (`dambo-cli`)**: In headless environments, prints a Unicode terminal QR code directly for login.
- **Account Switching**: Supports clicking the account badge in the control panel or using the tray menu to log out and re-trigger QR login.

### 3.2 Single-Room Collection, Live Detection & Room Switcher UX (`Collector`)

- **Numeric Data-Layer Identifier**: Configuration files and internal event streams uniformly use numeric `room_id` (automatically resolving Bilibili short IDs to real long IDs).
- **Real-Time Live Status Detection (`is_live`)**:
  - **Initial Status Fetch**: On room connection, reads `live_status` from the room info API (`1` = live, `0`/`2` = offline or round/loop) to set initial `is_live`.
  - **Sub-Second Signaling Response**: While connected, receiving `LIVE` immediately sets `is_live = true`, and receiving `PREPARING` sets `is_live = false`, syncing to the control panel via Tauri native IPC (`room_state_changed`) and to external web/OBS clients via `/ws` `Snapshot`.
- **Interactive Display & Room Selection**:
  - **Unified Format & Tri-State Indicator**: Displays `room_id (Streamer Name)` once room metadata is fetched, or `room_id` when disconnected/unresolved:
    - ⚪ **Gray**: Disconnected or reconnecting;
    - 🟡 **Yellow**: Connected to danmaku server, streamer offline (e.g. `🟡 21452505 (StreamerA) · Offline`);
    - 🟢 **Green**: Connected and streaming live (e.g. `🟢 21452505 (StreamerA)`, with subtle breathing pulse).
  - **Live URL Parsing**: Input box accepts full room URLs (e.g. `https://live.bilibili.com/21452505?...`), automatically extracting numeric `room_id` via regex.
  - **On-Demand Dropdown Room Switcher**: Clicking the room selector expands an on-demand dropdown (no background polling) with three quick-access sections:
    1. 🏠 **My Room**: Resolves the logged-in user's own room via `DedeUserID` for one-click connection.
    2. 🔴 **Following Live**: Fetches currently live streamers followed by the logged-in account (showing `room_id (Streamer Name)` and room title) for one-click connection.
    3. **History Rooms**: Lists `history_rooms` entries (with `×` button to remove individual items).
- **Hot Switching & Exponential Backoff Reconnect**: Switching rooms cancels the old collector task, clears the in-memory snapshot, immediately connects to the new room, and persists `dambo_config.toml`; network drops trigger exponential backoff reconnects capped at 60 seconds.
- **Anti-Corruption Layer**: Third-party `blivedm` crate calls are strictly confined inside `collector.rs`, normalizing all outputs into internal `RawBiliEvent` structures.

### 3.3 Lossless Full-Event Recording & Session Rotation (`Recorder`)

- **Pre-Filter Tap**: `recorder` subscribes directly to `collector`'s raw normalized event stream, unaffected by downstream noise filters (e.g. like suppression or low-value gift filtering), appending 100% of room activity as JSONL.
- **Configurable Naming Template & Sanitization**:
  - Supports `{room_name}` (streamer name), `{room_id}`, and `chrono` strftime specifiers (such as `%Y%m%d_%H%M%S`) in `record.filename_pattern`, defaulting to `dambo_{room_name}_%Y%m%d_%H%M%S.jsonl`.
  - Automatically sanitizes cross-platform illegal filename characters (`/ \ : * ? " < > |` and control chars -> `_`) in `{room_name}`, falling back to `{room_id}` if the streamer name is not yet resolved.
- **Dual-Trigger File Rotation**:
  1. **Connect / Switch Room**: Closes the active recording file, creates a new file once room initialization resolves the room name, and resets `offset_ms = 0`.
  2. **On `LIVE` Signal**: If idling in the room before stream start, receiving `live` (`LIVE`) flushes and closes the pre-stream file, resets `offset_ms = 0`, and starts a new live-session file with that `live` event as line 1.
- **Complete Covered Event Kinds (`kind`)**:
  - **Room Status & System**: `live` (`LIVE`), `preparing` (`PREPARING`), `room_change` (`ROOM_CHANGE`), `room_block` (`ROOM_BLOCK_MSG`), `warning` (`WARNING`), `cut_off` (`CUT_OFF`).
  - **Viewer Interactions**: `chat`, `super_chat`, `gift` (all gifts including free ones), `guard`, `enter`, `follow`, `share`, `like`.
- **Dual Timeline Fields**: Every JSONL line includes both `offset_ms` (milliseconds since the current recording file started, for 1:1 offline replay and `robabel` `.srt` alignment) and `timestamp` (13-digit Unix millisecond timestamp).

### 3.4 Ingress Filtering & Avatar Extraction Strategy (`Filter`)

- **Real-Time Noise Filtering**: Applies only to downstream Channel A and Channel B—by default keeps room-enter events (`ignore_enter = false`), suppresses likes (`ignore_like = true`) and free/low-value gifts below `min_gift_coin` (default `1000` gold seeds = `1` CNY), and passes room status events (`live`, `preparing`, `room_change`).
- **Zero-Risk Avatar Extraction & LRU Cache**:
  - Extracts avatar URLs directly from `DANMU_MSG` (preferring `info[0][15].user.base.face`, compatible with `dm_v2`), `SUPER_CHAT_MESSAGE` (`data.user_info.face`), `SEND_GIFT` (`data.face`), and `GUARD_BUY` (`data.face`).
  - **Never** calls HTTP user-info APIs to look up avatars. `filter.rs` maintains a fixed-capacity (e.g. `2,000` entries) in-memory LRU cache `uid -> face_url` to backfill avatars on subsequent avatar-less `INTERACT_WORD` enter events; unknown users omit `face` so the frontend renders a default placeholder.

### 3.5 Axum Dual-Channel Output Architecture (Port `3838`)

`server.rs` hosts a unified async HTTP and WebSocket service via `axum` (sharing `Arc<AppState>`), splitting filtered events on the in-memory bus into two non-blocking channels:

#### Channel A: Web Display & Real-Time Passthrough (`http://<host>:3838/`, `/list` & `ws://<host>:3838/ws` for OBS & Web Monitoring)

- **Axum Routing & Static Hosting**:
  - `GET /`: Mirrors `robabel` conventions, dynamically serving the active theme (`static/themes/<theme>/index.html`) as a transparent OBS Browser Source danmaku overlay with instant runtime theme switching.
  - `GET /list`: Serves `static/themes/list/index.html`, an optional web danmaku feed (ideal for OBS streaming or LAN iPad/mobile monitoring) with zero extra desktop window overhead.
  - `nest_service("/static/themes", ServeDir::new(...))`: Mounts theme static assets via `tower-http` with `Cache-Control: no-store, no-cache, must-revalidate` middleware to prevent OBS Browser Source caching stale styles.
  - `GET /ws` (`WebSocketUpgrade`): Real-time passthrough stream. Immediately sends a `Snapshot` frame containing up to `feed.max_history` (default `300`) recent items on connect so page refreshes preserve context; broadcasts new events in milliseconds with `face` URLs (frontends must include `<meta name="referrer" content="no-referrer">` to prevent Bilibili CDN 403s).
- **`/list` Readability Enhancements**:
  - **Pinned SC / Guard Bar**: SuperChats and Guards pin to a dedicated top bar with countdown progress bars.
  - **Smart Auto-Scroll Lock**: Auto-scrolls to newest messages by default; scrolling up or hovering locks the viewport and reveals a `"↓ N new messages"` jump-to-bottom button.
  - **Multi-Dimensional Filtering**: Filter by All / Chat Only / Gifts & SC / Medal Holders.

#### Channel B: `robao` Central Controller Dam Stream (`ws://<host>:3838/ws/dam`)

- **`GET /ws/dam` (`WebSocketUpgrade`) Traffic Shaping (Dam Engine)**:
  - **Reservoir**: On a `window_ms` (default `2500ms`) timer, deduplicates and counts repeated chat messages by `content` (e.g., `666 ×15`) with representative user lists, batches `enter` events, and emits `DamBatch` with `priority = 0`.
  - **Spillway**: `super_chat`, `guard`, and room status events (`live`, `preparing`, `room_change`, `warning`, `cut_off`) bypass the window and flush immediately at `priority = 2`; high-value `gift` events flush at `priority = 1`.
  - **Backpressure Protection**: Bounded ingress queue (`buffer_capacity = 512`). Under danmaku storms when full, drops non-medal `enter` events first, then normal `chat`, never dropping SC/Guard or room status events and never blocking the collector heartbeat.
  - **Lean Payload**: Strips `face` avatar URLs from `/ws/dam` messages, retaining `uid`, `uname`, `medal_level`, `room_id`, `room_uname`, and local millisecond `timestamp` for `robao` dual-stream alignment and lifecycle computation.

---

## 4. UI & Interaction Design (`dambo-ui`)

1. **Main Control Capsule** (replicating `robabel-ui`'s compact single-row floating capsule):
   ```text
   +---------------------------------------------------------------------------------------------------+
   | [⋮⋮]  🟢 @Account | Room: [ 🟢 21452505 (StreamerA) ▾ ] [Switch] | Theme: default ▾ | [📋 OBS (:3838)] |
   +---------------------------------------------------------------------------------------------------+
   ```
   - Left drag handle (`data-tauri-drag-region`) + logged-in account badge (click to log out and re-scan QR).
   - Room selector & input (tri-state indicator ⚪Disconnected / 🟡Offline / 🟢Live + `room_id (Streamer Name)`, click to expand input + My Room / Following Live / History Rooms panel) + connect/switch button.
   - Controls: OBS theme selector dropdown, `[📋 OBS (:3838)]` one-click copy `http://localhost:3838` button (Dam Engine is always active with no UI toggle).
   - **IPC Decoupling (Critical Rule)**: `dambo-ui` does **not** connect to `/ws` (avoiding deserialization overhead during danmaku storms). It communicates exclusively via Tauri native IPC (`invoke` commands and `tauri::Emitter` events `room_state_changed` / `auth_state_changed`) for minimal system overhead.
2. **System Tray Icon**:
   - Left-click toggles main control capsule visibility; hover tooltip shows current room ID, streamer name, and live status. Right-click menu includes: Show/Hide Panel, Copy OBS URL (`:3838`), Open Web Danmaku List (`:3838/list`), Re-login Bilibili, Open Config/Records/Logs Folders, and Quit `dambo`.

---

## 5. Configuration Specification (`dambo_config.toml`)

```toml
[bilibili]
room_id = 0                    # Target live room ID (numeric; 0 = disconnected; auto-resolves short IDs)
history_rooms = []             # Recently connected numeric room IDs
session_file = "./dambo_session.json"
min_gift_coin = 1000           # Real-time channel gift filter threshold in gold seeds (1000 = 1 CNY; does not affect recorder)
ignore_enter = false           # Whether to drop room-enter events in real-time channels (default: false)
ignore_like = true             # Whether to drop like events in real-time channels (default: true; does not affect recorder)
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
host = "localhost"             # Change to "0.0.0.0" for LAN access (e.g., iPad/mobile /list)
port = 3838                    # Adjacent to robabel's port 3737
html_dir = "static/themes"
theme = "default"              # Active OBS overlay theme served at root /

[log]
level = "info"
path = "./logs"

[record]
enabled = true                 # Enable lossless JSONL recording (tapped pre-filter)
dir = "./rec"
# Placeholders: {room_name} (sanitized streamer name), {room_id}, and chrono strftime specifiers
# Automatically rotates to a new file and resets offset_ms on room switch or LIVE signal
filename_pattern = "dambo_{room_name}_%Y%m%d_%H%M%S.jsonl"
```

---

## 6. Data Contracts (JSONL Recording & WebSocket Protocols)

### 6.1 Local Full-Event Recording Format (`./rec/dambo_StreamerA_YYYYMMDD_HHMMSS.jsonl`)

Line-delimited JSON covering 100% of room events (`live`, `preparing`, `room_change`, `room_block`, `warning`, `cut_off`, `chat`, `super_chat`, `gift`, `guard`, `enter`, `follow`, `share`, `like`):

```json
{"offset_ms":0,"timestamp":1763280000000,"room_id":21452505,"room_uname":"StreamerA","kind":"live","uid":0,"uname":"","content":"直播开始","price":0,"raw_cmd":"LIVE"}
{"offset_ms":1234,"timestamp":1763280001234,"room_id":21452505,"room_uname":"StreamerA","kind":"chat","uid":3829104,"uname":"ViewerA","face":"https://i0.hdslb.com/bfs/face/example.jpg","medal_name":"Starlight","medal_level":12,"content":"Nice play!","price":0,"raw_cmd":"DANMU_MSG"}
{"offset_ms":5420,"timestamp":1763280005420,"room_id":21452505,"room_uname":"StreamerA","kind":"like","uid":9918273,"uname":"FanD","content":"liked the stream","price":0,"raw_cmd":"LIKE_INFO_V3_CLICK"}
```

### 6.2 Channel A: `ws://<host>:3838/ws` (OBS Overlay `/` & Web Feed `/list`; `dambo-ui` uses native IPC)

Immediately pushes a `Snapshot` frame upon connection, followed by real-time event and room state broadcasts:

```json
{
  "type": "Snapshot",
  "data": {
    "room_id": 21452505,
    "room_uname": "StreamerA",
    "is_live": true,
    "connected": true,
    "items": [
      {
        "id": 1024,
        "kind": "chat",
        "uid": 3829104,
        "uname": "ViewerA",
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

### 6.3 Channel B: `ws://<host>:3838/ws/dam` (For `robao` Central Controller)

Broadcast on reservoir window flush (`priority = 0`) or spillway flush (`priority = 1` gifts / `priority = 2` SC, Guard, and live-status changes):

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
        "uname": "ViewerA",
        "medal_level": 12,
        "content": "666",
        "count": 14,
        "sample_users": ["ViewerA", "ViewerB", "ViewerC"],
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
