# Session Intelligence Contract Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add versioned/freshness-aware API contracts for session-intelligence endpoints and move proxy/local session merge logic to a server endpoint consumed by the dashboard.

**Architecture:** Introduce a shared versioned response envelope in `dashboard.rs`, add a new merged sessions API sourced from a new `StatsStore` aggregation method, and update frontend data-loading paths to consume `envelope.data` instead of raw arrays. Keep existing data derivation behavior intact and layer contract hardening + metadata on top so behavior remains backward-comprehensible during rollout.

**Tech Stack:** Rust (axum, serde, rusqlite), vanilla dashboard JS/HTML, cargo test

---

## Scope Check

The spec includes several recommendations. This plan intentionally focuses one deliverable: **contract hardening + freshness metadata + server-side session merge** for the existing session intelligence surfaces. This is a single coherent subsystem slice and can ship independently.

## File Structure and Responsibilities

- **Modify:** `src/stats.rs`
  - Add merged-session DTO and aggregation method that combines proxy + local session signals.
  - Keep derivation purely read-side from existing persisted data.

- **Modify:** `src/dashboard.rs`
  - Add shared versioned envelope type.
  - Add `/api/sessions/merged` endpoint.
  - Return versioned envelopes from correlation/explanation/timeline/session-graph/session-details endpoints.
  - Add/adjust endpoint tests.

- **Modify:** `src/dashboard.html`
  - Switch sessions loader to `/api/sessions/merged`.
  - Update all session-intelligence fetchers to consume envelope payloads (`payload.data`).
  - Render confidence/provenance helper messaging in request detail surfaces.

- **Modify:** `README.md`
  - Update API contract section for versioned envelopes and merged sessions endpoint.

- **Test touchpoints:**
  - `src/stats.rs` unit tests for merged sessions aggregation.
  - `src/dashboard.rs` endpoint tests for envelope shape and merged endpoint.
  - Existing dashboard HTML string-contract tests for endpoint wiring text.

---

### Task 1: Add merged sessions aggregation in StatsStore

