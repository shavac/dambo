# Dambo Plugin Architecture & Implementation Guide (`PLUGIN_GUIDE.md`)

> **Target Audience**: AI Coding Assistants (Claude Code / Cursor / Gemini CLI).
> **Objective**: Generate, validate, and ship a zero-error, clickless Dambo OBS / Broadcast Overlay plugin using **only** this document.

---

## 1. Hard Invariants & Architectural Boundaries

1. **Clickless Broadcast Overlay & URL-Driven Configuration**: Plugins are passive graphics overlays rendered inside OBS Browser Source during live streams where mouse clicks and `:hover` states are unreachable:
   - **Zero-Click Auto-Start**: Plugins **MUST** initialize, connect, and render automatically on load. **NEVER** render settings buttons, clickable drawers, dismissible modals, `:hover`-gated controls, or blocking `alert()` dialogs.
   - **URL Query Configuration**: Expose all user-configurable options (e.g., `theme`, `font_size`, `max_items`, `show_like`) via URL query parameters using `Dambo.utils.getQueryParam()` / `Dambo.utils.getQueryBool()`, and document supported parameters in `plugin.json`'s `description`.
   - **Unattended Self-Healing**: Keep `autoReconnect: true` enabled and silently recover from transient network drops or broken media without leaving stuck error overlays on stream.
2. **Pure Browser, No-Build Runtime & Synchronous SDK**: Every plugin under `static/html/<plugin-id>/` runs directly in browsers/OBS with **no build step**. Rely exclusively on standard browser Web APIs and `/sdk/dambo.js`—**NEVER** use Node.js APIs, CommonJS `require()`, bare `npm` module imports, or non-standard host/desktop bridge globals. Load `/sdk/dambo.js` via a plain synchronous `<script src="/sdk/dambo.js"></script>` tag **before** your plugin script (never add `defer`, `async`, or `type="module"`).
3. **Zero Hardcoded Hosts or Ports**: **NEVER** hardcode hostnames, IP addresses, or port numbers in plugin HTML, CSS, or JS:
   - Reference the SDK via `/sdk/dambo.js`, add `<base href="/{plugin-id}/">` inside `<head>`, and reference plugin assets via relative paths (`./plugin.css`, `./plugin.js`, and optional static files under `./assets/`, such as images, fonts, or audio), ensuring both `GET /{plugin}` (no trailing slash) and `GET /{plugin}/` (with trailing slash) resolve assets identically.
   - When calling `Dambo.connect()`, omit `url` so the SDK automatically derives the WebSocket endpoint from `window.location` (or allow optional query override via `url: Dambo.utils.getQueryParam('ws') || undefined`).
   - All server bind address and port settings live in the host configuration file (`etc/config.toml` under `[server]` `host` and `port`).
4. **Required `<meta name="referrer" content="no-referrer">`**: Bilibili avatar (`face`) and emote (`emoticon`, `emots`) CDNs reject requests carrying a `Referer` header. Every `index.html` **MUST** include `<meta name="referrer" content="no-referrer">` inside `<head>`.
5. **XSS Prohibition & DOM Performance**: **NEVER** assign user-controlled fields (`uname`, `content`, `room_uname`, `room_title`, `medal_name`) to `.innerHTML`. Always use `.textContent`, `document.createTextNode`, or `Dambo.utils.renderContent(el, item, opts)`. Prefer `.textContent` over `.innerText` to avoid layout reflows in high-frequency streams.
6. **Transparent Background Default**: For OBS and desktop floating overlays, keep `html, body { background: transparent !important; overflow: hidden; }`.
7. **Desktop Preview Control Bar Safe Area**: When previewed in desktop overlay mode (`plugin-overlay-*`), the host renders a floating control bar (`#dambo-overlay-host`, `z-index: 2147483647`) at `top: 8px; right: 8px` (`100px × 44px`). Keep the **top-right `100px × 44px`** corner free of critical text or badges so desktop preview windows do not clip content.

---

## 2. Plugin Directory Structure & Host Discovery Contract

Dambo dynamically scans `static/html/` (configurable via `[server].html_dir` in `etc/config.toml`) and hot-reloads changes (~200ms debounce). New plugins are automatically served over HTTP at the configured `[server]` `host` and `port` and registered in the desktop UI and tray menu without modifying any host files.

### 2.1 Directory Layout

