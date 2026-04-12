# Dashboard Restructure & Documentation Update

## Goal

Split the monolithic 2,841-line `dashboard.html` into focused files using Alpine.js for component organization, assembled at compile time via `include_str!`. Update ARCHITECTURE.md and README.md to reflect the post-v2-rewrite codebase.

## Constraints

- **Zero build step** — no Node.js, no npm, no bundler. `cargo build` is the only command.
- **Compile-time assembly** — all dashboard files embedded via `include_str!` and concatenated in Rust. Single binary, no runtime file dependencies.
- **Alpine.js via CDN** — loaded from `cdn.jsdelivr.net` (already loading Chart.js this way).

---

## 1. Dashboard File Structure

### Current state

```
src/
  dashboard.html   # 2,841 lines, 120KB — CSS + HTML + 88 JS functions, all inline
  dashboard.rs     # 2,001 lines — Axum routes, serves dashboard.html via include_str!
```

### Target state

```
src/
  dashboard.rs                # Axum routes, compile-time HTML assembly
  dashboard/
    shell.html                # DOCTYPE, <head>, layout skeleton, tab containers, template slots
    styles.css                # All CSS extracted from the <style> block
    app.js                    # Alpine.js app init, global store, tab routing
    tabs/
      overview.js             # Overview tab component: stat cards, timeseries, breakdowns
      requests.js             # Requests tab component: table, search, filters, detail panel
      conformance.js          # Conformance tab component: model scoreboard placeholder
      anomalies.js            # Anomalies tab component: list, severity badges, anomaly focus
      sessions.js             # Sessions tab component: split layout, timeline, conversation
    components/
      charts.js               # Chart.js wrapper: create, update, destroy helpers
      websocket.js            # WebSocket connection, reconnect logic, event dispatch to Alpine store
    utils.js                  # Shared helpers: formatters, time utils, API fetch wrappers
```

### Compile-time assembly in dashboard.rs

```rust
async fn serve_dashboard() -> impl IntoResponse {
    let html = format!(
        include_str!("dashboard/shell.html"),
        css = include_str!("dashboard/styles.css"),
        app_js = include_str!("dashboard/app.js"),
        overview_js = include_str!("dashboard/tabs/overview.js"),
        requests_js = include_str!("dashboard/tabs/requests.js"),
        conformance_js = include_str!("dashboard/tabs/conformance.js"),
        anomalies_js = include_str!("dashboard/tabs/anomalies.js"),
        sessions_js = include_str!("dashboard/tabs/sessions.js"),
        charts_js = include_str!("dashboard/components/charts.js"),
        websocket_js = include_str!("dashboard/components/websocket.js"),
        utils_js = include_str!("dashboard/utils.js"),
    );
    Html(html)
}
```

`shell.html` uses `{css}`, `{app_js}`, etc. as template placeholders that `format!` fills in.

**Important:** Any literal `{` or `}` in the HTML/CSS/JS must be escaped as `{{` and `}}` since `format!` interprets them. This applies to CSS rules, JS objects, and template literals.

---

## 2. Alpine.js Component Architecture

### Global store (app.js)

```js
document.addEventListener('alpine:init', () => {
    Alpine.store('app', {
        activeTab: 'overview',
        connected: false,
        stats: null,
        entries: [],
        switchTab(tab) {
            const allowed = ['overview', 'requests', 'conformance', 'anomalies', 'sessions'];
            if (allowed.includes(tab)) this.activeTab = tab;
        }
    });
});
```

### Tab components (e.g., tabs/overview.js)

Each tab registers an `Alpine.data()` component:

```js
document.addEventListener('alpine:init', () => {
    Alpine.data('overviewTab', () => ({
        cards: [],
        loaded: false,
        init() {
            this.$watch('$store.app.activeTab', (tab) => {
                if (tab === 'overview' && !this.loaded) this.refresh();
            });
            if (this.$store.app.activeTab === 'overview') this.refresh();
        },
        async refresh() {
            const res = await fetch('/api/stats');
            const data = await res.json();
            this.cards = data;
            this.loaded = true;
        }
    }));
});
```

### Tab visibility in shell.html

```html
<div x-data="overviewTab" x-show="$store.app.activeTab === 'overview'" x-cloak>
    <!-- Overview markup -->
</div>
<div x-data="requestsTab" x-show="$store.app.activeTab === 'requests'" x-cloak>
    <!-- Requests markup -->
</div>
<!-- etc. -->
```

