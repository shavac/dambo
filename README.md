# dambo

[English](README.md) | [简体中文](README.zh.md)

A lightweight Bilibili live-room danmaku collector, OBS overlay plugin host, lossless event recorder, and AI-ready traffic-shaping edge node built with Rust and Tauri v2.

Use **dambo** standalone as a transparent OBS danmaku overlay and live-stream recorder, or consume its prioritized `/ws/event` WebSocket stream in downstream bots and AI pipelines.

- **Architecture & PRD**: [DESIGN.md](DESIGN.md)
- **Plugin Development Guide**: [PLUGIN_GUIDE.md](PLUGIN_GUIDE.md)

---

## Features

- **Desktop Control Capsule & System Tray (`dambo-ui`)**
  - Compact floating toolbar with Bilibili QR sign-in, quick room switching (My Room, Following Live, Recent Rooms, or direct URL paste), system tray controls, and live **English / 日本語 / 简体中文** switching.
- **Hot-Reloaded OBS & Desktop Overlays**
  - Auto-discovers web plugins under `static/html/` with ~200ms hot reload. Open any plugin as a transparent OBS Browser Source or launch it directly as an always-on-top desktop overlay window.
- **Lossless JSONL Recording**
  - Records 100% of live-room activity (`chat`, `superchat`, `gift`, `guard`, interactions, and room status events) into session-continuous `.jsonl` files under `var/records/`, with optional sidecar raw packet capture (`--record-raw`).
- **Dual WebSocket Streams**
  - **`/ws` (Visual Feed)**: Low-latency stream with snapshot history (`Snapshot`, `Event`, `RoomState`) powering OBS and desktop overlays.
  - **`/ws/event` (AI Shaped Stream)**: 3-lane event shaping that aggregates short-message spam and gift combos into prioritized batches while passing SuperChats, Guards, and status signals immediately.
- **Headless CLI Mode (`dambo-cli`)**
  - Run everything in terminal or server environments without desktop GUI overhead.

---

## Quick Start

### 1. Prerequisites

- **Rust**: `1.90+` (Cargo Edition 2024)
- **macOS**: Xcode Command Line Tools + `cmake` (`brew install cmake`)
- **Windows**: MSVC Build Tools, WebView2 (pre-installed on Windows 10/11), and `nasm`
- **Linux**: Tauri v2 WebKit/GTK libraries (`libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `libayatana-appindicator3-dev`)

### 2. Run Dambo

**Desktop App (`dambo-ui`)**:
```bash
cargo run -p dambo-ui --release -- --config etc/config.toml
```

**Headless CLI (`dambo-cli`)**:
```bash
cargo run -p dambo-cli --release -- --config etc/config.toml
```

<details>
<summary>CLI Options (<code>dambo-cli</code>)</summary>

| Flag | Default | Description |
| --- | --- | --- |
| `-c, --config <CONFIG>` | `etc/config.toml` | Path to the read-only TOML config file |
| `-r, --room <ROOM>` | *(from state/config)* | Room ID, short ID, or `https://live.bilibili.com/...` URL |
| `--no-login` | `false` | Skip interactive terminal QR login prompt |
| `--record-raw` | `false` | Capture sidecar `*_raw.jsonl` raw packet files |

</details>

### 3. Connect a Room & Add to OBS

1. Scan the QR code with the Bilibili app when prompted (your session is saved locally in `var/session.json`).
2. Enter a room ID / live URL, or pick directly from **My Room**, **Following Live**, or **Recent Rooms**.
3. Open **Plugins** in the desktop capsule or System Tray:
   - Click **Copy OBS URL** and add it as a **Browser Source** in OBS Studio (`http://localhost:3838/default` or any plugin route).
   - Or click **Open Desktop** to float the overlay directly on your screen.
4. *(Optional)* To view `/list` from a tablet or phone on your LAN, set `host = "0.0.0.0"` under `[server]` in `etc/config.toml`.

---

## Built-in Plugins & Custom Overlays

Dambo ships with three ready-to-use web plugins in `static/html/`, powered by the embedded pure-Web SDK (`/sdk/dambo.js`):

| Plugin | Route | Best For |
| --- | --- | --- |
| **`default`** | `/` or `/default` | Transparent OBS danmaku overlay with pinned SuperChat/Guard bar and enter dock |
| **`list`** | `/list` | Interactive monitor feed with search/filters, scroll lock, and sticky SC header |
| **`sc-ticker`** | `/sc-ticker` | Minimal SuperChat & Guard countdown ticker cards |

### Creating Your Own Plugin

Drop a folder into `static/html/<your-plugin>/` following the standard layout (`index.html`, `plugin.json`, `plugin.css`, `plugin.js`, plus optional `assets/` for images/fonts/audio)—it appears in the UI and tray automatically without restarting Dambo.

See [PLUGIN_GUIDE.md](PLUGIN_GUIDE.md) for the complete SDK reference, starter templates, and self-verification rules.

---

## Configuration & Endpoints

Dambo keeps static configuration and runtime files strictly separated:
- **`etc/config.toml`** (read-only): Copy `etc/config.sample.toml` to `etc/config.toml` when running from source (release archives already include a default `etc/config.toml`). Sets `[server]` bind address/port (`localhost:3838`), `[feed]` snapshot size & display flags, `[event]` batching thresholds, and `[record]` naming patterns (`dambo_{room_name}_%Y%m%d_%H%M%S.jsonl`). If omitted, Dambo falls back to built-in defaults.
- **`var/`** (runtime data, gitignored): Stores `state.json` (last room, locale, history), `session.json`, `logs/`, and `records/`.

| Endpoint | Purpose |
| --- | --- |
| `GET /` | Default OBS overlay (`static/html/default/index.html`) |
| `GET /{plugin}` | Auto-discovered plugin entry page |
| `GET /sdk/dambo.js` | Official zero-dependency Pure-Web Plugin SDK |
| `GET /ws` | Channel A WebSocket — real-time overlay feed (`Snapshot`, `Event`, `RoomState`) |
| `GET /ws/event` | Channel B WebSocket — prioritized shaped event batches (`RoomInfo`, `EventBatch`) |

---

## Development & Packaging

```bash
# Run workspace tests
cargo test --workspace

# Run Clippy checks
cargo clippy --workspace --all-targets -- -D warnings

# Package macOS app bundle (./Dambo.app)
chmod +x scripts/package_macos.sh
./scripts/package_macos.sh
```

`scripts/package_macos.sh` supports `--debug`, `--offline`, and `--skip-build`. Multi-platform CI builds and release staging are defined in [.github/workflows/build.yml](.github/workflows/build.yml).

## Workspace Layout

```text
dambo/
├── dambo-core/   # Core library: collector, auth, recorder, event engine, plugin registry, HTTP/WS server & embedded SDK
├── cli/          # Headless CLI binary (dambo-cli)
├── src-tauri/    # Tauri v2 desktop host, tray menu & overlay window manager (dambo-ui)
├── ui/           # Floating control capsule & QR login frontend
├── static/html/  # Hot-reloaded OBS & web plugins (default, list, sc-ticker, custom)
├── etc/          # Static read-only configuration template (config.toml)
└── scripts/      # Packaging helpers
```
