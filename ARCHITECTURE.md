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