```text
static/html/<plugin-id>/
├── index.html      # [REQUIRED] Must be a regular file; triggers auto-discovery
├── plugin.json     # [RECOMMENDED] Metadata, query param docs & OBS overlay dimensions
├── plugin.css      # [OPTIONAL] Plugin styles
├── plugin.js       # [OPTIONAL] Plugin logic using /sdk/dambo.js
└── assets/         # [OPTIONAL] Static assets (images, fonts, audio, etc.) referenced via ./assets/<file>
```

### 2.2 Discovery, Routing & Manifest Rules

- **Valid `<plugin-id>`**: Must match `^[A-Za-z0-9_-]+$` and must **NOT** collide (case-insensitive) with reserved top-level routes: `ws`, `api`, `sdk`, `static`, `assets`, `health`.
- **`plugin.json` Schema**:
  ```json
  {
    "title": "Human-Friendly Name",
    "description": "Short summary and supported URL query params (e.g. ?max_items=40&show_like=false)",
    "obs": {
      "width": 420,
      "height": 720
    }
  }
  ```
  - `obs.width` and `obs.height` take effect only if `> 0`; otherwise the desktop overlay defaults to `420 × 720`.
  - Missing or malformed `plugin.json` never blocks plugin loading—the host logs a warning and falls back to the directory name and default dimensions.
