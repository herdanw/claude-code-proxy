# Proxy v2 Rewrite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite ClaudeProxy into a proxy-only request analysis platform that captures all proxied traffic, stores searchable request/response data, detects anomalies with hypotheses, and scores model conformance against a 140-parameter fingerprint.

**Architecture:** Keep one Rust binary with modular internals (`proxy`, `store`, `analyzer`, `model_profile`, `dashboard`, `types`). Preserve the strong streaming proxy core from `proxy.rs`, replace the legacy monolithic data path with focused SQLite + FTS5 persistence, and add background analysis + report-card dashboard APIs/UI. All dropped modules are removed; all intelligence comes from proxy wire data.

**Tech Stack:** Rust, Tokio, Axum, Reqwest, Rusqlite (SQLite + FTS5), Serde, Tower-HTTP, WebSocket, Vanilla HTML/CSS/JS, Chart.js.

---

## Scope Check

This is one coherent subsystem (proxy-only capture + analysis + dashboard) with tightly coupled data flow, so a single implementation plan is appropriate.

## File Structure

- Create: `src/types.rs` — shared DTOs/enums used by proxy/store/analyzer/dashboard
- Create: `src/store.rs` — SQLite schema, persistence, FTS queries, session/model aggregates
- Create: `src/analyzer.rs` — anomaly detection + hypothesis generation + health score helpers
- Create: `src/model_profile.rs` — model profile config, wildcard mapping, conformance calculations, auto-tuning
- Modify: `src/proxy.rs` — keep proxy core, add richer extraction and store integration
- Modify: `src/dashboard.rs` — replace with v2 API + websocket broadcaster
- Modify: `src/main.rs` — new CLI args, worker orchestration, module wiring
- Modify: `src/dashboard.html` — full SPA rewrite with 5 tabs
- Modify: `Cargo.toml` — minimal dependency adjustments for v2 features
- Remove: `src/local_context.rs`, `src/correlation_engine.rs`, `src/correlation.rs`, `src/explainer.rs`, `src/settings_admin.rs`, `src/session_admin.rs`, legacy `src/stats.rs`

---

### Task 1: Create shared v2 domain types

**Files:**
- Create: `src/types.rs`
- Modify: `src/main.rs`
- Test: `src/types.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_status_kind_roundtrip() {
        let value = RequestStatusKind::ServerError;
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(json, "\"server_error\"");
        let decoded: RequestStatusKind = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, RequestStatusKind::ServerError);
    }

    #[test]
    fn anomaly_kind_roundtrip() {
        let value = AnomalyKind::SlowTtft;
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(json, "\"slow_ttft\"");
        let decoded: AnomalyKind = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, AnomalyKind::SlowTtft);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test types::tests::request_status_kind_roundtrip -- --exact`
Expected: FAIL with unresolved module/type errors for `RequestStatusKind`.

