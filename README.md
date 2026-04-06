# Claude Code Proxy — Ultra-Fast API Monitor

A near-zero-latency logging proxy for Claude Code with a real-time web dashboard. Built in Rust for maximum performance on Windows.

## What It Does

Sits transparently between Claude Code and the Anthropic API. Logs every request and provides:

- **Real-time anomaly detection** — slow TTFT, stream stalls, error spirals, rate limiting, gateway errors
- **Request correlation** — links proxied API calls to local `.claude` events (project files, shell snapshots, config drift)
- **Settings management** — full admin UI for `settings.json` with history, backups, mismatch detection, and one-click reconciliation
- **Session intelligence** — timelines, graphs, and conversation drill-down per Claude Code session

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

Binary will be at `target\release\claude-proxy.exe` (~5MB).

### 3. Run
```powershell
claude-proxy.exe --target https://your-api-url.com
```

This starts:
- **Proxy** on `http://127.0.0.1:9090` (Claude Code connects here)
- **Dashboard** on `http://127.0.0.1:9091` (opens in browser automatically)

### 4. Configure Claude Code

In your Claude Code settings, set `ANTHROPIC_BASE_URL`:

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:9090"
  }
}
```

Start using Claude Code normally — everything shows up in the dashboard.

## Dashboard

The dashboard provides real-time visibility into:

- **Health score** (0-100) with color-coded status
- **TTFT chart** — time to first token over the last 60 minutes
- **Error rate chart** — requests vs errors over time
- **Request feed** — every API call with status, timing, tokens, stalls
- **Session browser** — group and explore requests by session
- **Anomaly feed** — all detected issues with severity
- **Search & filter** — find specific errors, models, sessions

All data updates via WebSocket in real-time.

### Correlation & Debugging Intelligence

- **Correlation links** — evidence links from proxied requests to local `.claude` events
- **Top explanations** — ranked likely causes for anomalous requests with confidence scores
- **Session timeline** — chronological request + local event overlays per session
- **Session graph** — request/local-event/explanation node and edge summary per session

### Settings Management

- **Live editor** with syntax-highlighted JSON editing
- **Version history** with tagging, search, and one-click restore
- **Backup management** — automatic timestamped backups before every save (20 max retention)
- **Mismatch detection** — detects when `settings.json` on disk diverges from the DB snapshot and surfaces a reconciliation UI (keep disk / keep DB snapshot)
- **File recreation** — automatically recreates `settings.json` from DB if the file is deleted

## CLI Options

```
claude-proxy.exe --target https://api.example.com    # Required: target API
                 --port 9090                          # Proxy port (default: 9090)
                 --dashboard-port 9091                # Dashboard port (default: 9091)
                 --stall-threshold 20.0               # Seconds before flagging a stall (default: 20)
                 --slow-ttft-threshold 8.0            # TTFT threshold in seconds (default: 8)
                 --max-entries 50000                   # Max entries in memory (default: 50000)
                 --log-dir C:\custom\path              # Storage directory (contains proxy.db)
                 --open-browser false                  # Disable auto-open dashboard (default: true)
                 --claude-dir C:\Users\<you>\.claude    # Override local Claude directory for correlation
                 --projects-policy metadata            # projects source policy: metadata|redacted|full
                 --shell-policy metadata               # shell source policy: metadata|redacted|full
                 --config-policy metadata              # config source policy: metadata|redacted|full
                 --enable-shell-correlation            # ingest .claude/shell-snapshots
                 --enable-config-correlation           # ingest .claude/settings.json drift events
```

## APIs

### Dashboard
- `GET /api/entries?limit=...&anomaly_ts_ms=...&window_ms=...`

### Correlation & Intelligence
- `GET /api/correlations?request_id=...&limit=...`
- `GET /api/explanations?request_id=...&limit=...`
- `GET /api/timeline?session_id=...&from=...&to=...&limit=...`
- `GET /api/session-graph?session_id=...&limit=...`

### Settings Admin
- `GET /api/settings/current` — current settings with mismatch detection
- `POST /api/settings/apply` — apply new settings (creates backup + history)
- `GET /api/settings/history?limit=...&offset=...&search=...`
- `PATCH /api/settings/history/:revision_id/tags`
- `DELETE /api/settings/history/:revision_id`
- `DELETE /api/settings/history`
- `GET /api/settings/backups`
- `DELETE /api/settings/backups/selected`
- `DELETE /api/settings/backups/all`

## Persistence

Requests are persisted in a local SQLite database (`proxy.db`) within the log directory (default: `~/.claude/api-logs/`). The proxy keeps a live in-memory ring buffer for the dashboard and writes every request to SQLite for durable history.

## Anomaly Detection

| Anomaly | Threshold | Severity |
|---------|-----------|----------|
| Slow TTFT | > 8s (configurable) | Warning/Error/Critical |
| Stream stall | > 20s gap (configurable) | Warning/Error/Critical |
| Timeout | Request timed out | Critical |
| Connection failure | Can't reach API | Critical |
| Rate limited | HTTP 429 | Error |
| Gateway error | 502/503/504 | Error |
| Model error | 403/404 with model message | Error |
| Slow response | > 120s total | Warning |
| Error spiral | 5+ errors in last 10 requests | Critical |

## Performance

- **Proxy overhead**: < 0.5ms per request
- **Memory**: ~100MB for 50,000 entries
- **Disk**: SQLite growth depends on request volume
- **CPU**: negligible (async Rust with tokio)

The proxy uses zero-copy streaming for SSE responses — data flows through without buffering.

## Architecture

```
Claude Code  -->  Proxy (:9090)  -->  Anthropic API
                    |
                    v
              Dashboard (:9091)
                    |
                    v
              SQLite (proxy.db)
```

Single Rust binary. No external dependencies at runtime.

## License

Private — all rights reserved.
