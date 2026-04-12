# Dashboard Restructure & Documentation Update — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split the monolithic 2,841-line `dashboard.html` into focused files using Alpine.js, assembled at compile time, and update ARCHITECTURE.md and README.md to reflect the current codebase.

**Architecture:** Dashboard files live in `src/dashboard/` and are embedded via `include_str!` + `format!` in `dashboard.rs`. Alpine.js (CDN) provides reactive component architecture. Each tab is a separate JS file registering an `Alpine.data()` component. No build step — `cargo build` only.

**Tech Stack:** Rust, Axum, Alpine.js (CDN), Chart.js (CDN), SQLite

---

## File Structure

### Files to create

| File | Responsibility |
|------|---------------|
| `src/dashboard/shell.html` | HTML skeleton: `<head>`, CDN scripts, layout, tab containers with `{placeholders}` for injected content |
| `src/dashboard/styles.css` | All CSS extracted from the `<style>` block (lines 8-384 of current `dashboard.html`) |
| `src/dashboard/app.js` | Alpine.js global store, tab routing, DOMContentLoaded event wiring, state constants |
| `src/dashboard/utils.js` | Shared helpers: `fmt()`, `esc()`, `envelopeData()`, `formatDuration()`, `tryFormatJson()`, normalize/persist functions |
| `src/dashboard/components/websocket.js` | `connectWS()`, reconnect logic, event dispatch to Alpine store |
| `src/dashboard/components/charts.js` | `initCharts()`, `updateCharts()`, `resizeCharts()`, `resetCharts()`, chart theme config |
| `src/dashboard/tabs/overview.js` | `overviewTab` Alpine component: `loadOverview()`, `updateOverview()`, `resetOverview()`, overview mode switching |
| `src/dashboard/tabs/requests.js` | `requestsTab` Alpine component: `loadEntries()`, table rendering, sorting, filtering, search, anomaly focus, request modal, correlation/explanation panels |
| `src/dashboard/tabs/conformance.js` | `conformanceTab` Alpine component: scoreboard placeholder |
| `src/dashboard/tabs/anomalies.js` | `anomaliesTab` Alpine component: anomaly list rendering, severity badges |
| `src/dashboard/tabs/sessions.js` | `sessionsTab` Alpine component: `loadSessions()`, `selectSession()`, `loadSessionDetails()`, conversation rendering, session timeline/graph |

### Files to modify

| File | Changes |
|------|---------|
| `src/dashboard.rs` | Change `serve_dashboard()` from `include_str!("dashboard.html")` to `format!` assembly of all `src/dashboard/*.js/css/html` files |
| `ARCHITECTURE.md` | Full rewrite to reflect 8-module, 2-server, 1-worker architecture |
| `README.md` | Update features, CLI flags, API list, remove deleted features |

### Files to delete

| File | Reason |
|------|--------|
| `src/dashboard.html` | Replaced by `src/dashboard/` directory |

---

## Important: Escape Handling

Rust's `format!` macro interprets `{` and `}` as placeholders. All literal braces in CSS, JS, and HTML must be doubled:
- CSS: `.card { color: red; }` → `.card {{ color: red; }}`
- JS: `const x = { a: 1 };` → `const x = {{ a: 1 }};`
- JS template literals: `` `${x}` `` → `` `${{x}}` ``

**The CSS and JS files will contain these escaped braces.** This is the core trade-off of compile-time assembly with `format!`.

---

### Task 1: Create directory structure and extract CSS

**Files:**
- Create: `src/dashboard/` directory
- Create: `src/dashboard/styles.css`
- Create: `src/dashboard/shell.html` (minimal skeleton to prove assembly works)
- Modify: `src/dashboard.rs` — change `serve_dashboard()` to use `format!` assembly

- [ ] **Step 1: Write a failing test that the assembled dashboard contains the CSS variable `--bg-0`**

Add to `src/dashboard.rs` in the `#[cfg(test)] mod tests` block:

```rust
#[tokio::test]
async fn assembled_dashboard_contains_css_variables() {
    let html = super::assemble_dashboard_html();
    assert!(html.contains("--bg-0"), "CSS variables must be present in assembled output");
    assert!(html.contains("--cyan"), "CSS variables must be present in assembled output");
}
```

Add a stub `assemble_dashboard_html` function in `dashboard.rs` (outside the test module):

```rust
fn assemble_dashboard_html() -> String {
    String::new() // stub
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test assembled_dashboard_contains_css_variables`
Expected: FAIL — empty string doesn't contain `--bg-0`

- [ ] **Step 3: Create the dashboard directory**

```bash
mkdir -p src/dashboard/tabs src/dashboard/components
```

- [ ] **Step 4: Extract CSS into `src/dashboard/styles.css`**

Copy lines 9-383 of `dashboard.html` (everything between `<style>` and `</style>`, not including the tags) into `src/dashboard/styles.css`.

**Critical:** Replace every `{` with `{{` and every `}` with `}}` throughout the CSS file. This is required for Rust's `format!` macro.

For example, the first few lines become:
```css
@import url('https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@300;400;500;600;700&family=DM+Sans:wght@300;400;500;600;700&display=swap');

:root {{
  --bg-0: #0a0b0e; --bg-1: #12131a; --bg-2: #1a1b26; --bg-3: #222338;
  --text-0: #e8e9f0; --text-1: #a9adc1; --text-2: #6c7086;
  --green: #a6e3a1; --yellow: #f9e2af; --red: #f38ba8; --cyan: #89dceb;
  --blue: #89b4fa; --mauve: #cba6f7; --peach: #fab387;
  --border: #2a2b3d; --hover: #2a2b3d;
  --radius: 10px; --mono: 'JetBrains Mono', monospace; --sans: 'DM Sans', sans-serif;
}}
```