**Files:**
- Modify: `src/stats.rs` (new DTO + `get_merged_sessions`)
- Test: `src/stats.rs` (unit tests near existing session tests)

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn get_merged_sessions_combines_proxy_and_local_presence_and_activity() {
    let log_dir = std::env::temp_dir().join(format!(
        "claude-proxy-merged-sessions-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&log_dir).unwrap();

    let store = StatsStore::new(50, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

    let mut req = sample_entry();
    req.id = "req-merged-1".into();
    req.session_id = Some("s-merged".into());
    store.add_entry(req);

    store.upsert_local_event(&LocalEvent {
        id: "evt-merged-1".into(),
        source_kind: SourceKind::ClaudeProject,
        source_path: "projects/demo/session.json".into(),
        event_time_ms: chrono::Utc::now().timestamp_millis(),
        session_hint: Some("s-merged".into()),
        event_kind: "session_touch".into(),
        model_hint: None,
        payload_policy: crate::correlation::PayloadPolicy::MetadataOnly,
        payload_json: serde_json::json!({}),
    });

    let rows = store.get_merged_sessions();
    let row = rows.iter().find(|r| r.session_id == "s-merged").unwrap();

    assert_eq!(row.presence, "both");
    assert_eq!(row.proxy_request_count, 1);
    assert_eq!(row.local_event_count >= 1, true);
    assert!(row.last_activity_ms >= row.last_proxy_activity_ms.unwrap_or_default());

    let _ = std::fs::remove_dir_all(&log_dir);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test stats::tests::get_merged_sessions_combines_proxy_and_local_presence_and_activity -- --exact`

Expected: FAIL with a compile error like `no method named 'get_merged_sessions' found for struct 'StatsStore'`.

- [ ] **Step 3: Write minimal implementation**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedSessionSummary {
    pub session_id: String,
    pub presence: String,
    pub proxy_request_count: u64,
    pub local_event_count: u64,
    pub proxy_error_count: u64,
    pub proxy_stall_count: u64,
    pub last_proxy_activity_ms: Option<i64>,
    pub last_local_activity_ms: Option<i64>,
    pub last_activity_ms: i64,
    pub project_path: Option<String>,
}

pub fn get_merged_sessions(&self) -> Vec<MergedSessionSummary> {
    let proxy = self.get_sessions();
    let local = self.get_claude_sessions();

    let mut by_id = std::collections::BTreeMap::<String, MergedSessionSummary>::new();

    for p in proxy {
        let last_proxy = p.last_request.map(|ts| ts.timestamp_millis());
        by_id.insert(
            p.session_id.clone(),
            MergedSessionSummary {
                session_id: p.session_id,
                presence: "proxy".to_string(),
                proxy_request_count: p.request_count,
                local_event_count: 0,
                proxy_error_count: p.error_count,
                proxy_stall_count: p.stall_count,
                last_proxy_activity_ms: last_proxy,
                last_local_activity_ms: None,
                last_activity_ms: last_proxy.unwrap_or(0),
                project_path: None,
            },
        );
    }

    for l in local {
        let row = by_id.entry(l.session_id.clone()).or_insert(MergedSessionSummary {
            session_id: l.session_id.clone(),
            presence: "local".to_string(),
            proxy_request_count: 0,
            local_event_count: 0,
            proxy_error_count: 0,
            proxy_stall_count: 0,
            last_proxy_activity_ms: None,
            last_local_activity_ms: None,
            last_activity_ms: 0,
            project_path: None,
        });

        row.local_event_count = row.local_event_count.max(l.request_count);
        row.last_local_activity_ms = Some(l.last_local_activity_ms.max(l.last_modified_ms));
        row.project_path = if l.project_path.is_empty() { None } else { Some(l.project_path) };

        row.presence = if row.last_proxy_activity_ms.is_some() {
            "both".to_string()
        } else {
            "local".to_string()
        };

        row.last_activity_ms = row
            .last_proxy_activity_ms
            .unwrap_or(0)
            .max(row.last_local_activity_ms.unwrap_or(0));
    }

    let mut out = by_id.into_values().collect::<Vec<_>>();
    out.sort_by(|a, b| b.last_activity_ms.cmp(&a.last_activity_ms).then_with(|| a.session_id.cmp(&b.session_id)));
    out
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test stats::tests::get_merged_sessions_combines_proxy_and_local_presence_and_activity -- --exact`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/stats.rs
git commit -m "feat: add merged session aggregation for proxy/local presence"
```

---

### Task 2: Add merged sessions endpoint in dashboard API

**Files:**
- Modify: `src/dashboard.rs`
- Test: `src/dashboard.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn merged_sessions_endpoint_returns_presence_rows() {
    let log_dir = std::env::temp_dir().join(format!(
        "claude-proxy-dashboard-merged-sessions-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&log_dir).unwrap();

    let store = Arc::new(StatsStore::new(20, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone()));

    let mut req = sample_entry();
    req.id = "req-merged-endpoint-1".into();
    req.session_id = Some("s-endpoint".into());
    store.add_entry(req);

    let app = build_dashboard_app(store);
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/sessions/merged")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(payload.get("version").and_then(|v| v.as_str()), Some("2026-04-11"));
    assert!(payload.get("generated_at_ms").and_then(|v| v.as_i64()).is_some());
    assert!(payload.get("data").and_then(|v| v.as_array()).map(|arr| !arr.is_empty()).unwrap_or(false));

    let _ = std::fs::remove_dir_all(&log_dir);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test dashboard::tests::merged_sessions_endpoint_returns_presence_rows -- --exact`

Expected: FAIL with `404` (route missing).

- [ ] **Step 3: Write minimal implementation**

```rust
#[derive(serde::Serialize)]
struct VersionedEnvelope<T> {
    version: &'static str,
    generated_at_ms: i64,
    data: T,
}

async fn api_sessions_merged(State(store): State<Arc<StatsStore>>) -> impl IntoResponse {
    let rows = store.get_merged_sessions();
    axum::Json(VersionedEnvelope {
        version: "2026-04-11",
        generated_at_ms: chrono::Utc::now().timestamp_millis(),
        data: rows,
    })
}
```

And add route:

```rust
.route("/api/sessions/merged", get(api_sessions_merged))
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test dashboard::tests::merged_sessions_endpoint_returns_presence_rows -- --exact`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/dashboard.rs
git commit -m "feat: add merged sessions dashboard endpoint"
```

---

### Task 3: Version and timestamp all session-intelligence endpoint responses

**Files:**
- Modify: `src/dashboard.rs`
- Test: `src/dashboard.rs`

- [ ] **Step 1: Write failing tests for envelope contracts**

```rust
#[tokio::test]
async fn correlations_endpoint_returns_versioned_envelope() {
    let log_dir = std::env::temp_dir().join(format!("claude-proxy-correlations-envelope-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&log_dir).unwrap();
    let store = Arc::new(StatsStore::new(10, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone()));
    let app = build_dashboard_app(store);

    let response = app
        .oneshot(Request::builder().method(Method::GET).uri("/api/correlations?request_id=req-1&limit=5").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(payload.get("data").is_some());
    assert!(payload.get("generated_at_ms").is_some());
    let _ = std::fs::remove_dir_all(&log_dir);
}
```

Add equivalent tests for:
- `/api/explanations`
- `/api/timeline`
- `/api/session-graph`
- `/api/session-details`

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
cargo test dashboard::tests::correlations_endpoint_returns_versioned_envelope -- --exact
cargo test dashboard::tests::timeline_endpoint_returns_ordered_events -- --exact
cargo test dashboard::tests::session_graph_endpoint_returns_graph_payload -- --exact
cargo test dashboard::tests::session_details_endpoint_unknown_session_includes_null_session_requests -- --exact
```

Expected: FAIL where tests still expect raw arrays/objects without envelope metadata.

- [ ] **Step 3: Write minimal implementation**

Wrap responses:

```rust
axum::Json(VersionedEnvelope {
    version: "2026-04-11",
    generated_at_ms: chrono::Utc::now().timestamp_millis(),
    data: links_or_rows_or_graph_or_details,
})
```

Apply to handlers:
- `api_correlations`
- `api_explanations`
- `api_timeline`
- `api_session_graph`
- `api_session_details` (for success and unknown payload paths)

Keep existing error semantics for bad request / not found unchanged.

- [ ] **Step 4: Run tests to verify pass**

Run:

```bash
cargo test dashboard::tests::correlations_endpoint_returns_versioned_envelope -- --exact
cargo test dashboard::tests::explanations_endpoint_returns_rows -- --exact
cargo test dashboard::tests::timeline_endpoint_returns_ordered_events -- --exact
cargo test dashboard::tests::session_graph_endpoint_returns_graph_payload -- --exact
cargo test dashboard::tests::session_details_endpoint_unknown_session_includes_null_session_requests -- --exact
```

Expected: PASS with envelope-aware assertions.

- [ ] **Step 5: Commit**

```bash
git add src/dashboard.rs
git commit -m "feat: version session intelligence API responses"
```

---

### Task 4: Update dashboard frontend to use merged sessions endpoint and envelope payloads

**Files:**
- Modify: `src/dashboard.html`
- Test: `src/dashboard.rs` (HTML contract tests)

- [ ] **Step 1: Write failing HTML contract tests**

Add/adjust tests:

```rust
#[test]
fn dashboard_html_uses_merged_sessions_endpoint() {
    let html = crate::dashboard::dashboard_html();
    assert!(html.contains("/api/sessions/merged"));
}

#[test]
fn dashboard_html_handles_versioned_intel_envelopes() {
    let html = crate::dashboard::dashboard_html();
    assert!(html.contains("payload?.data"));
    assert!(html.contains("Confidence is heuristic"));
}
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
cargo test dashboard::tests::dashboard_html_uses_merged_sessions_endpoint -- --exact
cargo test dashboard::tests::dashboard_html_handles_versioned_intel_envelopes -- --exact
```

Expected: FAIL because `dashboard.html` still fetches `/api/sessions` + `/api/claude-sessions` and assumes raw payloads.

- [ ] **Step 3: Write minimal implementation**

Add helper in `dashboard.html`:

```javascript
function envelopeData(payload) {
  if (payload && typeof payload === 'object' && payload.data !== undefined) {
    return payload.data;
  }
  return payload;
}
```

Update fetch consumers:

```javascript
const resp = await fetch('/api/sessions/merged');
const payload = await resp.json();
const rows = envelopeData(payload);
```

And for request intelligence:

```javascript
const payload = await resp.json();
const links = envelopeData(payload);
```

Apply to:
- `loadSessions`
- `loadCorrelations`
- `loadExplanations`
- timeline/session-details/session-graph loaders

Add confidence label in explanation/correlation render path:

```javascript
const confidenceHint = 'Confidence is heuristic (ranking signal, not probability).';
```

- [ ] **Step 4: Run tests to verify pass**

Run:

```bash
cargo test dashboard::tests::dashboard_html_uses_merged_sessions_endpoint -- --exact
cargo test dashboard::tests::dashboard_html_handles_versioned_intel_envelopes -- --exact
cargo test dashboard::tests::dashboard_html_includes_correlation_panel -- --exact
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/dashboard.html src/dashboard.rs
git commit -m "feat: consume versioned intel payloads in dashboard"
```

---

### Task 5: Update docs and run full regression

**Files:**
- Modify: `README.md`
- Verify: `src/dashboard.rs`, `src/stats.rs`, `src/dashboard.html`

- [ ] **Step 1: Write failing test for docs consistency (endpoint contract smoke)**

Add a dashboard contract test asserting merged sessions route exists and removed split-session dependency does not:

```rust
#[test]
fn dashboard_html_no_longer_fetches_split_session_sources() {
    let html = crate::dashboard::dashboard_html();
    assert!(!html.contains("fetch('/api/sessions')"));
    assert!(!html.contains("fetch('/api/claude-sessions')"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test dashboard::tests::dashboard_html_no_longer_fetches_split_session_sources -- --exact`

Expected: FAIL before README/frontend finalization if old calls remain.

- [ ] **Step 3: Write minimal implementation + docs update**

Update README API section to include:

```markdown
- `GET /api/sessions/merged` — merged proxy/local session rows
- `GET /api/correlations?request_id=...&limit=...` — versioned envelope `{ version, generated_at_ms, data }`
- `GET /api/explanations?request_id=...&limit=...` — versioned envelope
- `GET /api/timeline?session_id=...` — versioned envelope
- `GET /api/session-graph?session_id=...` — versioned envelope
- `GET /api/session-details?session_id=...` — versioned envelope
```

- [ ] **Step 4: Run full verification**

Run:

```bash
cargo build
cargo test --no-run
cargo test
```

Expected:
- Build/test compilation succeeds.
- Full test suite passes.

- [ ] **Step 5: Commit**

```bash
git add README.md src/dashboard.rs src/dashboard.html src/stats.rs
git commit -m "docs: align API docs with versioned session intelligence contracts"
```

---

## Spec Coverage Check

- ✅ Requests/session/correlation/explanation/timeline flow is covered by contract and endpoint tasks (Tasks 1-4).
- ✅ Freshness/version metadata added to intelligence endpoints (Task 3).
- ✅ Server-side session merge + reduced frontend reconciliation duplication (Tasks 1, 2, 4).
- ✅ Confidence/provenance communication in UI contract (Task 4).
- ✅ Regression and docs synchronization (Task 5).

## Placeholder Scan

- No TBD/TODO placeholders.
- Every code-changing step includes concrete code.
- Every verification step includes explicit command + expected outcome.

## Type Consistency Check

- Envelope type uses stable fields: `version`, `generated_at_ms`, `data` across all targeted endpoints.
- Merged session DTO fields are consistent with frontend consumption.
- Tests and implementation reference same endpoint paths and payload keys.
