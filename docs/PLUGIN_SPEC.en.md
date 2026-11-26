# Dambo Plugin Technical Specification & Code Generation Contract (For AI)

> **Purpose**: Use this document together with the "Dambo Plugin Requirements Brief" produced via [`PLUGIN_GUIDE.en.md`](PLUGIN_GUIDE.en.md). When the user uploads this file and replies **`Start generating`**, the AI must strictly follow the directory layout, security boundaries, and `/sdk/dambo.js` v2.0.0 APIs below to output complete, zero-build Pure-Web plugin files ready to drop into `static/html/<plugin-name>/`.

---

## 1. AI Generation Rules & Hard Invariants

1. **Zero-Build Pure-Web Runtime**: Plugins run directly in modern browsers, OBS Studio Browser Sources, and Dambo's transparent desktop overlay windows—**no Node/npm build step**. **Never** use `require()`, bare `import`, TypeScript syntax, or non-standard browser globals; use only standard Web APIs and `/sdk/dambo.js`.
2. **Synchronous Single SDK Entry**:
   - Every `plugin.html` and custom `manage.html` must load the SDK synchronously before any plugin scripts:
     ```html
     <script src="/sdk/dambo.js"></script>
     ```
     **Never** add `defer`, `async`, or `type="module"`.
   - **All capabilities go through `window.Dambo`**: use `Dambo.connect()` for live streams, and `Dambo.config`, `Dambo.storage`, `Dambo.memory`, `Dambo.topics`, `Dambo.acl` for state/config/topics/permissions. **Never** bypass the SDK with raw `new WebSocket('/ws')` or manual `fetch('/api/v1/...')`.
3. **Relative `./` Paths for Local Assets & Zero Hardcoded Hosts/Ports**:
   - **Never use leading `/` paths for plugin-local files**: every plugin `.css`, `.js`, and `assets/` reference (both in HTML `<link>` / `<script>` tags and inside JS for images/audio) **must** use a `./`-prefixed relative path (`./plugin.css`, `./plugin.js`, `./manage.css`, `./manage.js`, `./assets/...`), resolved against `<base href="/{plugin-name}/">` in `<head>`.
   - **Never** write `/plugin.css`, `/plugin.js`, `/manage.js`, `/{plugin-name}/plugin.js`, or `/static/html/{plugin-name}/...` (leading-slash absolute paths bypass `<base href>` and hit `404` at server root); **the ONLY allowed leading-`/` asset path is `<script src="/sdk/dambo.js"></script>`**.
   - **Never** hardcode `localhost`, `127.0.0.1`, or port numbers (such as `3838`) anywhere; omit `url` when calling `Dambo.connect()` (or pass `Dambo.utils.getQueryParam('ws') || undefined`) so the SDK derives the endpoint from `window.location`.
4. **Strict Referrer Policy Isolation (Common Pitfall)**:
   - `plugin.html` (view page) `<head>` **must** include `<meta name="referrer" content="no-referrer">`; otherwise Bilibili avatars (`face`) and emotes (`emoticon`/`emots`) are blocked by CDN with `403`.
   - Custom `manage.html` (management page) **must NOT** include `no-referrer`, because management authentication relies on same-origin `Referer: /{plugin-name}/m`.
5. **No `.innerHTML` Anywhere (XSS Prevention)**:
   - **Never** pass user-controlled fields (`uname`, `content`, `room_uname`, `room_title`, `medal_name`) or cross-plugin strings (`reason`, `note`, `key`, `value`, `description`) into `.innerHTML`.
   - Always use `.textContent`, `document.createTextNode()`, `el.replaceChildren()`, `Dambo.utils.createAvatar()`, or `Dambo.utils.renderContent(el, item)`.