### Key patterns

- **Lazy loading**: Tabs fetch data on first show, not on page load
- **WebSocket updates**: `websocket.js` pushes updates to `Alpine.store('app')`, tabs react via `$watch`
- **Chart lifecycle**: Charts created when tab shown, updated on data change, not destroyed on hide
- **No HTML in JS**: All markup stays in `shell.html`, JS files contain only data and methods

---

## 3. WebSocket Integration

```js
// components/websocket.js
function initWebSocket() {
    const ws = new WebSocket(`ws://${location.host}/ws`);

    ws.onopen = () => {
        Alpine.store('app').connected = true;
    };

    ws.onmessage = (event) => {
        const data = JSON.parse(event.data);
        Alpine.store('app').stats = data;
    };

    ws.onclose = () => {
        Alpine.store('app').connected = false;
        setTimeout(initWebSocket, 3000);
    };
}

document.addEventListener('DOMContentLoaded', initWebSocket);
```

---

## 4. Chart.js Wrapper

```js
// components/charts.js
const chartInstances = {};

function getOrCreateChart(canvasId, config) {
    if (chartInstances[canvasId]) {
        return chartInstances[canvasId];
    }
    const ctx = document.getElementById(canvasId)?.getContext('2d');
    if (!ctx) return null;
    chartInstances[canvasId] = new Chart(ctx, config);
    return chartInstances[canvasId];
}

function updateChartData(canvasId, labels, datasets) {
    const chart = chartInstances[canvasId];
    if (!chart) return;
    chart.data.labels = labels;
    chart.data.datasets = datasets;
    chart.update('none');
}
```

---

## 5. Documentation Updates

### ARCHITECTURE.md — Full rewrite

Must reflect:
- **8 source modules**: analyzer, correlation, dashboard, model_profile, proxy, stats, store, types
- **2 runtime components**: proxy server (port 8000) + dashboard server (port 3000)
- **1 background worker**: analyzer tick (5s interval)
- **Database schema**: requests, request_bodies, request_bodies_fts, model_profiles, model_observed — remove deleted tables
- **Dashboard**: Alpine.js component architecture, compile-time assembly, 5 tabs
- **Correct line counts and file sizes**
- **Remove**: all references to local_context, explainer, correlation_engine, settings_admin, session_admin, explanation worker, correlation worker, ingestion worker

### README.md — Update features and setup

Must reflect:
- **Features**: anomaly detection, model profiling/auto-tuning, session tracking, forward-compat monitoring, 5-tab dashboard
- **Remove**: settings management, request correlation, explanation generation
- **CLI flags**: `--target`, `--port`, `--dashboard-port`, `--data-dir`, `--stall-threshold`, `--slow-ttft-threshold`, `--max-body-size`, `--open-browser`
- **Default ports**: proxy 8000, dashboard 3000
- **Keep**: quick setup section, performance claims, Rust build instructions

---

## 6. Migration Strategy

### Extraction order

1. Extract `styles.css` from the `<style>` block
2. Extract `utils.js` — shared formatters and fetch helpers
3. Extract `components/websocket.js` — WebSocket logic
4. Extract `components/charts.js` — Chart.js wrappers
5. Extract `tabs/overview.js` through `tabs/sessions.js` — one tab at a time
6. Extract `app.js` — Alpine.js init, global store, tab routing
7. Create `shell.html` — layout skeleton with `{placeholders}`
8. Update `dashboard.rs` — switch from `include_str!("dashboard.html")` to `format!` assembly

### Escape handling

All `{` and `}` in CSS, JS, and HTML content must be escaped to `{{` and `}}` for Rust's `format!` macro. This is the main mechanical risk. Each file must be validated after extraction.

### Testing approach

- Existing dashboard tests (HTML content assertions) continue to work against the assembled output
- Each extraction step must be followed by `cargo test` to verify no regressions
- Manual browser verification after each tab extraction

---

## 7. What This Does NOT Change

- **No new npm/Node.js dependency** — Alpine.js loaded via CDN
- **No build step** — `cargo build` remains the only command
- **No API changes** — all `/api/*` routes unchanged
- **No WebSocket protocol changes**
- **No new Rust dependencies**
- **Single binary deployment** — dashboard still fully embedded