- [ ] **Step 5: Create minimal `src/dashboard/shell.html`**

```html
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Claude Code Proxy — Dashboard</title>
<script src="https://cdn.jsdelivr.net/npm/chart.js@4"></script>
<script defer src="https://cdn.jsdelivr.net/npm/alpinejs@3"></script>
<style>
{css}
</style>
</head>
<body>
<div class="header">
  <h1><span>⚡</span> Claude Code Proxy</h1>
  <div class="header-right">
    <span><span class="status-dot disconnected" id="ws-dot"></span><span id="ws-status">Connecting...</span></span>
    <span id="uptime" style="color:var(--text-2)">0:00:00</span>
  </div>
</div>
<div class="container">
  <p style="color:var(--text-1)">Dashboard shell loaded — tabs coming next.</p>
</div>
<script>
{utils_js}
{charts_js}
{websocket_js}
{overview_js}
{requests_js}
{conformance_js}
{anomalies_js}
{sessions_js}
{app_js}
</script>
</body>
</html>
```

- [ ] **Step 6: Create stub JS files**

Create each of these files with a single comment so `include_str!` doesn't fail:

`src/dashboard/utils.js`:
```js
// utils.js — shared helpers (populated in Task 2)
```

`src/dashboard/components/charts.js`:
```js
// charts.js — Chart.js wrappers (populated in Task 4)
```

`src/dashboard/components/websocket.js`:
```js
// websocket.js — WebSocket connection (populated in Task 3)
```

`src/dashboard/tabs/overview.js`:
```js
// overview.js — Overview tab (populated in Task 5)
```

`src/dashboard/tabs/requests.js`:
```js
// requests.js — Requests tab (populated in Task 6)
```

`src/dashboard/tabs/conformance.js`:
```js
// conformance.js — Conformance tab (populated in Task 7)
```

`src/dashboard/tabs/anomalies.js`:
```js
// anomalies.js — Anomalies tab (populated in Task 8)
```

`src/dashboard/tabs/sessions.js`:
```js
// sessions.js — Sessions tab (populated in Task 9)
```

`src/dashboard/app.js`:
```js
// app.js — Alpine.js global store (populated in Task 10)
```

- [ ] **Step 7: Implement `assemble_dashboard_html()` in `dashboard.rs`**

```rust
fn assemble_dashboard_html() -> String {
    format!(
        include_str!("dashboard/shell.html"),
        css = include_str!("dashboard/styles.css"),
        utils_js = include_str!("dashboard/utils.js"),
        charts_js = include_str!("dashboard/components/charts.js"),
        websocket_js = include_str!("dashboard/components/websocket.js"),
        overview_js = include_str!("dashboard/tabs/overview.js"),
        requests_js = include_str!("dashboard/tabs/requests.js"),
        conformance_js = include_str!("dashboard/tabs/conformance.js"),
        anomalies_js = include_str!("dashboard/tabs/anomalies.js"),
        sessions_js = include_str!("dashboard/tabs/sessions.js"),
        app_js = include_str!("dashboard/app.js"),
    )
}
```

**Do NOT change `serve_dashboard()` yet** — keep it pointing at `dashboard.html` so the app still works. We'll switch over in Task 11 after all tabs are populated.

- [ ] **Step 8: Run tests**

Run: `cargo test assembled_dashboard_contains_css_variables`
Expected: PASS — the assembled HTML now contains the CSS with `--bg-0`

- [ ] **Step 9: Commit**

```bash
git add src/dashboard/ src/dashboard.rs
git commit -m "feat: create dashboard directory structure and extract CSS"
```

---

### Task 2: Extract utility functions into `utils.js`

**Files:**
- Modify: `src/dashboard/utils.js`

- [ ] **Step 1: Write a failing test**

Add to `src/dashboard.rs` tests:

```rust
#[tokio::test]
async fn assembled_dashboard_contains_utility_functions() {
    let html = super::assemble_dashboard_html();
    assert!(html.contains("function fmt("), "utils.js must contain fmt()");
    assert!(html.contains("function esc("), "utils.js must contain esc()");
    assert!(html.contains("function formatDuration("), "utils.js must contain formatDuration()");
    assert!(html.contains("function tryFormatJson("), "utils.js must contain tryFormatJson()");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test assembled_dashboard_contains_utility_functions`
Expected: FAIL

- [ ] **Step 3: Populate `src/dashboard/utils.js`**

Extract these functions from `dashboard.html` (lines ~2695-2841) into `utils.js`. These are the shared helpers that multiple tabs use.

**Critical:** All `{` must become `{{` and all `}` must become `}}` in the JS content.

The functions to extract (copy from `dashboard.html`, then escape braces):