- [ ] **Step 3: Write minimal implementation**

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatusKind {
    Pending,
    Success,
    ClientError,
    ServerError,
    Timeout,
    ConnectionError,
    ProxyError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnomalyKind {
    SlowTtft,
    Stall,
    Timeout,
    ApiError,
    ClientError,
    RateLimited,
    Overload,
    HighTokens,
    CacheMiss,
    InterruptedStream,
    MaxTokensHit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestRecord {
    pub id: String,
    pub session_id: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub method: String,
    pub path: String,
    pub model: String,
    pub stream: bool,
    pub status_code: Option<u16>,
    pub status_kind: RequestStatusKind,
    pub ttft_ms: Option<f64>,
    pub duration_ms: Option<f64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_creation_tokens: Option<u64>,
    pub thinking_tokens: Option<u64>,
    pub request_size_bytes: u64,
    pub response_size_bytes: u64,
    pub stall_count: u32,
    pub stall_details_json: String,
    pub error_summary: Option<String>,
    pub stop_reason: Option<String>,
    pub content_block_types_json: String,
    pub anomalies_json: String,
    pub analyzed: bool,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test types::tests -- --nocapture`
Expected: PASS for serialization roundtrip tests.

- [ ] **Step 5: Commit**

```bash
git add src/types.rs src/main.rs
git commit -m "feat: add shared proxy v2 domain types"
```

---

### Task 2: Build v2 SQLite store and schema (including FTS5)

**Files:**
- Create: `src/store.rs`
- Modify: `src/main.rs`
- Test: `src/store.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_schema_creates_core_tables() {
        let path = std::env::temp_dir().join(format!("proxy-v2-{}.db", uuid::Uuid::new_v4()));
        let store = Store::new(&path).unwrap();
        let tables = store.list_table_names().unwrap();

        assert!(tables.contains(&"requests".to_string()));
        assert!(tables.contains(&"request_bodies".to_string()));
        assert!(tables.contains(&"request_bodies_fts".to_string()));
        assert!(tables.contains(&"tool_usage".to_string()));
        assert!(tables.contains(&"anomalies".to_string()));
        assert!(tables.contains(&"model_profiles".to_string()));
        assert!(tables.contains(&"sessions".to_string()));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test store::tests::initialize_schema_creates_core_tables -- --exact`
Expected: FAIL with unresolved `Store` and missing methods.

- [ ] **Step 3: Write minimal implementation**

```rust
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Arc;

pub struct Store {
    db: Arc<Mutex<Connection>>,
}

impl Store {
    pub fn new(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        let store = Self { db: Arc::new(Mutex::new(conn)) };
        store.initialize_schema()?;
        Ok(store)
    }

    fn initialize_schema(&self) -> Result<(), rusqlite::Error> {
        let db = self.db.lock();
        db.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS requests (
                id TEXT PRIMARY KEY,
                session_id TEXT,
                timestamp_ms INTEGER NOT NULL,
                method TEXT NOT NULL,
                path TEXT NOT NULL,
                model TEXT NOT NULL DEFAULT '',
                stream INTEGER NOT NULL DEFAULT 0,
                status_code INTEGER,
                status_kind TEXT NOT NULL DEFAULT 'pending',
                ttft_ms REAL,
                duration_ms REAL,
                input_tokens INTEGER,
                output_tokens INTEGER,
                cache_read_tokens INTEGER,
                cache_creation_tokens INTEGER,
                thinking_tokens INTEGER,
                request_size_bytes INTEGER,
                response_size_bytes INTEGER,
                stall_count INTEGER NOT NULL DEFAULT 0,
                stall_details_json TEXT NOT NULL DEFAULT '[]',
                error_summary TEXT,
                stop_reason TEXT,
                content_block_types_json TEXT NOT NULL DEFAULT '[]',
                anomalies_json TEXT NOT NULL DEFAULT '[]',
                analyzed INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_requests_timestamp ON requests(timestamp_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_requests_session ON requests(session_id);
            CREATE INDEX IF NOT EXISTS idx_requests_model ON requests(model);
            CREATE INDEX IF NOT EXISTS idx_requests_analyzed ON requests(analyzed) WHERE analyzed = 0;

            CREATE TABLE IF NOT EXISTS request_bodies (
                request_id TEXT PRIMARY KEY REFERENCES requests(id),
                request_body TEXT,
                response_body TEXT,
                truncated INTEGER NOT NULL DEFAULT 0
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS request_bodies_fts USING fts5(
                request_id,
                request_body,
                response_body,
                content=request_bodies,
                content_rowid=rowid
            );

            CREATE TABLE IF NOT EXISTS tool_usage (
                id TEXT PRIMARY KEY,
                request_id TEXT NOT NULL REFERENCES requests(id),
                tool_name TEXT NOT NULL,
                tool_input_json TEXT,
                success INTEGER,
                is_error INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_tool_usage_request ON tool_usage(request_id);
            CREATE INDEX IF NOT EXISTS idx_tool_usage_name ON tool_usage(tool_name);

            CREATE TABLE IF NOT EXISTS anomalies (
                id TEXT PRIMARY KEY,
                request_id TEXT NOT NULL REFERENCES requests(id),
                kind TEXT NOT NULL,
                severity TEXT NOT NULL,
                summary TEXT NOT NULL,
                hypothesis TEXT,
                evidence_json TEXT NOT NULL DEFAULT '{}',
                created_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_anomalies_request ON anomalies(request_id);
            CREATE INDEX IF NOT EXISTS idx_anomalies_kind ON anomalies(kind);
            CREATE INDEX IF NOT EXISTS idx_anomalies_severity ON anomalies(severity);

            CREATE TABLE IF NOT EXISTS model_profiles (
                model_name TEXT PRIMARY KEY,
                behavior_class TEXT,
                config_json TEXT NOT NULL DEFAULT '{}',
                observed_json TEXT NOT NULL DEFAULT '{}',
                sample_count INTEGER NOT NULL DEFAULT 0,
                last_updated_ms INTEGER
            );

            CREATE TABLE IF NOT EXISTS sessions (
                session_id TEXT PRIMARY KEY,
                first_seen_ms INTEGER NOT NULL,
                last_seen_ms INTEGER NOT NULL,
                request_count INTEGER NOT NULL DEFAULT 0,
                error_count INTEGER NOT NULL DEFAULT 0,
                total_input_tokens INTEGER NOT NULL DEFAULT 0,
                total_output_tokens INTEGER NOT NULL DEFAULT 0
            );
            "#,
        )?;
        Ok(())
    }

    pub fn list_table_names(&self) -> Result<Vec<String>, rusqlite::Error> {
        let db = self.db.lock();
        let mut stmt = db.prepare("SELECT name FROM sqlite_master WHERE type IN ('table','view')")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows { out.push(row?); }
        Ok(out)
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test store::tests::initialize_schema_creates_core_tables -- --exact`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/store.rs src/main.rs
git commit -m "feat: add proxy v2 sqlite store schema with fts5"
```

---

### Task 3: Implement store CRUD and FTS search APIs

**Files:**
- Modify: `src/store.rs`
- Modify: `src/types.rs`
- Test: `src/store.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn add_request_and_search_by_fts() {
    let path = std::env::temp_dir().join(format!("proxy-v2-{}.db", uuid::Uuid::new_v4()));
    let store = Store::new(&path).unwrap();

    let request = sample_request("req-1", "claude-opus-4-1");
    store.add_request(&request).unwrap();
    store.write_bodies("req-1", "{\"input\":\"latency issue\"}", "{\"output\":\"observed stall\"}", false).unwrap();

    let ids = store.search_request_ids("stall", 10, 0).unwrap();
    assert_eq!(ids, vec!["req-1".to_string()]);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test store::tests::add_request_and_search_by_fts -- --exact`
Expected: FAIL with missing `add_request`, `write_bodies`, `search_request_ids`.

- [ ] **Step 3: Write minimal implementation**

```rust
impl Store {
    pub fn add_request(&self, req: &crate::types::RequestRecord) -> Result<(), rusqlite::Error> {
        let db = self.db.lock();
        db.execute(
            r#"INSERT INTO requests (
                id, session_id, timestamp_ms, method, path, model, stream, status_code, status_kind,
                ttft_ms, duration_ms, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
                thinking_tokens, request_size_bytes, response_size_bytes, stall_count, stall_details_json,
                error_summary, stop_reason, content_block_types_json, anomalies_json, analyzed
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)"#,
            params![
                req.id,
                req.session_id,
                req.timestamp.timestamp_millis(),
                req.method,
                req.path,
                req.model,
                req.stream as i64,
                req.status_code,
                serde_json::to_string(&req.status_kind).unwrap_or("\"pending\"".into()).replace('"', ""),
                req.ttft_ms,
                req.duration_ms,
                req.input_tokens,
                req.output_tokens,
                req.cache_read_tokens,
                req.cache_creation_tokens,
                req.thinking_tokens,
                req.request_size_bytes as i64,
                req.response_size_bytes as i64,
                req.stall_count as i64,
                req.stall_details_json,
                req.error_summary,
                req.stop_reason,
                req.content_block_types_json,
                req.anomalies_json,
                req.analyzed as i64,
            ],
        )?;
        Ok(())
    }

    pub fn write_bodies(&self, request_id: &str, request_body: &str, response_body: &str, truncated: bool) -> Result<(), rusqlite::Error> {
        let db = self.db.lock();
        db.execute(
            "INSERT OR REPLACE INTO request_bodies (request_id, request_body, response_body, truncated) VALUES (?1, ?2, ?3, ?4)",
            params![request_id, request_body, response_body, truncated as i64],
        )?;

        db.execute(
            "INSERT INTO request_bodies_fts (rowid, request_id, request_body, response_body) SELECT rowid, request_id, request_body, response_body FROM request_bodies WHERE request_id = ?1",
            params![request_id],
        )?;
        Ok(())
    }

    pub fn search_request_ids(&self, query: &str, limit: usize, offset: usize) -> Result<Vec<String>, rusqlite::Error> {
        let db = self.db.lock();
        let mut stmt = db.prepare(
            "SELECT request_id FROM request_bodies_fts WHERE request_bodies_fts MATCH ?1 LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(params![query, limit as i64, offset as i64], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows { out.push(row?); }
        Ok(out)
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test store::tests::add_request_and_search_by_fts -- --exact`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/store.rs src/types.rs
git commit -m "feat: add request persistence and fts search APIs"
```

---

### Task 4: Refactor proxy pipeline to capture full wire data for v2

**Files:**
- Modify: `src/proxy.rs`
- Modify: `src/types.rs`
- Test: `src/proxy.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn extract_sse_usage_captures_thinking_tokens_and_stop_reason() {
    let chunk = b"data: {\"type\":\"message_delta\",\"delta\":{},\"usage\":{\"input_tokens\":10,\"output_tokens\":20,\"cache_read_input_tokens\":5,\"cache_creation_input_tokens\":2,\"thinking_tokens\":7},\"stop_reason\":\"end_turn\"}\n\n";

    let mut usage = UsageData::default();
    let mut stop_reason = None;
    extract_sse_usage_and_metadata(chunk, &mut usage, &mut stop_reason);

    assert_eq!(usage.thinking_tokens, Some(7));
    assert_eq!(stop_reason.as_deref(), Some("end_turn"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test proxy::tests::extract_sse_usage_captures_thinking_tokens_and_stop_reason -- --exact`
Expected: FAIL because `extract_sse_usage_and_metadata` and `thinking_tokens` field do not exist.

- [ ] **Step 3: Write minimal implementation**

```rust
#[derive(Default)]
struct UsageData {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
    cache_creation_tokens: Option<u64>,
    thinking_tokens: Option<u64>,
}

fn extract_sse_usage_and_metadata(chunk: &[u8], usage: &mut UsageData, stop_reason: &mut Option<String>) {
    let text = match std::str::from_utf8(chunk) {
        Ok(t) => t,
        Err(_) => return,
    };

    for line in text.lines() {
        if !line.starts_with("data: ") {
            continue;
        }
        let payload = &line[6..];
        let Ok(data) = serde_json::from_str::<serde_json::Value>(payload) else { continue };

        if let Some(u) = data.get("usage") {
            if let Some(v) = u.get("input_tokens").and_then(|v| v.as_u64()) { usage.input_tokens = Some(v); }
            if let Some(v) = u.get("output_tokens").and_then(|v| v.as_u64()) { usage.output_tokens = Some(v); }
            if let Some(v) = u.get("cache_read_input_tokens").and_then(|v| v.as_u64()) { usage.cache_read_tokens = Some(v); }
            if let Some(v) = u.get("cache_creation_input_tokens").and_then(|v| v.as_u64()) { usage.cache_creation_tokens = Some(v); }
            if let Some(v) = u.get("thinking_tokens").and_then(|v| v.as_u64()) { usage.thinking_tokens = Some(v); }
        }

        if let Some(reason) = data.get("stop_reason").and_then(|v| v.as_str()) {
            *stop_reason = Some(reason.to_string());
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test proxy::tests -- --nocapture`
Expected: PASS including new extraction test.

- [ ] **Step 5: Commit**

```bash
git add src/proxy.rs src/types.rs
git commit -m "feat: enrich proxy extraction for thinking tokens and stop reasons"
```

---

### Task 5: Replace main orchestration with v2 modules and CLI

**Files:**
- Modify: `src/main.rs`
- Modify: `Cargo.toml`
- Test: `src/main.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn parse_args_exposes_v2_threshold_flags() {
    use clap::Parser;

    let args = Args::parse_from([
        "claude-proxy",
        "--target", "https://api.anthropic.com",
        "--slow-ttft-threshold", "3000",
        "--stall-threshold", "0.5",
    ]);

    assert_eq!(args.slow_ttft_threshold, 3000);
    assert!((args.stall_threshold - 0.5).abs() < f64::EPSILON);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test main::tests::parse_args_exposes_v2_threshold_flags -- --exact`
Expected: FAIL if fields are missing or old args remain.

- [ ] **Step 3: Write minimal implementation**

```rust
mod analyzer;
mod dashboard;
mod model_profile;
mod proxy;
mod store;
mod types;

#[derive(clap::Parser, Debug, Clone)]
struct Args {
    #[arg(long)]
    target: String,
    #[arg(long, default_value_t = 8000)]
    port: u16,
    #[arg(long, default_value_t = 3000)]
    dashboard_port: u16,
    #[arg(long)]
    data_dir: Option<std::path::PathBuf>,
    #[arg(long)]
    model_config: Option<std::path::PathBuf>,
    #[arg(long, default_value_t = 0.5)]
    stall_threshold: f64,
    #[arg(long, default_value_t = 3000)]
    slow_ttft_threshold: u64,
    #[arg(long, default_value_t = 2 * 1024 * 1024)]
    max_body_size: usize,
    #[arg(long, default_value_t = false)]
    open_browser: bool,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test main::tests::parse_args_exposes_v2_threshold_flags -- --exact`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs Cargo.toml
git commit -m "feat: wire proxy v2 module graph and cli arguments"
```

---

### Task 6: Implement model profile config, wildcard mapping, and auto-tune primitives

**Files:**
- Create: `src/model_profile.rs`
- Modify: `src/types.rs`
- Test: `src/model_profile.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn resolve_behavior_class_uses_wildcard_mapping() {
    let config = ModelConfig {
        profiles: std::collections::HashMap::new(),
        model_mappings: std::collections::HashMap::from([
            ("claude-opus-4-*".to_string(), "opus".to_string()),
        ]),
    };

    let class = resolve_behavior_class(&config, "claude-opus-4-20260301");
    assert_eq!(class.as_deref(), Some("opus"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test model_profile::tests::resolve_behavior_class_uses_wildcard_mapping -- --exact`
Expected: FAIL with missing `ModelConfig` and `resolve_behavior_class`.

- [ ] **Step 3: Write minimal implementation**

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub profiles: HashMap<String, serde_json::Value>,
    pub model_mappings: HashMap<String, String>,
}

pub fn resolve_behavior_class(config: &ModelConfig, model_name: &str) -> Option<String> {
    for (pattern, class) in &config.model_mappings {
        if wildcard_match(pattern, model_name) {
            return Some(class.clone());
        }
    }
    None
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        value.starts_with(prefix)
    } else {
        value == pattern
    }
}

pub fn should_auto_tune(sample_count: u64) -> bool {
    sample_count >= 50 && sample_count % 50 == 0
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test model_profile::tests -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/model_profile.rs src/types.rs
git commit -m "feat: add model profile mapping and auto-tune thresholds"
```

---

### Task 7: Implement anomaly detection + hypothesis generation + health score helpers

**Files:**
- Create: `src/analyzer.rs`
- Modify: `src/types.rs`
- Test: `src/analyzer.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn detects_slow_ttft_and_generates_hypothesis() {
    let req = sample_request_with_ttft("req-1", "claude-sonnet-4-5", 4200.0);
    let rules = AnalyzerRules { slow_ttft_threshold_ms: 3000.0, stall_threshold_s: 0.5 };

    let anomalies = detect_anomalies(&req, &rules, &[]);
    assert_eq!(anomalies.len(), 1);
    assert_eq!(anomalies[0].kind, AnomalyKind::SlowTtft);
    assert!(anomalies[0].hypothesis.as_ref().unwrap().contains("4200"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test analyzer::tests::detects_slow_ttft_and_generates_hypothesis -- --exact`
Expected: FAIL due to missing analyzer functions/types.

- [ ] **Step 3: Write minimal implementation**

```rust
use crate::types::{AnomalyKind, Severity, RequestRecord};

#[derive(Debug, Clone)]
pub struct AnalyzerRules {
    pub slow_ttft_threshold_ms: f64,
    pub stall_threshold_s: f64,
}

#[derive(Debug, Clone)]
pub struct DetectedAnomaly {
    pub kind: AnomalyKind,
    pub severity: Severity,
    pub summary: String,
    pub hypothesis: Option<String>,
}

pub fn detect_anomalies(req: &RequestRecord, rules: &AnalyzerRules, _recent: &[RequestRecord]) -> Vec<DetectedAnomaly> {
    let mut out = Vec::new();

    if let Some(ttft) = req.ttft_ms {
        if ttft > rules.slow_ttft_threshold_ms {
            out.push(DetectedAnomaly {
                kind: AnomalyKind::SlowTtft,
                severity: if ttft > 8000.0 { Severity::Error } else { Severity::Warning },
                summary: format!("TTFT {:.0}ms exceeded threshold {:.0}ms", ttft, rules.slow_ttft_threshold_ms),
                hypothesis: Some(format!("TTFT {:.0}ms is elevated for model {}. Check context size, recent 429s, and upstream latency spikes.", ttft, req.model)),
            });
        }
    }

    out
}

pub fn compute_health_score(error_count: usize, warning_count: usize, info_count: usize, critical_count: usize) -> i32 {
    (100 - (error_count as i32 * 5) - (warning_count as i32 * 2) - (info_count as i32) - (critical_count as i32 * 10)).max(0)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test analyzer::tests -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/analyzer.rs src/types.rs
git commit -m "feat: add anomaly detection and health scoring primitives"
```

---

### Task 8: Add conformance engine skeleton with full 140-parameter registry

**Files:**
- Modify: `src/model_profile.rs`
- Modify: `src/types.rs`
- Test: `src/model_profile.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn parameter_registry_contains_140_entries() {
    let params = fingerprint_parameter_names();
    assert_eq!(params.len(), 140);
    assert!(params.contains(&"avg_ttft_ms"));
    assert!(params.contains(&"unknown_delta_types"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test model_profile::tests::parameter_registry_contains_140_entries -- --exact`
Expected: FAIL because `fingerprint_parameter_names` does not exist.

- [ ] **Step 3: Write minimal implementation**

```rust
pub fn fingerprint_parameter_names() -> Vec<&'static str> {
    vec![
        "avg_ttft_ms", "median_ttft_ms", "p95_ttft_ms", "p99_ttft_ms", "min_ttft_ms", "max_ttft_ms", "ttft_stddev_ms", "avg_duration_ms", "tokens_per_second", "avg_inter_chunk_ms", "chunk_timing_variance", "ttft_vs_context_correlation",
        "thinking_frequency", "avg_thinking_tokens", "median_thinking_tokens", "thinking_token_ratio", "thinking_depth_by_complexity", "redacted_thinking_frequency", "thinking_before_tool_rate", "thinking_per_turn_variance", "max_thinking_tokens", "effort_response_correlation",
        "avg_input_tokens", "avg_output_tokens", "median_output_tokens", "p95_output_tokens", "output_input_ratio", "max_tokens_hit_rate", "cache_creation_rate", "cache_hit_rate", "avg_cache_read_tokens", "cache_miss_after_hit_rate", "total_tokens_per_request", "output_token_consistency", "token_efficiency", "context_window_utilization",
        "tool_call_rate", "tools_per_turn", "max_tools_per_turn", "multi_tool_rate", "unique_tool_diversity", "tool_preference_distribution", "tool_chain_depth", "max_tool_chain_depth", "tool_success_rate", "tool_retry_rate", "tool_adaptation_rate", "tool_input_avg_size", "tool_call_position", "text_before_tool_ratio", "tool_use_after_thinking", "deferred_tool_usage",
        "avg_content_blocks", "max_content_blocks", "text_block_count_avg", "avg_text_block_length", "block_type_distribution", "stop_reason_distribution", "end_turn_rate", "code_in_response_rate", "markdown_usage_rate", "response_structure_variance", "multi_text_block_rate", "interleaved_thinking_rate", "citations_frequency", "connector_text_frequency",
        "stall_rate", "avg_stall_duration_ms", "max_stall_duration_ms", "stalls_per_request", "stall_position_distribution", "stream_completion_rate", "interrupted_stream_rate", "ping_frequency", "avg_chunks_per_response", "bytes_per_chunk_avg", "first_content_event_ms", "stream_warmup_pattern",
        "error_rate", "server_error_rate", "rate_limit_rate", "overload_rate", "client_error_rate", "timeout_rate", "connection_error_rate", "error_type_distribution", "refusal_rate", "error_recovery_rate", "consecutive_error_max", "error_time_clustering",
        "avg_requests_per_session", "session_duration_avg_ms", "inter_request_gap_avg_ms", "inter_request_gap_variance", "context_growth_rate", "conversation_depth_avg", "session_error_clustering", "session_tool_evolution", "session_ttft_trend", "session_token_trend",
        "system_prompt_frequency", "system_prompt_avg_size", "avg_message_count", "tools_provided_avg", "tool_choice_distribution", "temperature_distribution", "max_tokens_setting_avg", "image_input_rate", "document_input_rate", "request_body_avg_bytes",
        "effort_param_usage", "effort_thinking_correlation", "effort_output_correlation", "effort_ttft_correlation", "speed_mode_usage", "speed_mode_ttft_impact", "speed_mode_quality_impact", "task_budget_usage",
        "cache_control_usage_rate", "cache_scope_global_rate", "cache_ttl_1h_rate", "cache_edit_usage_rate", "cache_cost_savings_ratio", "cache_stability", "cache_warmup_requests", "cache_invalidation_pattern",
        "beta_features_count", "beta_feature_set", "custom_headers_present", "anthropic_version", "provider_type", "auth_method", "request_id_tracking", "response_request_id",
        "unknown_sse_event_types", "unknown_content_block_types", "unknown_request_fields", "unknown_header_patterns", "unknown_stop_reasons", "unknown_delta_types",
    ]
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test model_profile::tests::parameter_registry_contains_140_entries -- --exact`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/model_profile.rs src/types.rs
git commit -m "feat: add full 140-parameter conformance registry"
```

---

### Task 9: Implement dashboard v2 REST API + websocket updates

**Files:**
- Modify: `src/dashboard.rs`
- Modify: `src/store.rs`
- Modify: `src/main.rs`
- Test: `src/dashboard.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn health_endpoint_returns_report_card_metrics() {
    let app = build_test_router();
    let response = app
        .oneshot(axum::http::Request::builder().uri("/api/health").body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), axum::http::StatusCode::OK);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test dashboard::tests::health_endpoint_returns_report_card_metrics -- --exact`
Expected: FAIL because `/api/health` route is missing in rewritten router.

- [ ] **Step 3: Write minimal implementation**

```rust
pub fn build_router(state: Arc<AppState>) -> axum::Router {
    axum::Router::new()
        .route("/api/health", axum::routing::get(api_health))
        .route("/api/anomalies/recent", axum::routing::get(api_recent_anomalies))
        .route("/api/requests", axum::routing::get(api_requests))
        .route("/api/requests/:id", axum::routing::get(api_request_detail))
        .route("/api/requests/:id/body", axum::routing::get(api_request_body))
        .route("/api/requests/:id/tools", axum::routing::get(api_request_tools))
        .route("/api/models", axum::routing::get(api_models))
        .route("/api/models/:name/profile", axum::routing::get(api_model_profile))
        .route("/api/models/:name/comparison", axum::routing::get(api_model_comparison))
        .route("/api/model-config", axum::routing::get(api_model_config).put(api_put_model_config))
        .route("/api/anomalies", axum::routing::get(api_anomalies))
        .route("/api/anomalies/:id", axum::routing::get(api_anomaly_detail))
        .route("/api/sessions", axum::routing::get(api_sessions))
        .route("/api/sessions/:id", axum::routing::get(api_session_detail))
        .route("/ws", axum::routing::get(ws_handler))
        .with_state(state)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test dashboard::tests -- --nocapture`
Expected: PASS for health endpoint and route availability tests.

- [ ] **Step 5: Commit**

```bash
git add src/dashboard.rs src/store.rs src/main.rs
git commit -m "feat: add proxy v2 dashboard api surface and websocket route"
```

---

### Task 10: Rewrite dashboard frontend for 5-tab report-card UX

**Files:**
- Modify: `src/dashboard.html`
- Test: `src/dashboard.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn dashboard_html_contains_required_v2_tabs() {
    let html = include_str!("dashboard.html");
    assert!(html.contains("data-tab=\"overview\""));
    assert!(html.contains("data-tab=\"requests\""));
    assert!(html.contains("data-tab=\"conformance\""));
    assert!(html.contains("data-tab=\"anomalies\""));
    assert!(html.contains("data-tab=\"sessions\""));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test dashboard::tests::dashboard_html_contains_required_v2_tabs -- --exact`
Expected: FAIL if old dashboard markup is still present.

- [ ] **Step 3: Write minimal implementation**

```html
<nav class="tabs">
  <button data-tab="overview" class="active">Overview</button>
  <button data-tab="requests">Requests</button>
  <button data-tab="conformance">Model Conformance</button>
  <button data-tab="anomalies">Anomalies</button>
  <button data-tab="sessions">Sessions</button>
</nav>

<section id="tab-overview" class="tab-panel active">
  <div id="health-score-banner"></div>
  <div id="issues-summary"></div>
  <div id="conformance-scoreboard"></div>
  <div id="key-metrics-row"></div>
  <div id="recent-anomalies"></div>
</section>

<section id="tab-requests" class="tab-panel">
  <form id="request-filters"></form>
  <table id="requests-table"></table>
  <aside id="request-detail"></aside>
</section>
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test dashboard::tests::dashboard_html_contains_required_v2_tabs -- --exact`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/dashboard.html src/dashboard.rs
git commit -m "feat: rewrite dashboard html for v2 report card and tabbed analysis"
```

---

### Task 11: Add analyzer worker loop, conformance updates, and auto-tuning in runtime

**Files:**
- Modify: `src/main.rs`
- Modify: `src/analyzer.rs`
- Modify: `src/model_profile.rs`
- Modify: `src/store.rs`
- Test: `src/main.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn analyzer_worker_marks_requests_as_analyzed() {
    let (store, request_id) = seed_unanalyzed_request();
    run_analyzer_tick(store.clone()).await.unwrap();
    let req = store.get_request(&request_id).unwrap().unwrap();
    assert!(req.analyzed);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test main::tests::analyzer_worker_marks_requests_as_analyzed -- --exact`
Expected: FAIL because `run_analyzer_tick` and mark-analyzed flow do not exist.

- [ ] **Step 3: Write minimal implementation**

```rust
pub async fn run_analyzer_tick(store: Arc<Store>, rules: AnalyzerRules) -> anyhow::Result<()> {
    let pending = store.list_unanalyzed_requests(200)?;
    for req in pending {
        let recent = store.list_recent_requests_for_model(&req.model, 50)?;
        let anomalies = analyzer::detect_anomalies(&req, &rules, &recent);
        store.insert_anomalies(&req.id, &anomalies)?;
        store.mark_analyzed(&req.id)?;

        let sample_count = store.increment_model_sample_count(&req.model)?;
        if model_profile::should_auto_tune(sample_count) {
            let observed = store.compute_model_observed_stats(&req.model)?;
            store.upsert_model_observed(&req.model, &observed)?;
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test main::tests::analyzer_worker_marks_requests_as_analyzed -- --exact`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/analyzer.rs src/model_profile.rs src/store.rs
git commit -m "feat: add background analyzer worker and model auto-tuning loop"
```

---

### Task 12: Forward-compatibility + unknown field tracking + integration verification

**Files:**
- Modify: `src/proxy.rs`
- Modify: `src/analyzer.rs`
- Modify: `src/store.rs`
- Create: `tests/proxy_v2_integration.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn unknown_sse_and_request_fields_are_preserved_and_counted() {
    let fixture = include_str!("fixtures/new-protocol-events.sse");
    let result = ingest_fixture_and_collect_unknowns(fixture).await;

    assert!(result.unknown_sse_event_types.contains(&"message_patch".to_string()));
    assert!(result.unknown_request_fields.contains(&"reasoning_budget".to_string()));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test proxy_v2_integration unknown_sse_and_request_fields_are_preserved_and_counted -- --exact`
Expected: FAIL because unknown-field tracking output is missing.

- [ ] **Step 3: Write minimal implementation**

```rust
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct UnknownFieldStats {
    pub unknown_sse_event_types: Vec<String>,
    pub unknown_content_block_types: Vec<String>,
    pub unknown_request_fields: Vec<String>,
    pub unknown_header_patterns: Vec<String>,
    pub unknown_stop_reasons: Vec<String>,
    pub unknown_delta_types: Vec<String>,
}

fn record_unknown_event(stats: &mut UnknownFieldStats, event_type: &str) {
    if !KNOWN_SSE_EVENTS.contains(&event_type) && !stats.unknown_sse_event_types.iter().any(|e| e == event_type) {
        stats.unknown_sse_event_types.push(event_type.to_string());
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test proxy_v2_integration -- --nocapture`
Expected: PASS with preserved unknown-field metrics.

- [ ] **Step 5: Commit**

```bash
git add src/proxy.rs src/analyzer.rs src/store.rs src/main.rs tests/proxy_v2_integration.rs
git commit -m "feat: add protocol forward-compat tracking and integration coverage"
```

---

## Final Verification Checklist

- [ ] Run full test suite

Run: `cargo test`
Expected: PASS.

- [ ] Run formatting and lint checks

Run: `cargo fmt -- --check && cargo clippy -- -D warnings`
Expected: PASS.

- [ ] Manual smoke run

Run: `cargo run -- --target https://api.anthropic.com --port 8000 --dashboard-port 3000`
Expected: Proxy starts, dashboard loads, `/api/health` returns JSON.

- [ ] Validate dropped modules are removed

Run: `git status --short src/`
Expected: No legacy files remain (`local_context.rs`, `correlation*.rs`, `explainer.rs`, `settings_admin.rs`, `session_admin.rs`, old `stats.rs`).

- [ ] Validate endpoint coverage

Run: `cargo test dashboard::tests -- --nocapture`
Expected: PASS for all v2 endpoint route tests.

---

## Spec Coverage Map

- Proxy-only architecture + dropped modules: Tasks 4, 5, Final Verification
- SQLite + FTS5 schema: Tasks 2, 3
- Full body/tool/token/stream capture: Task 4
- Anomaly engine + hypothesis generation: Task 7
- 140-parameter conformance: Task 8
- Model mapping + auto-tuning: Tasks 6, 11
- Overview report card + 4 detail tabs: Task 10
- REST endpoints + WebSocket: Task 9
- Forward compatibility unknown fields: Task 12
- Phase alignment (Foundation, Analysis, API, Frontend, Polish): Tasks 1–12 in order
