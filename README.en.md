# dambo — Danmaku Platform for the AI Era

[简体中文](README.md) | [English](README.en.md)

**dambo** is a lightweight, high-performance Bilibili live-room interaction & overlay toolkit combining **zero-code AI plugin customization**, **transparent OBS overlays & desktop floating widgets**, **desktop chat input**, **lossless session recording**, and **real-time event streams** out of the box.

**Turn chat into custom overlays:** No coding skills needed—send our plugin guide docs to any AI assistant (ChatGPT, Claude, Gemini, DeepSeek, etc.), answer 2 quick rounds of multiple-choice questions, and drop the generated folder into `static/html/`. Dambo hot-reloads it in ~**200ms with no restart** and automatically provides a browser management console.

### Documentation & Quick Links

- **AI Custom Plugin Docs**: [`docs/PLUGIN_GUIDE.en.md`](docs/PLUGIN_GUIDE.en.md) (Requirements Interview, [中文](docs/PLUGIN_GUIDE.md)) · [`docs/PLUGIN_SPEC.en.md`](docs/PLUGIN_SPEC.en.md) (Technical Spec & SDK Contract, [中文](docs/PLUGIN_SPEC.md))
- **Architecture, Config & Contributing**: [`docs/PLUGIN_API_DESIGN.md`](docs/PLUGIN_API_DESIGN.md) (Platform Design) · [`etc/config.sample.toml`](etc/config.sample.toml) (Sample Config) · [`CONTRIBUTING.md`](CONTRIBUTING.md) (Dev & Testing Conventions)

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

1. **QR Sign In**: Click the account button on the left of the capsule and scan the QR code with the Bilibili mobile app (your session is persisted locally in `var/dambo.redb`).
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

- **Use in OBS Studio**: Click **Copy OBS URL**, create a **Browser** source in OBS, paste the URL, and set your preferred width and height for transparent overlay rendering.
- **Float on Desktop**: Click **Open on Desktop** to pop out any plugin as a transparent desktop window. Hover near the top-right corner of the overlay to drag it, toggle **Always on Top**, or close it (or press `Esc`).
- **LAN Multi-Device Access**: To open monitor views (such as `/list/`) from a phone or tablet on the same Wi-Fi network, set `host = "0.0.0.0"` under `[server]` in `etc/config.toml`.

### 5. Plugin Management Console (`/{plugin}/m`)

Every plugin exposes a dedicated browser management page at `GET /{plugin}/m` for live configuration and permission control:

- **How to Open**: Click **Manage** on a plugin card in the control capsule, select a plugin under **Plugins -> Open Manage Page** in the system tray, or navigate directly to `http://localhost:3838/{plugin}/m` in your browser.
- **Custom Page or Built-in Console**: If the plugin folder includes `manage.html` (with optional `manage.css` / `manage.js`), Dambo serves that custom UI; otherwise Dambo automatically renders its zero-code built-in console:
  - **Live Config Form**: Renders typed controls from `plugin.json` `defaults`; changes are persisted immediately to `var/dambo.redb` and pushed live to active OBS / desktop overlay views (with per-field and full-reset support).
  - **Cross-Plugin Access Control (ACL)**: Approve, adjust, or revoke incoming data/topic access requests from other plugins, and inspect outgoing permission requests.
  - **Topics & Storage Browser**: Inspect, toggle (`public` / `private`), or remove plugin event topics, and browse/edit/clear typed keys across both `persist` and `memory` namespaces.
- **Security Boundaries**: Local browser document navigation automatically mints an `HttpOnly; SameSite=Strict` admin session cookie, and framing is blocked (`X-Frame-Options: DENY` / `CSP frame-ancestors 'none'`). When exposing `[server].host` beyond loopback (e.g. `0.0.0.0`), configure `[server].admin_token` in `etc/config.toml` so remote browsers unlock management actions via the prompt modal.

---

## Built-in Plugins

Dambo includes four ready-to-use web plugins out of the box:

| Plugin | Route | Use Case |
| --- | --- | --- |
| **`default`** | `/` or `/default/` | **Primary OBS Transparent Overlay**: pinned SuperChat/Guard bar, ephemeral enter dock, 4s sliding gift combo merging with pulse feedback, and auto-follow bottom scroll |
| **`list`** | `/list/` | **Streamer / Moderator Monitor**: category filters (All / Chat / Gift & SC / Medal), manual scroll-up pause with unread counter, and sticky SC header |
| **`sc-ticker`** | `/sc-ticker/` | **SC & Guard Countdown Cards**: compact corner widget focused purely on SuperChats and Guard subscriptions |
| **`game24`** | `/game24/` | **24-Point Poker Chat Game**: deals 4 cards (A–K) each round for viewers to solve via danmaku math expressions (e.g. `(3+3)*(8-4)`), with live scoring & leaderboard; defaults to `300s` per round and supports `?timeout=300&solvable_only=false&settle_time=3.5&skip_time=2.5` |

---

## Build Plugins from Scratch with Any AI Assistant

No coding required—turn an idea into a working plugin in two simple steps with any AI assistant (ChatGPT / Claude / Gemini / DeepSeek, etc.):

1. **Refine Your Idea**: Download [`docs/PLUGIN_GUIDE.en.md`](docs/PLUGIN_GUIDE.en.md) ([中文](docs/PLUGIN_GUIDE.md)) from this repo, upload it to your AI, and describe your idea in one sentence (e.g., *"I want a gift battery goal progress bar"*).
2. **Generate the Plugin**: Answer the AI's 2 rounds of multiple-choice questions to lock down the details, then upload [`docs/PLUGIN_SPEC.en.md`](docs/PLUGIN_SPEC.en.md) ([中文](docs/PLUGIN_SPEC.md)) when prompted and reply **`Start generating`**. Drop the generated folder into `static/html/` and it is ready to use.

> Click **Plugins -> Open Plugins Folder** in Dambo (`static/html/`) and drop in the generated folder (`plugin.html`, `plugin.json`, `plugin.css`, `plugin.js`, plus optional `manage.html` / `manage.css` / `manage.js` and static assets). Dambo hot-reloads it in ~200ms with no restart required. For manual plugin development, see the technical specification **[`docs/PLUGIN_SPEC.en.md`](docs/PLUGIN_SPEC.en.md)** ([中文](docs/PLUGIN_SPEC.md)).

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
- **`var/`** (runtime data): stores the unified embedded database `dambo.redb` (login session, room history, language preference, recording toggle, and plugin storage/ACL/topics), logs (`logs/`), and recordings (`records/`).

### HTTP & WebSocket Endpoints

| Endpoint | Purpose |
| --- | --- |
| `GET /` or `GET /{plugin}/` | Open any built-in or custom plugin view page in a browser / OBS |
| `GET /{plugin}/m` | Open the plugin configuration, storage & ACL management page in a browser |
| `GET /sdk/dambo.js` | Embedded zero-dependency Pure-Web Plugin SDK |
| `GET /api/v1/*` | Layer 1 Plugin Platform HTTP REST + SSE APIs |
| `GET /ws` | Channel A WebSocket — real-time danmaku & overlay event feed (`Snapshot`, `Event`, `RoomState`) |
| `GET /ws/event` | Channel B WebSocket — shaped, combo-settled event batches for bots, TTS & AI pipelines |