- `fmt(n)` (line 2695)
- `esc(s)` (line 2702)
- `envelopeData(payload)` (line 2711)
- `formatDuration(s)` (line 2718)
- `tryFormatJson(text)` (line 1537)
- `getStatusClass(status)` (line 1375)
- `formatSignedDeltaMs(deltaMs)` (line 1290)
- `formatCoverageLabel(mode, start, end)` (line 2618)
- `normalizeOverviewMode(mode)` (line 2630)
- `normalizeTab(tab)` (line 2634)
- `normalizeRequestFilter(filter)` (line 2638)
- `normalizeSearchQuery(value)` (line 2642)
- `loadStoredValue(key, normalize, fallback)` (line 2646)
- `persistStoredValue(key, value)` (line 2655)
- `loadPersistedTab()` (line 2663)
- `persistTab(tab)` (line 2667)
- `loadPersistedRequestFilter()` (line 2671)
- `persistRequestFilter(filter)` (line 2675)
- `loadPersistedSearchQuery()` (line 2679)
- `persistSearchQuery(value)` (line 2683)
- `loadPersistedOverviewMode()` (line 2687)
- `persistOverviewMode(mode)` (line 2691)

Also extract the storage key constants:

```js
const TAB_STORAGE_KEY = 'claude-proxy.active-tab';
const REQUEST_FILTER_STORAGE_KEY = 'claude-proxy.request-filter';
const SEARCH_QUERY_STORAGE_KEY = 'claude-proxy.search-query';
const OVERVIEW_MODE_STORAGE_KEY = 'claude-proxy.overview-mode';
const MAX_TABLE_ENTRIES = 500;
```

- [ ] **Step 4: Run test**