6. **Separation of View & Management Responsibilities**:
   - `plugin.html` is a broadcast overlay: keep `html, body { background: transparent !important; overflow: hidden; }`, **start automatically with zero clicks** (no settings buttons, modals, or `:hover` menus), and **keep the top-right `100px × 44px` region unobstructed** (reserved for Dambo's desktop floating-window control bar).
   - If the plugin only needs configuration forms, data inspection, or cross-plugin ACL approval, **do NOT write `manage.html`**—simply declare `defaults` in `plugin.json` and Dambo automatically renders the built-in management console at `/{plugin-name}/m`. Generate `manage.html` + `manage.css` + `manage.js` only when custom interactive controls are needed (e.g., "manual score +/-, lucky draw, next question, one-click reset panel").

---

## 2. Directory Layout, Routes & `plugin.json` Schema

### 2.1 Plugin Directory & Recognition Rules
Place plugins under `static/html/<plugin-name>/` (auto hot-reloaded in ~200ms). `<plugin-name>` must match `^[A-Za-z0-9_-]{1,64}$` and must not collide with reserved top-level routes (`ws`, `api`, `sdk`, `admin`).

```text
static/html/<plugin-name>/
├── plugin.html     # [Required] OBS view page (legacy index.html is no longer supported), served at GET /{plugin-name}/ (e.g. http://localhost:3838/default/)
├── plugin.json     # [Recommended] Manifest, recommended OBS dimensions, and typed defaults
├── plugin.css      # [Optional] View stylesheet
├── plugin.js       # [Optional] View script
├── manage.html     # [Optional] Custom manage page, served at GET /{plugin-name}/m (falls back to built-in console if omitted)
├── manage.css      # [Optional] Manage stylesheet
├── manage.js       # [Optional] Manage script
└── assets/         # [Optional] Local images, fonts, sounds, etc.
```

### 2.2 `plugin.json` & Typed `defaults`
`plugin.json` is read-only at runtime (**do NOT include a `permissions` field**—cross-plugin permissions are requested at runtime via `Dambo.acl` and approved on the management page):

```json
{
  "plugin_id": "gift-goal",
  "version": "1.0.0",
  "name": "Gift Battery Goal Bar",
  "description": "Tracks room gifts and displays real-time goal progress with customizable target and theme",
  "obs": {
    "width": 460,
    "height": 180
  },
  "defaults": {
    "goal_cny": {
      "type": "number",
      "value": 100,
      "meta": { "min": 1, "max": 100000, "step": 10, "format": "integer", "label": "Goal Amount (CNY)" }
    },
    "title": {
      "type": "string",
      "value": "Today's Battery Goal",
      "meta": { "max_length": 40, "label": "Progress Title" }
    },
    "accent_color": {
      "type": "string",
      "value": "#38bdf8",
      "meta": { "format": "color", "label": "Accent Color" }
    },
    "theme": {
      "type": "enum",
      "value": "dark",
      "meta": { "options": ["dark", "light", "transparent"], "label": "Card Theme" }
    },
    "show_recent": {
      "type": "bool",
      "value": true,
      "meta": { "label": "Show Recent Gift" }
    }
  }
}
```

- `plugin_id` (defaults to `<plugin-name>`) and the major part of `version` form the ownership domain `namedomain = "{plugin_id}@{major}"`; minor/patch upgrades within the same major version preserve persistent data and ACLs automatically.
- `defaults` supports these **6 strong types** (usable in both `defaults` and `storage`/`memory` entries):
  - `number`: supports `meta.min`, `meta.max`, `meta.step`, `meta.format` (`"integer"` | `"slider"`)
  - `string`: supports `meta.max_length`, `meta.placeholder`, `meta.format` (`"color"` | `"textarea"` | `"url"`)
  - `bool`: boolean toggle
  - `enum`: **must** declare `meta.options` (`string[]` or `[{ "value": "...", "label": "..." }]`)
  - `list`: array, supports `meta.item` (nested type definition) and `meta.max_items`
  - `struct`: object, supports `meta.fields` (nested per-field definitions supporting `optional: true` / `nullable: true`; omitted fields default to free-form JSON object)
- All `defaults` entries also support `meta.label` and `meta.description` so the built-in management console renders clean human-readable forms.

---

## 3. Live-Room Event Stream SDK (`Dambo.connect` & `Dambo.utils`)

### 3.1 Connection Options & Lifecycle
```javascript
const client = Dambo.connect({
    channel: 'feed',                 // 'feed' (/ws, best for OBS overlays) | 'event' (/ws/event, shaped batches)
    url: Dambo.utils.getQueryParam('ws') || undefined,
    autoReconnect: true,             // default true: exponential backoff with ±15% jitter + tab visibility wake-up
    replaySnapshot: false,           // default false; when true, replays snapshot items to 'event' and kind handlers
    showLike: null,                  // set false to ignore kind==='like'; recommend null + dynamic config check
    onReset: (snapshot) => {},       // fires before snapshot replay on each Snapshot; clear old DOM/timers here
});
```

### 3.2 Events & Payload Schemas
- **Channel A (`channel: 'feed'`) Events**:
  - `snapshot` (`SnapshotData`): `{ room_id, room_uname, room_title, is_live, connected, items: SnapshotItem[] }` (sent on initial connect, reconnect, room switch, or lag recovery; ring buffer holds recent `chat`/`gift`/`superchat`/`guard`)
  - `room_state` (`RoomStateSnapshot`): `{ room_id, room_uname, room_title, is_live, connected }`
  - `event` (`item, meta`) & per-`kind` listeners: `meta = { isSnapshot: boolean }`; `kind === 'super_chat'` emits both `'super_chat'` and `'superchat'`.
  - **Supported `item.kind` values**: `'chat'` | `'gift'` | `'superchat'` | `'super_chat'` | `'guard'` | `'guard_enter'` | `'enter'` | `'follow'` | `'special_follow'` | `'mutual_follow'` | `'share'` | `'like'` | `'live'` | `'preparing'` | `'room_change'` | `'room_silent'` | `'room_block'` | `'superchat_delete'` | `'warning'` | `'cut_off'` | `'reconnected'`
  - **Key `SnapshotItem` fields**:
    - `id`: monotonic ring-buffer ID (deduplication)
    - `seq`: stream session sequence number
    - `kind`: event type
    - `msg_type?`: `INTERACT_WORD*` subtype (`1` enter, `2` follow, `3` share, `4` special follow, `5` mutual follow)
    - `uid`, `uname`, `face?`, `medal_name?`, `medal_level?`
    - `content`: text payload (danmaku body, `"小心心 x2"`, SC message, etc.)
    - `emoticon?`: large sticker URL; `emots?`: inline emote map `{ "[dog]": "https://..." }`
    - `price`: **gold seeds** (`1000` gold seeds = `¥1` CNY; `0` for free gifts / non-paid events)
    - `num?`: gift quantity or guard months
    - `guard_level?`: `1` Governor (总督), `2` Admiral (提督), `3` Captain (舰长)
    - `ids?`: array of deleted SC IDs (`kind === 'superchat_delete'`)
    - `batch_combo_id?`, `combo_stay_time?`: native gift combo batch ID and stay duration (seconds)
    - `timestamp`: 13-digit Unix millisecond timestamp
- **Channel B (`channel: 'event'`) Events**:
  - `room_info` (`RoomInfoEvent`): full room metadata (`room_id`, `short_id`, `title`, `area_name`, `live_status`, `start_time`)
  - `batch` (`EventBatchData`) / `batch_item` (`ev, batch`): `priority` `0` (periodic aggregation), `1` (pass-through / settled combos), `2` (critical status / SC / guard); `ev` carries aggregated `count` and `sample_users` (up to 5 names), with `face` included **only on paid events**.

### 3.3 Essential `Dambo.utils` Helpers
Prefer built-in SDK helpers over reimplementing parsing/formatting/DOM safety logic:

| Method | Purpose & Return Value |
| --- | --- |
| `Dambo.utils.isSuperChat(itemOrKind)` | Matches both `'superchat'` and `'super_chat'` |
| `Dambo.utils.isPaidEvent(item)` | True for SC, guard, or paid gift (`price > 0`) |
| `Dambo.utils.formatPrice(priceSeeds, symbol='¥')` | Converts gold seeds (`1000 = ¥1`) to formatted price (e.g., `'¥30'`), `''` if `<= 0` |
| `Dambo.utils.formatGift(item)` | Returns `{ giftName, giftId, batchComboId, coinType: 'gold'|'silver', isPaid, num, comboNum, totalCoin, comboTotalCoin, unitPrice, priceSeeds, comboStayTimeMs, mergeKey }` |
| `Dambo.utils.formatGuard(item)` | Strips `<%...%>` markup and returns `{ guardLevel, guardName, badgeLabel, num, monthsSuffix, actionText, highlightText, summaryText }` |
| `Dambo.utils.formatInteract(item)` | Returns `{ kind, msgType, guardLevel, guardName, actionText, summaryText }` |
| `Dambo.utils.createGiftComboTracker(opts)` | `4000ms` sliding-window combo merger (`{ windowMs, useServerComboStayTime, canMerge, onCreate, onUpdate, onExpire }`), exposing `.push(item, { isSnapshot })`, `.expire(key)`, `.pruneExpiredSnapshots()`, `.clear()`, `.size()` |
| `Dambo.utils.createAutoScroll(containerEl, opts)` | Auto-scroll bottom follower with manual scroll-up pause lock; `.schedule(forceOpts?)`, `.isFollowing()`, `.isNearBottom()`, `.reset()`, `.destroy()` |
| `Dambo.utils.createTimerBag()` | Managed timer pool: `.set(fn, delayMs)`, `.clear(id)`, `.clearAll()`, `.size()` |
| `Dambo.utils.superChatDurationMs(item)` | Recommended display duration in ms (`guard`: 90s; `<100`: 60s; `>=100`: 120s; `>=500`: 180s; `>=1000`: 300s) |
| `Dambo.utils.remainingDurationMs(item, totalMs, opts)` | Remaining lifetime for snapshot replay (`<= 0` means expired) |
| `Dambo.utils.createAvatar(faceOrItem, className)` | Builds a lazy-loaded `<img>` with HTTPS upgrade and built-in SVG fallback avatar |
| `Dambo.utils.renderContent(parentEl, item, opts)` | Safely renders text, `emoticon`, and inline `emots` (zero `innerHTML`) |
| `Dambo.utils.formatRoomBadge(roomState)` | Formats room status badge label |
| `Dambo.utils.getQueryParam(name, fallback)` / `getQueryBool(name, fallback)` | Reads URL query parameters safely (great for overriding `defaults`) |
| `Dambo.utils.normalizeUrl(url)` / `guardLevelName(level)` | Upgrades URL to HTTPS / returns guard level name |

---

## 4. Storage, Config Sync, Topics & ACL (`Dambo` v2 Platform API)

### 4.1 Mapping Requirements to Hot/Cold Storage
- **"Keep progress / settings permanently across restarts"** → Use `Dambo.config` (typed settings merged with `plugin.json` `defaults`) or `Dambo.storage` (persistent namespace stored in `var/dambo.redb`, survives restarts and offline transitions).
- **"Reset automatically on OBS refresh or restart"** → Use in-memory JS variables (single-page state only) or `Dambo.memory` (ephemeral state shared between `plugin.html` and `manage.html`, cleared on restart).
- **View ↔ Manage Real-Time Sync**: Watch changes on the view side via `Dambo.config.watch(...)`, `Dambo.storage.watch(ns, cb, opts?)`, or `Dambo.memory.watch(ns, cb, opts?)`. Writes from the management page are pushed over a lazy shared SSE stream (`/api/v1/events`) to all open view/manage tabs.

### 4.2 Exact API Signatures (`window.Dambo` v2.0.0)

```javascript
// 1. Plugin Context & Discovery
Dambo.plugin.name                            // Directory plugin_name
Dambo.plugin.mode                            // 'view' | 'manage'
await Dambo.plugin.meta({ force?: boolean }) // { plugin_name, plugin_id, version, namedomain, defaults, config, topics, ... }
await Dambo.plugins.list()
await Dambo.plugins.get(target)

// 2. Typed Config (backed by persist namespace "config", merged with defaults)
await Dambo.config.get()                     // Flat { key: value, ... } object
await Dambo.config.get('goal_cny')           // Single value
await Dambo.config.set('goal_cny', 200)      // Update & persist single key
await Dambo.config.set({ goal_cny: 200, title: 'New Goal' }) // Atomic batch update
await Dambo.config.reset('goal_cny')         // Reset one key (omit arg to reset all defaults)
await Dambo.config.schema()                  // { key: { type, value, meta, is_overridden } }
const offCfg = Dambo.config.watch((ev) => {})

// 3. Persistent Store (Dambo.storage) & Memory Store (Dambo.memory) — symmetric APIs
await Dambo.storage.namespaces()             // string[]
await Dambo.storage.get(ns, key)             // Returns undefined on KEY_NOT_FOUND; throws DamboError otherwise
await Dambo.storage.getEntry(ns, key)        // { key, type, value, meta? }; returns null on KEY_NOT_FOUND
await Dambo.storage.set(ns, key, value, schema?) // Optional schema: { type, meta? } (owner-only; top-level null forbidden)
await Dambo.storage.delete(ns, key)          // Returns boolean (always use delete instead of writing null)
await Dambo.storage.list(ns, { prefix?, keys?, limit?, withSchema? }) // { key: value } by default; full envelope when withSchema: true
await Dambo.storage.mset(ns, { k1: v1, k2: v2 }) // Max 256 entries per call; single atomic tx in persist
await Dambo.storage.clearNamespace(ns)       // Returns deleted count
const offWatch = Dambo.storage.watch(ns, (ev) => {
    // ev: { store, owner, namespace, op: 'set'|'delete'|'mset'|'clear', keys: string[], values?: object, by, ts }
    // Note: when serialized batch <= 16KB, ev.values[k] is included directly—no extra get() needed
}, { prefix?: string })

// Cross-plugin scoped handles (target accepts plugin_id / plugin_name / namedomain)
const otherStorage = Dambo.storage.of('target-plugin')
const otherMemory  = Dambo.memory.of('target-plugin')

// 4. Plugin-Owned Pub/Sub Topics (real-time 1-to-N fan-out, no history persisted, metadata persisted)
await Dambo.topics.publish(name, payload, { public?: boolean, description?: string }) // default public: false
await Dambo.topics.configure(name, { public: boolean, description?: string })
await Dambo.topics.remove(name)
await Dambo.topics.list(target?)             // default '@self'
const offTopic = Dambo.topics.subscribe(target, name, (msg) => {
    // msg: { owner, topic, payload, ts }
})

// 5. Unified Cross-Plugin Access Control (ACL)
const perm = await Dambo.acl.check(target, resource) // resource examples: 'persist:scores' | 'memory:cmd' | 'topic:round' | 'persist:*'
await Dambo.acl.request(target, { resource, level: 'read'|'write'|'subscribe', reason })
const { incoming, outgoing } = await Dambo.acl.list()
await Dambo.acl.set(requesterNamedomain, { resource, level: 'read'|'write'|'subscribe'|'none', note? }) // /m only
await Dambo.acl.remove(requesterNamedomain, resource) // /m only
const offAcl = Dambo.acl.onChange((ev) => {})

// 6. Remote Management Unlock (only when [server].admin_token is set and remote /m returns 401)
await Dambo.plugin.unlockManage(token)
```

### 4.3 Constraints, Quotas & Auth Notes
- **Identifier Rules**:
  - `namespace` and `topic` allow `[A-Za-z0-9_.-]` only, length `1..=128` bytes;
  - `key` length `1..=128` bytes, no control chars, null bytes, or `/`;
  - **Top-level `null` values are forbidden** (use `delete()` to remove keys).
- **Quotas**: Single entry JSON size `<= 256 KB`; `mset` max `256` entries per call; per-`namedomain` limit of `10,000` keys / `16 MB` separately for `persist` and `memory`; overflow rejects with `Dambo.DamboError` (`code === 'QUOTA_EXCEEDED'`).
- **Management API Boundary**: Do NOT invoke management approval APIs (`Dambo.acl.set` / `Dambo.acl.remove`) automatically from a view page (`/{plugin-name}/`)—those belong strictly on `/{plugin-name}/m`. Normal `Dambo.config.set` / `Dambo.storage.set` / `Dambo.memory.set` work on loopback and unlocked browsers (if `UNAUTHORIZED` occurs on a custom `manage.html`, prompt for `admin_token` and call `Dambo.plugin.unlockManage(token)`).
- **Errors & Cleanup**: Except `get()`/`getEntry()` returning `undefined`/`null` for `KEY_NOT_FOUND`, failures reject with `Dambo.DamboError` (`code`, `status`, `message`). When all `off()` unsubscribers from `watch`/`subscribe`/`onChange` are called, the SDK automatically closes the idle SSE stream.

---

## 5. Standard Code Skeleton & Delivery Checklist

### 5.1 Code Skeleton (Live Config + Persistent State + Event Stream)

When the requirements call for the **default built-in console**, output only `plugin.json`, `plugin.html`, `plugin.css`, and `plugin.js`. When a **custom control panel** is needed, also output `manage.html`, `manage.css`, and `manage.js`.

`plugin.html` standard `<head>` & script order:
```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <meta name="referrer" content="no-referrer" />
  <base href="/<plugin-name>/" />
  <title>Plugin Title</title>
  <link rel="stylesheet" href="./plugin.css" />
</head>
<body>
  <main id="app" class="app"></main>
  <script src="/sdk/dambo.js"></script>
  <script src="./plugin.js"></script>
</body>
</html>
```

Standard view script (`plugin.js`):
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

    // 1. Live-reload config edits from the management console
    Dambo.config.watch(loadAndApplyConfig);

    // 2. Sync persistent/ephemeral state across pages when needed
    Dambo.storage.watch('state', (ev) => {
        if (ev.values && Object.prototype.hasOwnProperty.call(ev.values, 'total_seeds')) {
            totalSeeds = Number(ev.values.total_seeds || 0);
            render();
        } else {
            syncStateFromStore();
        }
    });

    // 3. Connect to the live-room feed (clear transient timers/caches in onReset)
    const client = Dambo.connect({
        channel: 'feed',
        url: Dambo.utils.getQueryParam('ws') || undefined,
        onReset: () => {
            timers.clearAll();
        },
    });

    client.on('gift', (item, meta) => {
        if (meta && meta.isSnapshot) return; // Usually skip historical replay to avoid double-counting
        const g = Dambo.utils.formatGift(item);
        if (!g.isPaid) return;
        // Safely update DOM & persist to storage/memory as needed
    });

    loadAndApplyConfig();
})();
```

> For custom `manage.html`: do **NOT** include `<meta name="referrer" content="no-referrer">` in `<head>`; when accessed at `/{plugin-name}/m`, relative paths `./manage.css` and `./manage.js` resolve naturally to `/{plugin-name}/manage.css` and `/{plugin-name}/manage.js` (you may also include `<base href="/<plugin-name>/">`), keeping `<script src="/sdk/dambo.js"></script>` before `<script src="./manage.js"></script>`.

### 5.2 AI Pre-Delivery Self-Check & User Instructions

Verify all 5 points before outputting the code:
1. All files are complete; `<plugin-name>` contains only `[A-Za-z0-9_-]` and is not `ws`, `api`, `sdk`, or `admin`.
2. `plugin.html` includes `<meta name="referrer" content="no-referrer">` and `<base href="/<plugin-name>/">`; custom `manage.html` (if any) omits `no-referrer`.
3. **Zero `.innerHTML`**, **zero hardcoded hosts/ports**, `<script src="/sdk/dambo.js"></script>` is loaded **synchronously before** plugin scripts, and every local `.css`/`.js`/asset reference (everything except `/sdk/dambo.js`) uses a `./...` relative path (no `/plugin.js`, `/manage.js`, or `/static/html/...` absolute paths).
4. `plugin.json` is valid JSON with no `permissions` field, and the view page stays transparent and clear of the top-right `100×44px` region.
5. End with a clear 3-step install guide:
   - In Dambo's capsule, click **Plugins -> Open Plugins Folder** (`static/html/`), create `<plugin-name>/`, and drop all generated files inside;
   - No restart needed (~200ms hot-reload)—click **Open on Desktop** or **Copy OBS URL** (such as `http://localhost:3838/default/` for the default overlay or `http://localhost:3838/<plugin-name>/`) and add it as an OBS Browser Source;
   - In the unified control panel (`http://localhost:3838/`), click **Settings / Setup** (or **Manage** in Developer View, or visit `http://localhost:<port>/<plugin-name>/m` directly) to edit settings, manage ACLs, or use your custom control panel.