- **HTTP Server Behavior**:
  - Routes served: `GET /{plugin}`, `GET /{plugin}/`, `GET /{plugin}/{*path}`, `GET /sdk/dambo.js`, `GET /ws`, `GET /ws/event`.
  - All HTTP responses include `Cache-Control: no-store, no-cache, must-revalidate` and `Pragma: no-cache`.
  - Path traversal (`..`, `\`, `\0`) returns `404 Asset not found`.

---

## 3. WebSocket Channels & SDK Contract (`/sdk/dambo.js`)

### 3.1 Channel A (`channel: 'feed'`, `/ws`) vs. Channel B (`channel: 'event'`, `/ws/event`)

- **Channel A (`/ws` — Real-time Overlay Feed)**:
  - Immediately sends a `Snapshot` envelope on connect (and re-sends `Snapshot` automatically if the client lags behind the broadcast buffer).
  - Never emits `"combo_end"`, `"room_info"`, or `"session_end"`.
  - `Snapshot.items` ring buffer retains only `"chat" | "gift" | "superchat" | "super_chat" | "guard"`.
  - `enter` events are omitted when `[feed].show_enter == false`; `medal_name` / `medal_level` are omitted when `[feed].show_medal == false`.
- **Channel B (`/ws/event` — Aggregated / Settled Event Batches)**:
  - Immediately sends `RoomInfo` on connect, followed by `EventBatch` frames (`priority: 0` = periodic reservoir batch, `1` = immediate signal / quality chat / gift / combo flush, `2` = critical status / SC / Guard).
  - Retains `face` only on paid events (`"superchat" | "super_chat" | "guard"`, or `"gift"` with `price > 0`).

### 3.2 Type Definitions (JSDoc)

> **Event Kind Helpers**: Always prefer `Dambo.utils.isSuperChat(item)` (which matches both `'superchat'` and `'super_chat'`) and `Dambo.utils.isPaidEvent(item)` (which matches SuperChat, Guard, and paid gifts with `price > 0`) instead of writing manual string comparisons.

```javascript
/**
 * @typedef {Object} RoomStateSnapshot
 * @property {number} room_id
 * @property {string} room_uname
 * @property {string} room_title
 * @property {boolean} is_live
 * @property {boolean} connected
 */

/**
 * @typedef {Object} SnapshotItem
 * @property {number} id
 * @property {number} seq
 * @property {string} kind `'chat'` | `'gift'` | `'superchat'` | `'super_chat'` | `'guard'` | `'guard_enter'` | `'enter'` | `'follow'` | `'special_follow'` | `'mutual_follow'` | `'share'` | `'like'` | `'live'` | `'preparing'` | `'room_change'` | `'room_silent'` | `'room_block'` | `'superchat_delete'` | `'warning'` | `'cut_off'` | `'reconnected'`
 * @property {number=} msg_type
 * @property {number} uid
 * @property {string} uname
 * @property {string=} face
 * @property {string=} medal_name
 * @property {number=} medal_level
 * @property {string} content
 * @property {string=} emoticon Big sticker image URL when the message is an emoticon sticker
 * @property {Object.<string, string>=} emots Inline text emote map keyed by bracketed placeholders (e.g. `{"[dog]": "https://..."}`)
 * @property {number} price Gold seeds (`1000` = `¥1` CNY; `0` for free gifts / non-paid events)
 * @property {number=} num Gift quantity or Guard subscription months
 * @property {number=} guard_level `1`=总督, `2`=提督, `3`=舰长
 * @property {number[]=} ids Deleted SuperChat IDs (present when `kind === 'superchat_delete'`)
 * @property {number} timestamp 13-digit Unix timestamp in ms
 */

/**
 * @typedef {Object} SnapshotData
 * @property {number} room_id
 * @property {string} room_uname
 * @property {string} room_title
 * @property {boolean} is_live
 * @property {boolean} connected
 * @property {SnapshotItem[]} items
 */

/**
 * @typedef {{type: 'Snapshot', data: SnapshotData} | {type: 'Event', data: SnapshotItem} | {type: 'RoomState', data: RoomStateSnapshot}} ChannelAMessage
 */

/**
 * @typedef {Object} RoomInfoEvent
 * @property {number} seq Fixed `0`
 * @property {number} offset_ms Fixed `0`
 * @property {number} timestamp
 * @property {number} room_id
 * @property {string} room_uname
 * @property {string} kind Fixed `"room_info"`
 * @property {number} uid
 * @property {string} uname
 * @property {string} content
 * @property {number=} short_id
 * @property {string=} title
 * @property {string=} area_name
 * @property {string=} parent_area_name
 * @property {number=} live_status `1` = live, `0` = offline, `2` = loop/round
 * @property {number=} start_time Unix timestamp in seconds (`undefined` if offline)
 * @property {string} raw_cmd Fixed `"ROOM_INFO"`
 */

/**
 * @typedef {Object} EventItem
 * @property {number} seq
 * @property {string} kind
 * @property {number=} msg_type
 * @property {number} uid
 * @property {string} uname
 * @property {string=} face Retained only on paid events (`Dambo.utils.isPaidEvent(item)`)
 * @property {number} medal_level
 * @property {string} content
 * @property {number} count Aggregated count / settled combo total / `1` for single events
 * @property {string[]} sample_users Up to 5 distinct representative nicknames in the bucket
 * @property {number} price Gold seeds (`1000` = `¥1` CNY)
 * @property {number=} guard_level
 * @property {string=} batch_combo_id
 * @property {number[]=} ids
 */

/**
 * @typedef {Object} EventBatchData
 * @property {number} batch_id
 * @property {number} room_id
 * @property {string} room_uname
 * @property {0|1|2} priority
 * @property {number} timestamp
 * @property {EventItem[]} events
 */

/**
 * @typedef {{type: 'RoomInfo', data: RoomInfoEvent} | {type: 'EventBatch', data: EventBatchData}} ChannelBMessage
 */

/**
 * @typedef {'connecting' | 'open' | 'reconnecting' | 'closed'} ConnectionState
 */

/**
 * @typedef {Object} EventMeta
 * @property {boolean} isSnapshot
 * @property {EventBatchData=} batch Present when dispatched on `channel: 'event'`
 */

/**
 * @typedef {Object} ConnectOptions
 * @property {'feed' | 'event'=} channel default `'feed'`
 * @property {string=} url Omit to auto-derive from `window.location` (or pass `Dambo.utils.getQueryParam('ws') || undefined`)
 * @property {boolean=} autoReconnect default `true`
 * @property {number=} reconnectDelayMs default `2000` (clamped min `200`)
 * @property {number=} maxReconnectDelayMs default `10000`
 * @property {number=} connectTimeoutMs default `8000` (`0` disables watchdog)
 * @property {boolean=} replaySnapshot default `false`
 * @property {boolean|null=} showLike default `null` (`false` suppresses `kind === 'like'`)
 * @property {((snapshot: SnapshotData) => void)=} onReset Invoked on `'Snapshot'` before history replay
 */

/**
 * @typedef {Object} RenderContentOptions
 * @property {string=} stickerClass default `'dambo-sticker'`
 * @property {string=} emotClass default `'dambo-emot'`
 */

/**
 * @typedef {Object} GuardFormattedInfo
 * @property {number} guardLevel `1`=总督, `2`=提督, `3`=舰长, `0`=未知
 * @property {string} guardName `'总督'` | `'提督'` | `'舰长'` | `'大航海'`
 * @property {string} badgeLabel `'总督'` | `'提督'` | `'舰长'` | `'上舰'`
 * @property {number} num
 * @property {string} monthsSuffix `' ×N个月'` when `num > 1`, else `''`
 * @property {string} actionText e.g. `'开通了'` | `'自动续费了'` | `'续费了'` | `''`
 * @property {string} highlightText e.g. `'舰长'` | `'3个月提督'` | `'舰长 ×2个月'`
 * @property {string} summaryText `actionText + highlightText`
 */

/**
 * @typedef {Object} RemainingDurationOptions
 * @property {boolean=} isSnapshot default `true` (`false` returns `totalMs`)
 * @property {number=} minMs default `0`
 * @property {number=} now default `Date.now()`
 */

/**
 * @typedef {Object} GiftFormattedInfo
 * @property {string} giftName Base gift name (trailing `xN` / `×N` stripped; fallback `'礼物'`)
 * @property {string} giftId Gift ID string (`''` when absent)
 * @property {string} batchComboId Native combo batch ID (`''` when absent)
 * @property {'gold' | 'silver'} coinType `'gold'` (paid) or `'silver'` (free)
 * @property {boolean} isPaid `coinType === 'gold'`
 * @property {number} num Incremental send quantity (`0` if omitted)
 * @property {number} comboNum Cumulative native combo count (`0` if omitted)
 * @property {number} totalCoin Per-event total gold seeds (`0` if omitted)
 * @property {number} comboTotalCoin Cumulative native combo gold seeds (`0` if omitted)
 * @property {number} unitPrice Unit price in seeds (`0` if omitted)
 * @property {number} priceSeeds Event `price` field in gold seeds (`0` for free)
 * @property {string} mergeKey `'batch:<id>'` or `'combo:<user>|<gift>|<coinType>'`
 */

/**
 * @typedef {Object} GiftComboEntry
 * @property {string} key Merge key
 * @property {string} giftName Base gift name
 * @property {string} giftId Gift ID
 * @property {'gold' | 'silver'} coinType Currency classification
 * @property {number} totalCount Accumulated gift count
 * @property {number} totalPriceSeeds Accumulated value in gold seeds (`0` for `'silver'`)
 * @property {number} lastComboNum Latest observed `combo_num`
 * @property {number} lastComboTotalCoin Latest observed `combo_total_coin`
 * @property {number} lastEventTimeMs Latest matched event timestamp (ms)
 * @property {*} data Caller-attached state (e.g. DOM element references)
 */

/**
 * @typedef {Object} GiftComboTrackerOptions
 * @property {number=} windowMs Sliding merge window in ms (default `4000`)
 * @property {((entry: GiftComboEntry, item: Object, opts: Object) => boolean)=} canMerge Guard hook before merging into an active entry
 * @property {((entry: GiftComboEntry, item: Object, opts: Object) => void)=} onCreate Invoked when a new combo entry starts
 * @property {((entry: GiftComboEntry, delta: {deltaCount: number, deltaPriceSeeds: number, meta: GiftFormattedInfo}, item: Object, opts: Object) => void)=} onUpdate Invoked when an existing combo entry increments
 * @property {((entry: GiftComboEntry) => void)=} onExpire Invoked on timeout or manual `expire()`
 */

/**
 * @typedef {Object} AutoScrollOptions
 * @property {number=} thresholdPx Distance from bottom to stay pinned (default `24`)
 * @property {boolean=} bindWindowResize Re-pin on `window.resize` when following (default `true`)
 * @property {((following: boolean) => void)=} onStateChange Invoked when follow state toggles
 */
```

### 3.3 Exported Global Object & Public Signatures

```javascript
// Attached to window.Dambo (loaded synchronously via <script src="/sdk/dambo.js"></script>)
{
    VERSION: '1.1.0',
    DamboClient: class DamboClient {
        /** @param {ConnectOptions} [options] */
        constructor(options);
        /** @param {string} eventName @param {Function} handler @returns {() => void} */
        on(eventName, handler);
        /** @param {string} eventName @param {Function} handler @returns {() => void} */
        once(eventName, handler);
        /** @param {string} eventName @param {Function} [handler] @returns {void} */
        off(eventName, handler);
        /** @returns {void} */
        disconnect();
        /** @returns {void} */
        close();
        /** @returns {void} */
        reconnect();
        /** @returns {ConnectionState} */
        getState();
        /** @returns {RoomStateSnapshot | null} */
        getRoomState();
        /** @returns {SnapshotData | null} */
        getSnapshot();
        /** @returns {RoomInfoEvent | null} */
        getRoomInfo();
        /** @returns {string} */
        getUrl();
    },
    /** @param {ConnectOptions} [options] @returns {DamboClient} */
    connect(options),
    utils: {
        DEFAULT_AVATAR_SVG: 'data:image/svg+xml;utf8,<svg xmlns="[http://www.w3.org/2000/svg](http://www.w3.org/2000/svg)" width="24" height="24" viewBox="0 0 24 24" fill="%2364748b"><circle cx="12" cy="8" r="4"/><path d="M4 20c0-4 4-6 8-6s8 2 8 6"/></svg>',
        /** @param {string | null | undefined} url @returns {string} Upgrades Bilibili CDN http:// or // URLs to https:// */
        normalizeUrl(url),
        /** @param {string | SnapshotItem | EventItem | null | undefined} itemOrKind @returns {boolean} True for 'superchat' or 'super_chat' */
        isSuperChat(itemOrKind),
        /** @param {SnapshotItem | EventItem | null | undefined} item @returns {boolean} True for SuperChat, Guard, or gift with price > 0 */
        isPaidEvent(item),
        /**
         * Convert gold seeds (1000 = 1 CNY) into a formatted price string (e.g. '¥30', '¥5.2').
         * Returns an empty string `''` when `priceSeeds <= 0` or non-numeric.
         * @param {number | null | undefined} priceSeeds
         * @param {string} [currencySymbol='¥']
         * @returns {string}
         */
        formatPrice(priceSeeds, currencySymbol),
        /** @param {SnapshotItem | EventItem | Object | null | undefined} item @returns {GiftFormattedInfo} */
        formatGift(item),
        /** @param {number | null | undefined} level @returns {string} 1->'总督', 2->'提督', 3->'舰长', else '' */
        guardLevelName(level),
        /** @param {SnapshotItem | EventItem | null | undefined} item @returns {GuardFormattedInfo} */
        formatGuard(item),
        /** @param {SnapshotItem | EventItem | null | undefined} item @returns {number} Recommended pin duration in ms */
        superChatDurationMs(item),
        /** @param {SnapshotItem | EventItem | {timestamp?: number} | null | undefined} item @param {number} totalMs @param {RemainingDurationOptions | number} [options] @returns {number} Remaining TTL in ms (0 if expired) */
        remainingDurationMs(item, totalMs, options),
        /** @param {string | SnapshotItem | EventItem | null | undefined} faceOrItem @param {string} [className='dambo-avatar'] @returns {HTMLImageElement} */
        createAvatar(faceOrItem, className),
        /** @param {GiftComboTrackerOptions} [options] @returns {{push(item, opts?): GiftComboEntry|null, expire(key, expectedEntry?): boolean, pruneExpiredSnapshots(): void, clear(): void, size(): number}} */
        createGiftComboTracker(options),
        /** @param {HTMLElement | null | undefined} containerEl @param {AutoScrollOptions} [options] @returns {{schedule(forceOpts?): void, isFollowing(): boolean, isNearBottom(): boolean, reset(): void, destroy(): void}} */
        createAutoScroll(containerEl, options),
        /** @returns {{set(fn, delayMs): number|null, clear(id): void, clearAll(): void, size(): number}} */
        createTimerBag(),
        /** @param {RoomStateSnapshot | SnapshotData | null | undefined} roomState @returns {string} */
        formatRoomBadge(roomState),
        /** @param {HTMLElement} parentEl @param {SnapshotItem | EventItem | {content?: string, emoticon?: string, emots?: Object.<string, string>}} item @param {RenderContentOptions} [options] @returns {void} */
        renderContent(parentEl, item, options),
        /** @param {string} name @param {string|null} [fallback=null] @returns {string|null} */
        getQueryParam(name, fallback),
        /** @param {string} name @param {boolean} [fallback=false] @returns {boolean} */
        getQueryBool(name, fallback),
    }
}
```

### 3.4 SDK Event Names & Error / Reconnect Behavior

- **Lifecycle events**: `'open' ()`, `'close' (CloseEvent)`, `'error' (Event | Error)`, `'reconnect' ({ attempt, delayMs })`, `'state' (ConnectionState)`, `'reset' (SnapshotData)`, `'*' (envelope)`.
- **Channel A (`channel: 'feed'`)**: `'reset' (SnapshotData)`, `'snapshot' (SnapshotData)`, `'room_state' (RoomStateSnapshot)`, `'event' (SnapshotItem, EventMeta)`, `<kind>` (exact `item.kind`; `'super_chat'` emits both `'super_chat'` and `'superchat'`).
- **Channel B (`channel: 'event'`)**: `'room_info' (RoomInfoEvent)`, `'batch' (EventBatchData)`, `'batch_item' (EventItem, EventBatchData)`, `'event' (EventItem, EventMeta)`, `<kind>` (exact `item.kind`; `'super_chat'` emits both `'super_chat'` and `'superchat'`).
- **Errors & Reconnect**: JSON parse failures emit `'error'` without crashing. Listener exceptions are isolated per handler. Socket errors or `connectTimeoutMs` watchdog timeouts close the socket and trigger exponential backoff with `±15%` jitter: `clamp(baseDelay ± 15%, 200, maxReconnectDelayMs)`. Browser `'online'` events resume reconnecting sockets immediately, and `'pagehide'` cleanly closes the socket.

---

## 4. Complete End-to-End Minimal Template (`static/html/minimal-demo/`)

Use this 3-file clickless scaffold as the baseline for any new plugin:

### 4.1 `static/html/minimal-demo/plugin.json`

```json
{
  "title": "Minimal Demo Overlay",
  "description": "Clickless OBS overlay using /sdk/dambo.js (query: ?max_items=40&show_like=false)",
  "obs": {
    "width": 400,
    "height": 600
  }
}
```

### 4.2 `static/html/minimal-demo/index.html`

```html
<!DOCTYPE html>
<html lang="zh-CN">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <meta name="referrer" content="no-referrer">
    <title>Minimal Demo Overlay</title>
    <style>
        * { box-sizing: border-box; margin: 0; padding: 0; }
        html, body {
            width: 100%;
            height: 100%;
            background: transparent !important;
            overflow: hidden;
            font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", sans-serif;
            color: #ffffff;
        }
        .shell {
            display: flex;
            flex-direction: column;
            width: 100%;
            height: 100%;
            /* Leave top-right 100x44px safe area for desktop preview hover controls */
            padding: 48px 12px 12px;
            gap: 8px;
        }
        .status {
            font-size: 12px;
            color: #94a3b8;
            text-shadow: 0 1px 2px rgba(0, 0, 0, 0.8);
            white-space: nowrap;
            overflow: hidden;
            text-overflow: ellipsis;
        }
        .feed {
            flex: 1 1 auto;
            min-height: 0;
            display: flex;
            flex-direction: column;
            justify-content: flex-end;
            gap: 6px;
            overflow: hidden;
        }
        .row {
            display: flex;
            align-items: center;
            gap: 6px;
            padding: 6px 10px;
            border-radius: 8px;
            background: rgba(18, 20, 26, 0.75);
            border: 1px solid rgba(255, 255, 255, 0.12);
            font-size: 14px;
            line-height: 1.4;
            word-break: break-word;
        }
        .row.paid {
            border-left: 3px solid #f87171;
        }
        .avatar {
            width: 20px;
            height: 20px;
            border-radius: 50%;
            object-fit: cover;
            flex-shrink: 0;
        }
        .name { color: #7dd3fc; font-weight: 600; flex-shrink: 0; }
        .price { color: #fde047; font-weight: 700; margin-left: auto; flex-shrink: 0; }
        .dambo-sticker { height: 36px; vertical-align: middle; }
        .dambo-emot { height: 18px; vertical-align: middle; }
    </style>
</head>
<body>
    <div class="shell">
        <div id="status" class="status">Connecting...</div>
        <div id="feed" class="feed"></div>
    </div>
    <script src="/sdk/dambo.js"></script>
    <script src="./plugin.js"></script>
</body>
</html>
```

### 4.3 `static/html/minimal-demo/plugin.js`

```javascript
(function () {
    'use strict';

    const statusEl = document.getElementById('status');
    const feedEl = document.getElementById('feed');
    const parsedMax = Number(Dambo.utils.getQueryParam('max_items', '40'));
    const MAX_ITEMS = Number.isFinite(parsedMax) && parsedMax > 0 ? Math.min(200, Math.floor(parsedMax)) : 40;

    function renderRow(item) {
        if (!item) return;

        const row = document.createElement('div');
        row.className = Dambo.utils.isPaidEvent(item) ? 'row paid' : 'row';

        row.appendChild(Dambo.utils.createAvatar(item, 'avatar'));

        const name = document.createElement('span');
        name.className = 'name';
        name.textContent = item.uname || 'Viewer';
        row.appendChild(name);

        const body = document.createElement('span');
        if (item.kind === 'guard') {
            body.textContent = Dambo.utils.formatGuard(item).summaryText;
        } else {
            Dambo.utils.renderContent(body, item);
        }
        row.appendChild(body);

        const priceText = Dambo.utils.formatPrice(item.price);
        if (priceText) {
            const price = document.createElement('span');
            price.className = 'price';
            price.textContent = priceText;
            row.appendChild(price);
        }

        feedEl.appendChild(row);
        while (feedEl.children.length > MAX_ITEMS) {
            feedEl.removeChild(feedEl.firstChild);
        }
    }

    const client = Dambo.connect({
        channel: 'feed',
        url: Dambo.utils.getQueryParam('ws') || undefined,
        replaySnapshot: false,
        showLike: Dambo.utils.getQueryBool('show_like', false),
    });

    client.on('room_state', (roomState) => {
        statusEl.textContent = Dambo.utils.formatRoomBadge(roomState);
    });

    client.on('snapshot', (snapshotData) => {
        feedEl.textContent = '';
        const items = Array.isArray(snapshotData?.items) ? snapshotData.items.slice(-MAX_ITEMS) : [];
        items.forEach(renderRow);
    });

    client.on('chat', renderRow);
    client.on('gift', renderRow);
    client.on('superchat', renderRow);
    client.on('guard', renderRow);
})();
```

---

## 5. Production Patterns from Reference Plugins

When building feature-rich overlays (pinned SuperChat/Guard bars, enter docks, scrollable history, or aggregated Channel B consumers), follow these established patterns:

1. **Bottom-Anchored Scrollable Feed CSS**:
   ```css
   .danmaku-container {
       flex: 1 1 auto;
       min-height: 0;
       display: flex;
       flex-direction: column;
       overflow-y: auto;
       overflow-x: hidden;
       scrollbar-width: none;
   }
   .danmaku-container > :first-child {
       margin-top: auto;
   }
   ```
   After `container.appendChild(div)` or an in-place gift combo update, prune old nodes (`while (container.children.length > MAX_ITEMS) container.removeChild(container.firstChild);`) and use `const autoScroll = Dambo.utils.createAutoScroll(container, { thresholdPx: 24 });` -> `autoScroll.schedule()` to scroll at `requestAnimationFrame` end without overriding manual scroll-up browsing.

2. **Snapshot Reset, Timer Bag & Pinned Card Deduplication**:
   - Use `const timerBag = Dambo.utils.createTimerBag()` for fade-out/TTL timers so `'snapshot'` (or `'reset'`) resets simply call `timerBag.clearAll()`, `giftCombos.clear()`, `autoScroll.reset()`, reset deduplication sets, and clear containers via `el.textContent = ''`.
   - Deduplicate pinned SC/Guard cards using `item.id != null ? 'id:' + item.id : item.kind + ':' + (item.uid || 0) + ':' + (item.timestamp || 0) + ':' + (item.content || '')` so reconnect snapshots do not duplicate active pins.

3. **Remaining TTL Calculation on Snapshot Replay**:
   - **Pinned SC / Guard**: Compute `totalMs = Dambo.utils.superChatDurationMs(item)` and `remainingMs = Dambo.utils.remainingDurationMs(item, totalMs, { isSnapshot: Boolean(meta?.isSnapshot), minMs: 3000 })`; skip if `remainingMs <= 0`.
   - **Ephemeral Enter Dock (`ENTER_TTL_MS = 3000`)**: Compute `delayMs = Dambo.utils.remainingDurationMs(item, ENTER_TTL_MS, { isSnapshot: Boolean(meta?.isSnapshot) })`; skip if `delayMs <= 0`.

4. **Guard & Gift Normalization / Combo Merging**:
   - **Guard (`kind === 'guard'`)**: Call `const g = Dambo.utils.formatGuard(item)` to strip `<%...%>` wrappers, leading `${item.uname}`, and `在主播(的)房间/直播间` prefixes, returning `{ guardName, badgeLabel, actionText, highlightText, summaryText }`.
   - **Gift (`kind === 'gift'`)**: Use `Dambo.utils.formatGift(item)` or `const giftCombos = Dambo.utils.createGiftComboTracker({ windowMs: 4000, onCreate, onUpdate, onExpire })` to merge active gift combos by `batch_combo_id` / `combo_id` or `uid/uname + gift_id/gift_name + coin_type` (keeping `'gold'` and `'silver'` separate, handling both incremental `num` and cumulative `combo_num`/`combo_total_coin`), and call `giftCombos.pruneExpiredSnapshots()` after replaying snapshot history.

5. **Channel B (`/ws/event`) Aggregated Event Consumption Pattern**:
   Use `channel: 'event'` for dashboards, alerts, TTS, or high-density stream summarization where reservoir-aggregated counts (`ev.count`) and representative users (`ev.sample_users`) are needed:
   ```javascript
   (function () {
       'use strict';

       const headerEl = document.getElementById('room-header');
       const listEl = document.getElementById('batch-list');
       const MAX_ITEMS = 50;

       const client = Dambo.connect({
           channel: 'event',
           url: Dambo.utils.getQueryParam('ws') || undefined,
           showLike: Dambo.utils.getQueryBool('show_like', false),
       });

       client.on('room_info', (info) => {
           const status = info.live_status === 1 ? 'LIVE' : 'OFFLINE';
           headerEl.textContent = `${info.room_uname || info.room_id} · ${info.title || ''} [${status}]`;
       });

       client.on('batch', (batch) => {
           if (!Array.isArray(batch?.events)) return;
           for (const ev of batch.events) {
               const row = document.createElement('div');
               row.className = `batch-row priority-${batch.priority} kind-${ev.kind}`;

               const users = Array.isArray(ev.sample_users) && ev.sample_users.length > 0
                   ? ev.sample_users.join('、')
                   : (ev.uname || 'Viewer');
               const countBadge = ev.count > 1 ? ` ×${ev.count}` : '';
               const priceText = Dambo.utils.formatPrice(ev.price);
               const summary = ev.kind === 'guard'
                   ? Dambo.utils.formatGuard(ev).summaryText
                   : ev.content;

               row.textContent = `[P${batch.priority}] ${users}: ${summary}${countBadge}${priceText ? ' (' + priceText + ')' : ''}`;
               listEl.appendChild(row);
               while (listEl.children.length > MAX_ITEMS) {
                   listEl.removeChild(listEl.firstChild);
               }
           }
       });
   })();
   ```

---

## 6. Agent Self-Verification Checklist

After generating or modifying a plugin under `static/html/<plugin-id>/`, the AI Agent **MUST** run both static and dynamic verification before completing the task:

### 6.1 Static Audit

1. **Clickless Compliance, HTML Head & Synchronous SDK Tag**:
   - Confirm `static/html/<plugin-id>/index.html` contains `<meta name="referrer" content="no-referrer">` inside `<head>`.
   - Confirm `<script src="/sdk/dambo.js"></script>` appears **before** the plugin script and has **no** `defer`, `async`, or `type="module"` attributes.
   - Confirm the plugin contains **zero** interactive form/button controls (`<button>`, `<input>`, `<select>`, `onclick`, `alert(`) or `:hover`-gated UI panels.
2. **Forbidden Patterns Scan**:
   - Scan all `.html`, `.js`, and `.css` files in `static/html/<plugin-id>/` to verify zero occurrences of:
     - `.innerHTML`
     - `require(` or bare `import ` statements
     - Hardcoded hostnames, IP literals, or fixed port numbers
   - Verify SDK uses `/sdk/dambo.js` and local plugin assets use `<base href="/<plugin-id>/">` with relative paths (`./plugin.css`, `./plugin.js`, `./assets/...`) or root-prefixed `/<plugin-id>/...`.
3. **Manifest & JS Syntax Check**:
   ```bash
   python3 -m json.tool static/html/<plugin-id>/plugin.json >/dev/null
   find "static/html/<plugin-id>" -maxdepth 2 -name '*.js' -exec node --check {} +
   ```

### 6.2 Dynamic Route Probe

Read the actual `[server]` `host` and `port` from the active Dambo configuration file (`etc/config.toml`) rather than assuming a fixed port (normalizing wildcard bind addresses `0.0.0.0` / `::` to loopback for local probing), and probe the live HTTP endpoints when the server is running:

```bash
# 1. Extract configured host and port dynamically from etc/config.toml
RAW_HOST="$(awk '/^\[server\]/{f=1;next} /^\[/{f=0} f && /^[[:space:]]*host[[:space:]]*=/{sub(/^[^"]*"/,"");sub(/".*$/,"");print;exit}' etc/config.toml)"
SERVER_PORT="$(awk '/^\[server\]/{f=1;next} /^\[/{f=0} f && /^[[:space:]]*port[[:space:]]*=/{sub(/^[^=]*=[[:space:]]*/,"");sub(/[[:space:]#].*$/,"");print;exit}' etc/config.toml)"
SERVER_HOST="${RAW_HOST:-localhost}"
if [ "$SERVER_HOST" = "0.0.0.0" ] || [ "$SERVER_HOST" = "::" ]; then
    SERVER_HOST="localhost"
fi

# 2. Verify GET /<plugin-id>/ and GET /sdk/dambo.js return 200 OK with Cache-Control: no-store
if [ -n "$SERVER_PORT" ]; then
    curl -sS -D - -o /dev/null "http://${SERVER_HOST}:${SERVER_PORT}/<plugin-id>/"
    curl -sS -D - -o /dev/null "http://${SERVER_HOST}:${SERVER_PORT}/sdk/dambo.js"
fi
```