Run: `cargo test assembled_dashboard_contains_utility_functions`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/dashboard/utils.js src/dashboard.rs
git commit -m "feat: extract utility functions into dashboard/utils.js"
```

---

### Task 3: Extract WebSocket logic into `websocket.js`

**Files:**
- Modify: `src/dashboard/components/websocket.js`

- [ ] **Step 1: Write a failing test**

```rust
#[tokio::test]
async fn assembled_dashboard_contains_websocket_logic() {
    let html = super::assemble_dashboard_html();
    assert!(html.contains("function connectWS("), "websocket.js must contain connectWS()");
    assert!(html.contains("ws.onmessage"), "websocket.js must handle onmessage");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test assembled_dashboard_contains_websocket_logic`
Expected: FAIL

- [ ] **Step 3: Populate `src/dashboard/components/websocket.js`**

Extract the `connectWS()` function (lines ~717-756 of `dashboard.html`) and related WebSocket state variables into `websocket.js`.

Extract these variables and the function:
```js
let ws = null;
```

Then the `connectWS()` function body. Remember to escape all `{` → `{{` and `}` → `}}`.

The function currently dispatches to `updateOverview()`, `updateCharts()`, `handleReset()`, `addTableRow()`, and `updateAnomalies()` — these will be global functions available when all scripts are loaded. Keep the function calls as-is; they'll be defined in their respective tab JS files by the time this runs.

- [ ] **Step 4: Run test**

Run: `cargo test assembled_dashboard_contains_websocket_logic`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/dashboard/components/websocket.js src/dashboard.rs
git commit -m "feat: extract WebSocket logic into dashboard/components/websocket.js"
```

---

### Task 4: Extract Chart.js wrappers into `charts.js`

**Files:**
- Modify: `src/dashboard/components/charts.js`

- [ ] **Step 1: Write a failing test**

```rust
#[tokio::test]
async fn assembled_dashboard_contains_chart_functions() {
    let html = super::assemble_dashboard_html();
    assert!(html.contains("function initCharts("), "charts.js must contain initCharts()");
    assert!(html.contains("function updateCharts("), "charts.js must contain updateCharts()");
    assert!(html.contains("function resetCharts("), "charts.js must contain resetCharts()");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test assembled_dashboard_contains_chart_functions`
Expected: FAIL

- [ ] **Step 3: Populate `src/dashboard/components/charts.js`**

Extract from `dashboard.html`:
- `let ttftChart = null;` and `let errorsChart = null;` (chart state)
- `initCharts()` (line ~893)
- `updateCharts()` (line ~915)
- `resizeCharts()` (line ~933)
- `resetCharts()` (line ~938)

Escape all braces. The chart config objects have many `{}`s — each must become `{{}}`.

- [ ] **Step 4: Run test**

Run: `cargo test assembled_dashboard_contains_chart_functions`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/dashboard/components/charts.js src/dashboard.rs
git commit -m "feat: extract Chart.js wrappers into dashboard/components/charts.js"
```

---

### Task 5: Extract Overview tab into `overview.js`

**Files:**
- Modify: `src/dashboard/tabs/overview.js`
- Modify: `src/dashboard/shell.html` — add overview tab HTML markup

- [ ] **Step 1: Write a failing test**

```rust
#[tokio::test]
async fn assembled_dashboard_contains_overview_tab() {
    let html = super::assemble_dashboard_html();
    assert!(html.contains("function loadOverview("), "overview.js must contain loadOverview()");
    assert!(html.contains("function updateOverview("), "overview.js must contain updateOverview()");
    assert!(html.contains("id=\"tab-overview\""), "shell.html must contain overview tab panel");
    assert!(html.contains("id=\"health-score\""), "shell.html must contain health score card");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test assembled_dashboard_contains_overview_tab`
Expected: FAIL

- [ ] **Step 3: Populate `src/dashboard/tabs/overview.js`**

Extract from `dashboard.html`:
- `updateOverview()` (line ~758)
- `resetOverview()` (line ~837)
- `loadOverview()` (line ~985)
- `scheduleOverviewRefresh()` (line ~999)
- `setOverviewMode()` (line ~1133)
- `clearMemoryStats()` (line ~1171) — the `runResetAction` helper too (line ~1143)
- `clearAllData()` (line ~1182)
- `handleReset()` (line ~953)

Plus state variables:
```js
let statsSnapshot = null;
let currentOverviewMode = 'live';
let overviewRequestToken = 0;
let overviewRefreshTimer = null;
```

And constants:
```js
const CLEAR_MEMORY_BUTTON_LABEL = 'Clear in-memory stats';
const CLEAR_ALL_BUTTON_LABEL = 'Clear stats + data';
const EMPTY_MODELS_HTML = '<div style="color:var(--text-2);font-size:12px">No models yet</div>';
```

Escape all braces.

- [ ] **Step 4: Add overview tab HTML to `shell.html`**

Add the overview tab markup (lines 409-491 of current `dashboard.html`) into the `<div class="container">` section of `shell.html`, replacing the placeholder paragraph. Include the tab navigation bar too.

Escape all braces in the HTML.

- [ ] **Step 5: Run test**

Run: `cargo test assembled_dashboard_contains_overview_tab`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/dashboard/tabs/overview.js src/dashboard/shell.html src/dashboard.rs
git commit -m "feat: extract overview tab into dashboard/tabs/overview.js"
```

---

### Task 6: Extract Requests tab into `requests.js`

**Files:**
- Modify: `src/dashboard/tabs/requests.js`
- Modify: `src/dashboard/shell.html` — add requests tab HTML markup

- [ ] **Step 1: Write a failing test**

```rust
#[tokio::test]
async fn assembled_dashboard_contains_requests_tab() {
    let html = super::assemble_dashboard_html();
    assert!(html.contains("function loadEntries("), "requests.js must contain loadEntries()");
    assert!(html.contains("function addTableRow("), "requests.js must contain addTableRow()");
    assert!(html.contains("function selectRequest("), "requests.js must contain selectRequest()");
    assert!(html.contains("id=\"tab-requests\""), "shell.html must contain requests tab panel");
    assert!(html.contains("id=\"request-table\""), "shell.html must contain request table");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test assembled_dashboard_contains_requests_tab`
Expected: FAIL

- [ ] **Step 3: Populate `src/dashboard/tabs/requests.js`**

This is the largest extraction. Extract from `dashboard.html`:

**State variables:**
```js
let entries = [];
let currentFilter = 'all';
let searchQuery = '';
let entriesRequestToken = 0;
let selectedRequestId = null;
let currentSortBy = 'timestamp_ms';
let currentSortOrder = 'desc';
let currentSessionFilter = 'all';
let anomalyFocus = null;
let anomalyFocusHasEmptyResults = false;
let requestViewSnapshot = null;
const DEFAULT_ANOMALY_WINDOW_MS = 120000;
const EXPANDED_ANOMALY_WINDOW_MS = 600000;
```

**Functions** (extract each, escape braces):
- `setActiveTab()` (line ~1007) — this is shared but primarily request-tab focused
- `snapshotRequestViewState()` (line ~1022)
- `restoreRequestViewState()` (line ~1034)
- `renderAnomalyFocusBar()` (line ~1064)
- `clearAnomalyFocus()` (line ~1086)
- `setRequestFilter()` (line ~1108)
- `setSearchQuery()` (line ~1122)
- `selectRequest()` (line ~1194)
- `addTableRow()` (line ~1207)
- `preselectRequestRow()` (line ~1271)
- `rebuildTable()` (line ~1305)
- `matchesFilter()` (line ~1316)
- `updateExpandAnomalyButton()` (line ~1335)
- `renderAnomalyFocusEmptyState()` (line ~1344)
- `expandAnomalyWindow()` (line ~1367)
- `updateSortArrows()` (line ~1384)
- `openRequestModal()` (line ~1395)
- `closeModal()` (line ~1447)
- `loadModalCorrelations()` (line ~1452)
- `loadModalExplanations()` (line ~1478)
- `loadModalBodies()` (line ~1503)
- `loadEntries()` (line ~1601)
- `populateSessionFilter()` (line ~1642)
- `filterSession()` (line ~1665)
- `loadCorrelations()` (line ~1674)
- `loadExplanations()` (line ~1718)
- `loadSessionTimeline()` (line ~1758)
- `loadSessionGraph()` (line ~1800)
- `applyAnomalyFocus()` (line ~1574)
- `updateAnomalies()` (line ~1549) — renders anomaly badge in the anomalies tab, but called from requests context

- [ ] **Step 4: Add requests tab HTML to `shell.html`**

Add the requests tab markup (lines 494-573 of current `dashboard.html`) and the request modal root (`<div id="request-modal-root"></div>`, line 657).

Escape all braces.

- [ ] **Step 5: Run test**

Run: `cargo test assembled_dashboard_contains_requests_tab`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/dashboard/tabs/requests.js src/dashboard/shell.html src/dashboard.rs
git commit -m "feat: extract requests tab into dashboard/tabs/requests.js"
```

---

### Task 7: Extract Conformance tab into `conformance.js`

**Files:**
- Modify: `src/dashboard/tabs/conformance.js`
- Modify: `src/dashboard/shell.html` — add conformance tab HTML

- [ ] **Step 1: Write a failing test**

```rust
#[tokio::test]
async fn assembled_dashboard_contains_conformance_tab() {
    let html = super::assemble_dashboard_html();
    assert!(html.contains("id=\"tab-conformance\""), "shell.html must contain conformance tab panel");
    assert!(html.contains("id=\"conformance-scoreboard\""), "shell.html must contain scoreboard");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test assembled_dashboard_contains_conformance_tab`
Expected: FAIL

- [ ] **Step 3: Add conformance tab HTML to `shell.html` and populate `conformance.js`**

Add the conformance tab markup (lines 576-594 of current `dashboard.html`) to `shell.html`. Escape braces.

`src/dashboard/tabs/conformance.js` stays minimal since this is a placeholder tab:

```js
// Conformance tab — populated when model profiling data is available
// No JS logic needed yet; the tab HTML is static placeholder content.
```

- [ ] **Step 4: Run test**

Run: `cargo test assembled_dashboard_contains_conformance_tab`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/dashboard/tabs/conformance.js src/dashboard/shell.html src/dashboard.rs
git commit -m "feat: extract conformance tab into dashboard/tabs/conformance.js"
```

---

### Task 8: Extract Anomalies tab into `anomalies.js`

**Files:**
- Modify: `src/dashboard/tabs/anomalies.js`
- Modify: `src/dashboard/shell.html` — add anomalies tab HTML

- [ ] **Step 1: Write a failing test**

```rust
#[tokio::test]
async fn assembled_dashboard_contains_anomalies_tab() {
    let html = super::assemble_dashboard_html();
    assert!(html.contains("id=\"tab-anomalies\""), "shell.html must contain anomalies tab panel");
    assert!(html.contains("id=\"anomaly-list\""), "shell.html must contain anomaly list");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test assembled_dashboard_contains_anomalies_tab`
Expected: FAIL

- [ ] **Step 3: Add anomalies tab HTML to `shell.html` and populate `anomalies.js`**

Add the anomalies tab markup (lines 614-618 of current `dashboard.html`) to `shell.html`. Escape braces.

`src/dashboard/tabs/anomalies.js`:

```js
const EMPTY_ANOMALIES_HTML = '<div class="empty-state"><h3>No anomalies detected</h3><p>Issues will appear here in real-time</p></div>';
```

(The `updateAnomalies()` function lives in `requests.js` since it's called from the request processing flow.)

- [ ] **Step 4: Run test**

Run: `cargo test assembled_dashboard_contains_anomalies_tab`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/dashboard/tabs/anomalies.js src/dashboard/shell.html src/dashboard.rs
git commit -m "feat: extract anomalies tab into dashboard/tabs/anomalies.js"
```

---

### Task 9: Extract Sessions tab into `sessions.js`

**Files:**
- Modify: `src/dashboard/tabs/sessions.js`
- Modify: `src/dashboard/shell.html` — add sessions tab HTML

- [ ] **Step 1: Write a failing test**

```rust
#[tokio::test]
async fn assembled_dashboard_contains_sessions_tab() {
    let html = super::assemble_dashboard_html();
    assert!(html.contains("function loadSessions("), "sessions.js must contain loadSessions()");
    assert!(html.contains("function selectSession("), "sessions.js must contain selectSession()");
    assert!(html.contains("id=\"tab-sessions\""), "shell.html must contain sessions tab panel");
    assert!(html.contains("id=\"session-list\""), "shell.html must contain session list");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test assembled_dashboard_contains_sessions_tab`
Expected: FAIL

- [ ] **Step 3: Populate `src/dashboard/tabs/sessions.js`**

Extract from `dashboard.html`:

**State variables:**
```js
let selectedSessionId = null;
let sessionsRequestToken = 0;
let sessionDetailsRequestToken = 0;
const sessionConversationFullTextCache = new Map();
const MAX_SESSION_CONVERSATION_CACHE_ITEMS = 10;
const EMPTY_SESSIONS_HTML = '<div class="empty-state"><h3>No sessions recorded yet</h3><p>Sessions appear as requests come in</p></div>';
```

**Functions** (escape braces):
- `setSessionConversationCache()` (line ~704)
- `loadSessions()` (line ~2181)
- `loadSessionDetails()` (line ~2258)
- `mergeConversationWithFullText()` (line ~2283)
- `renderSessionConversationItem()` (line ~2337)
- `renderSessionConversation()` (line ~2398)
- `expandSessionConversationMessage()` (line ~2429)
- `selectSession()` (line ~2463)

- [ ] **Step 4: Add sessions tab HTML to `shell.html`**

Add the sessions tab markup (lines 597-611 of current `dashboard.html`) to `shell.html`. Escape braces.

- [ ] **Step 5: Run test**

Run: `cargo test assembled_dashboard_contains_sessions_tab`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/dashboard/tabs/sessions.js src/dashboard/shell.html src/dashboard.rs
git commit -m "feat: extract sessions tab into dashboard/tabs/sessions.js"
```

---

### Task 10: Extract app initialization into `app.js` and populate `shell.html` with remaining markup

**Files:**
- Modify: `src/dashboard/app.js`
- Modify: `src/dashboard/shell.html` — add settings panel HTML, DOMContentLoaded event wiring

- [ ] **Step 1: Write a failing test**

```rust
#[tokio::test]
async fn assembled_dashboard_contains_app_initialization() {
    let html = super::assemble_dashboard_html();
    assert!(html.contains("DOMContentLoaded"), "app.js must wire DOMContentLoaded");
    assert!(html.contains("connectWS()"), "app.js must call connectWS()");
    assert!(html.contains("initCharts()"), "app.js must call initCharts()");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test assembled_dashboard_contains_app_initialization`
Expected: FAIL

- [ ] **Step 3: Populate `src/dashboard/app.js`**

Extract the DOMContentLoaded event handler (lines ~2726-2839 of `dashboard.html`) — this is the main initialization block that:
- Calls `connectWS()`, `initCharts()`, `loadOverview()`
- Wires tab click handlers
- Wires filter click handlers  
- Wires search input
- Wires sorting headers
- Wires settings editor events
- Sets up keyboard navigation
- Loads persisted tab/filter/search state

Also extract the settings-related functions:
- `isSettingsEditorDirty()` (line ~1847)
- `parseErrorLine()` (line ~1859)
- `highlightJsonLine()` (line ~1871)
- `renderSettingsEditor()` (line ~1897)
- `syncSettingsEditorScroll()` (line ~1932)
- `renderSettingsErrorBanner()` (line ~1942)
- `validateSettingsEditorNow()` (line ~1962)
- `scheduleSettingsValidation()` (line ~1990)
- `jumpToSettingsErrorLine()` (line ~1997)
- `loadSettingsCurrent()` (line ~2011)
- `applySettingsPayload()` (line ~2055)
- `applySettings()` (line ~2119)
- `formatSettingsEditor()` (line ~2146)
- `resetSettingsEditor()` (line ~2173)

Plus settings state variables:
```js
let settingsMismatchActive = false;
let settingsDiskLoadedSnapshot = null;
let settingsApplyInFlight = false;
let settingsValidationErrorLine = null;
let settingsValidationMessage = '';
let settingsValidateTimer = null;
```

Escape all braces.

- [ ] **Step 4: Add settings panel HTML to `shell.html`**

Add the settings editor panel (lines 620-654 of current `dashboard.html`) to `shell.html`. Escape braces.

Also add the toolbar action buttons to the header in `shell.html`:
```html
<div class="toolbar-actions">
  <button class="toolbar-btn secondary" id="clear-memory-btn" type="button" title="Reset dashboard statistics in memory but keep saved SQLite data">Clear in-memory stats</button>
  <button class="toolbar-btn danger" id="clear-data-btn" type="button" title="Delete saved SQLite request history and reset all dashboard statistics">Clear stats + data</button>
</div>
```

- [ ] **Step 5: Run test**

Run: `cargo test assembled_dashboard_contains_app_initialization`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/dashboard/app.js src/dashboard/shell.html src/dashboard.rs
git commit -m "feat: extract app initialization into dashboard/app.js"
```

---

### Task 11: Switch `serve_dashboard()` to assembled output and delete `dashboard.html`

**Files:**
- Modify: `src/dashboard.rs` — switch `serve_dashboard()` to use `assemble_dashboard_html()`
- Delete: `src/dashboard.html`
- Modify: `src/dashboard.rs` — update all tests that reference `include_str!("dashboard.html")`

- [ ] **Step 1: Write a test that verifies the switch**

```rust
#[tokio::test]
async fn assembled_dashboard_preserves_v2_tab_markers() {
    let html = super::assemble_dashboard_html();
    for tab in &["overview", "requests", "conformance", "anomalies", "sessions"] {
        assert!(
            html.contains(&format!("data-tab=\"{}\"", tab)),
            "assembled output missing tab button for {}",
            tab
        );
        assert!(
            html.contains(&format!("id=\"tab-{}\"", tab)),
            "assembled output missing tab panel for {}",
            tab
        );
    }
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test assembled_dashboard_preserves_v2_tab_markers`
Expected: PASS (all tab markup should be in `shell.html` by now)

- [ ] **Step 3: Switch `serve_dashboard()`**

Change `serve_dashboard()` in `dashboard.rs` from:

```rust
async fn serve_dashboard() -> impl IntoResponse {
    Html(include_str!("dashboard.html"))
}
```

To:

```rust
async fn serve_dashboard() -> impl IntoResponse {
    Html(assemble_dashboard_html())
}
```

- [ ] **Step 4: Update existing tests**

All tests that use `include_str!("dashboard.html")` must be updated to use `super::assemble_dashboard_html()` instead. Search for all occurrences and replace. For example:

```rust
// Before:
let html = include_str!("dashboard.html");

// After:
let html = super::assemble_dashboard_html();
```

There are approximately 20 such tests. Update all of them.

- [ ] **Step 5: Run full test suite**

Run: `cargo test`
Expected: All tests pass

- [ ] **Step 6: Delete `src/dashboard.html`**

```bash
rm src/dashboard.html
```

- [ ] **Step 7: Run full test suite again**

Run: `cargo test`
Expected: All tests still pass (no references to `dashboard.html` remain)

- [ ] **Step 8: Commit**

```bash
git add src/dashboard.rs src/dashboard.html
git commit -m "feat: switch to assembled dashboard and delete monolithic dashboard.html"
```

---

### Task 12: Update ARCHITECTURE.md

**Files:**
- Modify: `ARCHITECTURE.md`

- [ ] **Step 1: Rewrite `ARCHITECTURE.md`**

Replace the entire file with accurate content:

```markdown
# ClaudeProxy: Architecture Document

**Version:** 2.0
**Date:** April 2026
**Language:** Rust (Edition 2021)

---

## Runtime Components

main() starts:
1. **Proxy Server** (Axum on :8000) — intercepts Claude Code API requests
2. **Dashboard Server** (Axum on :3000) — REST API + WebSocket + embedded SPA
3. **Analyzer Worker** (5s tick) — background anomaly detection and model profiling

---

## Proxy Request Flow

```
Request → Bind :8000 → Parse metadata (model, stream, session_id)
→ Forward headers to upstream → Send request → Handle response:

1. ERROR (status ≥ 400): Read body, summarize, record, store
2. SSE STREAMING: Spawn task, detect stalls, extract usage, track unknown events
3. REGULAR JSON: Read body, parse JSON for usage, record, store

→ Response back to client
```

---

## Database Schemas

### Stats DB (proxy.db in data_dir)

```
requests: id, timestamp_ms, session_id, method, path, model, stream, status,
          ttft_ms, duration_ms, tokens, sizes, error, anomalies

request_bodies: request_id, request_body, response_body, truncated

local_events: id, source_kind, source_path, event_time_ms, session_hint,
              event_kind, payload_json

request_correlations: request_id, local_event_id, link_type, confidence, reason
```

### V2 Store (proxy-v2.db in data_dir)

```
requests: id, session_id, timestamp, method, path, model, stream, status_code,
          status_kind, ttft_ms, duration_ms, tokens, sizes, stalls, error,
          stop_reason, content_block_types, anomalies, analyzed

request_bodies: request_id, request_body, response_body, truncated
request_bodies_fts: FTS5 virtual table synced via triggers

anomalies: id, request_id, kind, severity, summary, hypothesis, detected_at

model_profiles: model, sample_count, last_updated
model_observed: model, observed_json, updated_at

sessions: session_id, first_seen, last_seen, request_count
tool_usage: id, request_id, tool_name, tool_input_json
```

---

## Dashboard Architecture

Single-page app assembled at compile time from `src/dashboard/` files:

```
src/dashboard/
├── shell.html              # HTML skeleton with {placeholders}
├── styles.css              # All CSS (brace-escaped for format!)
├── app.js                  # DOMContentLoaded wiring, settings editor
├── utils.js                # Shared helpers: fmt, esc, formatDuration, etc.
├── components/
│   ├── charts.js           # Chart.js lifecycle management
│   └── websocket.js        # WebSocket connection + reconnect
└── tabs/
    ├── overview.js          # Stat cards, timeseries, breakdowns
    ├── requests.js          # Table, search, filters, modal, correlations
    ├── conformance.js       # Model scoreboard (placeholder)
    ├── anomalies.js         # Anomaly list with severity badges
    └── sessions.js          # Split layout, timeline, conversation

Assembly: dashboard.rs → format!(shell.html, css=styles.css, app_js=app.js, ...)
Result: Single HTML response served at GET /
```

5 tabs: Overview, Requests, Model Conformance, Anomalies, Sessions

---

## Source Modules

| Module | Lines | Responsibility |
|--------|-------|---------------|
| `stats.rs` | ~7,300 | Stats store, live stats, session aggregation, DB schema |
| `dashboard.rs` | ~2,000 | Axum routes, REST API, WebSocket, HTML assembly |
| `store.rs` | ~870 | V2 SQLite store, FTS5, CRUD |
| `proxy.rs` | ~690 | HTTP proxy, SSE streaming, request forwarding |
| `model_profile.rs` | ~410 | Model config, behavior class resolution, auto-tune |
| `main.rs` | ~330 | CLI, runtime orchestration, analyzer worker |
| `types.rs` | ~220 | V2 types, forward-compat tracking |
| `analyzer.rs` | ~110 | Anomaly detection rules, health score |
| `correlation.rs` | ~50 | PayloadPolicy enum |

---

## Analyzer Worker

Runs every 5 seconds:
1. Fetch up to 200 unanalyzed requests
2. For each: detect anomalies against recent model history
3. Persist anomalies atomically (transaction: delete prior → insert new → mark analyzed)
4. At 50-sample boundaries: compute and store model observed stats

---

## Key Metrics Per Request

RequestEntry captures:
- Identification: id, timestamp, session_id, method, path, model, stream
- Status: status, duration_ms, ttft_ms
- Tokens: input, output, cache_read, cache_creation, thinking
- Sizes: request_size_bytes, response_size_bytes
- Quality: stalls (array), error message, anomalies (array), stop_reason

---

## Dependencies

Runtime: tokio, axum, hyper, reqwest (proxy), rusqlite (SQLite)
Serialization: serde, serde_json, chrono
CLI: clap, uuid, dirs, parking_lot
Streaming: futures-util, tokio-stream
Frontend: Alpine.js (CDN), Chart.js (CDN)

Release: LTO enabled, single codegen unit, symbols stripped
```

- [ ] **Step 2: Run `cargo test` to verify nothing broke**

Run: `cargo test`
Expected: All pass (documentation changes don't affect tests)

- [ ] **Step 3: Commit**

```bash
git add ARCHITECTURE.md
git commit -m "docs: rewrite ARCHITECTURE.md for v2 post-rewrite state"
```

---

### Task 13: Update README.md

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Rewrite `README.md`**

Replace the entire file with accurate content:

```markdown
# Claude Code Proxy — Ultra-Fast API Monitor

A near-zero-latency logging proxy for Claude Code with a real-time web dashboard. Built in Rust for maximum performance.

## What It Does

Sits transparently between Claude Code and the Anthropic API. Logs every request and provides:

- **Real-time anomaly detection** — slow TTFT, stream stalls, error spirals, rate limiting, gateway errors
- **Model profiling** — automatic behavior fingerprinting with 140-parameter profiles, auto-tuning at 50-sample intervals
- **Session tracking** — timelines, conversation drill-down, and session graphs per Claude Code session
- **Forward-compat monitoring** — detects unknown SSE events, stop reasons, and API fields as Anthropic evolves the protocol
- **5-tab dashboard** — overview report card, request browser, model conformance, anomaly feed, session explorer

Adds **< 0.5ms** latency per request (measured on localhost passthrough).

## Quick Setup

### 1. Install Rust (one time)
```powershell
winget install Rustlang.Rust.MSVC
# Or download from https://rustup.rs
```

### 2. Build
```powershell
cd claude-proxy
cargo build --release
```

Binary at `target\release\claude-proxy.exe` (~5MB).

### 3. Run
```powershell
claude-proxy.exe --target https://api.anthropic.com
```

This starts:
- **Proxy** on `http://127.0.0.1:8000`
- **Dashboard** on `http://127.0.0.1:3000`

### 4. Configure Claude Code

Set `ANTHROPIC_BASE_URL` in Claude Code settings:

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:8000"
  }
}
```

Start using Claude Code normally — everything shows up in the dashboard.

## Dashboard

5-tab real-time dashboard:

- **Overview** — health score (0-100), stat cards, TTFT/error timeseries, model and error breakdowns
- **Requests** — sortable table with search, filters (error/5xx/4xx/timeout/stall), session filter, request detail modal with body viewer, correlation links, session timeline
- **Model Conformance** — conformance scoreboard (populates as profiling data accumulates)
- **Anomalies** — severity-badged anomaly feed with click-to-focus request filtering
- **Sessions** — split layout browser with session list, detail panel (metrics, timeline, conversation preview)

All data updates via WebSocket in real-time.

## CLI Options

```
claude-proxy --target https://api.anthropic.com   # Required: upstream API URL
             --port 8000                           # Proxy port (default: 8000)
             --dashboard-port 3000                 # Dashboard port (default: 3000)
             --data-dir ~/.claude/api-logs          # Storage directory
             --stall-threshold 0.5                  # Stall detection threshold in seconds (default: 0.5)
             --slow-ttft-threshold 3000             # Slow TTFT threshold in ms (default: 3000)
             --max-body-size 2097152                # Max request/response body to store (default: 2MB)
             --open-browser                         # Auto-open dashboard in browser
```

## API Endpoints

### Core
- `GET /api/health` — health metrics and report card
- `GET /api/stats` — live statistics snapshot
- `GET /api/entries` — request entries with optional filters

### Requests
- `GET /api/requests?limit=&offset=&search=` — paginated request list with FTS search
- `GET /api/requests/:id` — request detail
- `GET /api/requests/:id/body` — request/response bodies
- `GET /api/requests/:id/tools` — tool usage for a request

### Models
- `GET /api/models` — model list with stats
- `GET /api/models/:name/profile` — model behavior profile
- `GET /api/models/:name/comparison` — model comparison data
- `GET /api/model-config` — model configuration
- `PUT /api/model-config` — update model configuration

### Anomalies
- `GET /api/anomalies` — all anomalies
- `GET /api/anomalies/recent` — recent anomalies
- `GET /api/anomalies/:id` — anomaly detail

### Sessions
- `GET /api/sessions` — session list
- `GET /api/sessions/:id` — session detail
- `GET /api/sessions/merged` — merged session data
- `GET /api/session-details?session_id=` — full session with timeline and conversation
- `GET /api/session-graph?session_id=` — session relationship graph
- `GET /api/timeline?session_id=` — chronological session timeline

### Intelligence
- `GET /api/correlations?request_id=` — correlation links
- `GET /api/explanations?request_id=` — ranked explanations

### WebSocket
- `GET /ws` — real-time stats stream

## Persistence

Two SQLite databases in the data directory (default: `~/.claude/api-logs/`):
- `proxy.db` — stats store (requests, bodies, events, correlations)
- `proxy-v2.db` — v2 store (requests, FTS search, anomalies, model profiles, sessions)

## Performance

- **Proxy overhead**: < 0.5ms per request
- **Memory**: ~100MB for 50,000 entries
- **CPU**: negligible (async Rust with tokio)
- **Streaming**: zero-copy SSE passthrough

## Architecture

```
Claude Code  →  Proxy (:8000)  →  Anthropic API
                    │
                    ↓
              Dashboard (:3000)
                    │
                    ↓
              SQLite (proxy.db + proxy-v2.db)
```

Single Rust binary. No Node.js. No npm. No build step beyond `cargo build`.

## License

Private — all rights reserved.
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: update README.md for v2 post-rewrite state"
```

---

### Task 14: Final verification

**Files:** None — verification only

- [ ] **Step 1: Run full test suite**

```bash
cargo test
```

Expected: All tests pass (should be ~128 + new assembly tests)

- [ ] **Step 2: Check for warnings**

```bash
cargo check 2>&1 | grep "warning:"
```

Expected: No warnings (or only the existing `#[allow(dead_code)]` suppressed ones)

- [ ] **Step 3: Check formatting**

```bash
cargo fmt -- --check
```

Expected: Clean

- [ ] **Step 4: Verify the assembled dashboard loads in browser**

```bash
cargo run -- --target https://api.anthropic.com --open-browser
```

Open the dashboard in a browser. Verify:
- All 5 tabs render and switch correctly
- WebSocket connects (green dot)
- No JavaScript console errors
- Charts render on the overview tab
- Styles look correct (dark theme, fonts loaded)

- [ ] **Step 5: Final commit if any cleanup needed**

```bash
git add -A
git commit -m "chore: final verification cleanup"
```
