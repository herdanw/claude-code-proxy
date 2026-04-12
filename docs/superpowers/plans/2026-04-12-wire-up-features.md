# Wire Up All Scaffolded Features — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete all 16 stub/scaffolded/dead-code items so every dashboard feature is functional end-to-end.

**Architecture:** Expand the analyzer worker (5s tick) to run correlation and explanation engines after anomaly detection. Add `Arc<Store>` to the dashboard Axum state alongside `Arc<StatsStore>` so API handlers can query the v2 SQLite store. Implement all 11 anomaly rules, tool usage extraction from SSE streaming, and build the conformance/anomalies tab frontends.

**Tech Stack:** Rust (axum, rusqlite, serde_json, tokio), vanilla JS (Chart.js), SQLite with FTS5.

**Spec:** `docs/superpowers/specs/2026-04-12-wire-up-features-design.md`

---

## File Structure

| File | Action | Responsibility |
|------|--------|---------------|
| `src/analyzer.rs` | Modify | Add 10 new anomaly detection rules, remove dead `compute_health_score()` |
| `src/explain.rs` | Create | Rule-based explanation generator |
| `src/correlation.rs` | Modify | Expand from enum-only to correlation engine (keep `PayloadPolicy`) |
| `src/main.rs` | Modify | Wire correlation+explanation into analyzer worker, pass `Store` to dashboard |
| `src/dashboard.rs` | Modify | Add `Store` to state, wire 6 stub handlers to real data |
| `src/store.rs` | Modify | Add tool_usage + anomaly-by-id read methods, remove dead code |
| `src/proxy.rs` | Modify | Extract tool_use from SSE content blocks, track unknown fields |
| `src/model_profile.rs` | Modify | Remove dead fingerprint scaffolding |
| `src/types.rs` | Modify | Remove unused conformance types |
| `src/dashboard/tabs/conformance.js` | Modify | Full implementation — model scoreboard |
| `src/dashboard/tabs/anomalies.js` | Modify | Full implementation — anomaly list with badges |

---

### Task 1: Implement All 11 Anomaly Detection Rules

**Files:**
- Modify: `src/analyzer.rs`

This task expands `detect_anomalies()` from 1 rule (SlowTtft) to all 11 `AnomalyKind` variants, using the `recent` parameter for trend-based thresholds.

- [ ] **Step 1: Write tests for the new anomaly rules**

Add to the `#[cfg(test)]` section at the bottom of `src/analyzer.rs`. Create test helper and tests for each rule:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{RequestRecord, RequestStatusKind};
    use chrono::Utc;

    fn sample_request() -> RequestRecord {
        RequestRecord {
            id: "test-1".to_string(),
            session_id: None,
            timestamp: Utc::now(),
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            model: "claude-opus-4-1".to_string(),
            stream: true,
            status_code: Some(200),
            status_kind: RequestStatusKind::Success,
            ttft_ms: Some(500.0),
            duration_ms: Some(3000.0),
            input_tokens: Some(100),
            output_tokens: Some(200),
            cache_read_tokens: Some(50),
            cache_creation_tokens: None,
            thinking_tokens: None,
            request_size_bytes: 1024,
            response_size_bytes: 2048,
            stall_count: 0,
            stall_details_json: "[]".to_string(),
            error_summary: None,
            stop_reason: Some("end_turn".to_string()),
            content_block_types_json: "[]".to_string(),
            anomalies_json: "[]".to_string(),
            analyzed: false,
        }
    }

    fn default_rules() -> AnalyzerRules {
        AnalyzerRules { slow_ttft_threshold_ms: 3000.0, stall_threshold_s: 0.5 }
    }

    #[test]
    fn detects_stall() {
        let mut req = sample_request();
        req.stall_count = 2;
        let anomalies = detect_anomalies(&req, &default_rules(), &[]);
        assert!(anomalies.iter().any(|a| a.kind == AnomalyKind::Stall));
    }

    #[test]
    fn detects_api_error() {
        let mut req = sample_request();
        req.status_code = Some(500);
        req.status_kind = RequestStatusKind::ServerError;
        req.error_summary = Some("Internal Server Error".to_string());
        let anomalies = detect_anomalies(&req, &default_rules(), &[]);
        assert!(anomalies.iter().any(|a| a.kind == AnomalyKind::ApiError));
    }

    #[test]
    fn detects_client_error_not_429() {
        let mut req = sample_request();
        req.status_code = Some(400);
        req.status_kind = RequestStatusKind::ClientError;
        let anomalies = detect_anomalies(&req, &default_rules(), &[]);
        assert!(anomalies.iter().any(|a| a.kind == AnomalyKind::ClientError));
    }

    #[test]
    fn detects_rate_limited() {
        let mut req = sample_request();
        req.status_code = Some(429);
        let anomalies = detect_anomalies(&req, &default_rules(), &[]);
        assert!(anomalies.iter().any(|a| a.kind == AnomalyKind::RateLimited));
    }

    #[test]
    fn detects_overload() {
        let mut req = sample_request();
        req.status_code = Some(529);
        let anomalies = detect_anomalies(&req, &default_rules(), &[]);
        assert!(anomalies.iter().any(|a| a.kind == AnomalyKind::Overload));
    }

    #[test]
    fn detects_max_tokens_hit() {
        let mut req = sample_request();
        req.stop_reason = Some("max_tokens".to_string());
        let anomalies = detect_anomalies(&req, &default_rules(), &[]);
        assert!(anomalies.iter().any(|a| a.kind == AnomalyKind::MaxTokensHit));
    }

    #[test]
    fn detects_interrupted_stream() {
        let mut req = sample_request();
        req.stream = true;
        req.stop_reason = None;
        req.error_summary = Some("connection reset".to_string());
        let anomalies = detect_anomalies(&req, &default_rules(), &[]);
        assert!(anomalies.iter().any(|a| a.kind == AnomalyKind::InterruptedStream));
    }

    #[test]
    fn detects_high_tokens_with_recent_baseline() {
        let mut req = sample_request();
        req.output_tokens = Some(5000);
        // Build 15 recent requests with avg ~200 output tokens
        let recent: Vec<RequestRecord> = (0..15).map(|i| {
            let mut r = sample_request();
            r.id = format!("recent-{i}");
            r.output_tokens = Some(200);
            r
        }).collect();
        let anomalies = detect_anomalies(&req, &default_rules(), &recent);
        assert!(anomalies.iter().any(|a| a.kind == AnomalyKind::HighTokens));
    }

    #[test]
    fn no_high_tokens_without_enough_samples() {
        let mut req = sample_request();
        req.output_tokens = Some(5000);
        let recent: Vec<RequestRecord> = (0..5).map(|i| {
            let mut r = sample_request();
            r.id = format!("recent-{i}");
            r.output_tokens = Some(200);
            r
        }).collect();
        let anomalies = detect_anomalies(&req, &default_rules(), &recent);
        assert!(!anomalies.iter().any(|a| a.kind == AnomalyKind::HighTokens));
    }

    #[test]
    fn detects_cache_miss() {
        let mut req = sample_request();
        req.cache_read_tokens = Some(0);
        // Recent requests have cache hits
        let recent: Vec<RequestRecord> = (0..15).map(|i| {
            let mut r = sample_request();
            r.id = format!("recent-{i}");
            r.cache_read_tokens = Some(500);
            r
        }).collect();
        let anomalies = detect_anomalies(&req, &default_rules(), &recent);
        assert!(anomalies.iter().any(|a| a.kind == AnomalyKind::CacheMiss));
    }

    #[test]
    fn healthy_request_produces_no_anomalies() {
        let req = sample_request();
        let anomalies = detect_anomalies(&req, &default_rules(), &[]);
        assert!(anomalies.is_empty());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib analyzer`
Expected: Most tests FAIL because only `SlowTtft` is implemented.

- [ ] **Step 3: Implement all 11 detection rules**

Replace the `detect_anomalies()` function body in `src/analyzer.rs`:

```rust
pub fn detect_anomalies(
    req: &RequestRecord,
    rules: &AnalyzerRules,
    recent: &[RequestRecord],
) -> Vec<DetectedAnomaly> {
    let mut out = Vec::new();

    // --- SlowTtft (existing) ---
    if let Some(ttft) = req.ttft_ms {
        if ttft > rules.slow_ttft_threshold_ms {
            out.push(DetectedAnomaly {
                kind: AnomalyKind::SlowTtft,
                severity: if ttft > 8000.0 { Severity::Error } else { Severity::Warning },
                summary: format!("TTFT {:.0}ms exceeded threshold {:.0}ms", ttft, rules.slow_ttft_threshold_ms),
                hypothesis: Some(format!(
                    "TTFT {:.0}ms is elevated for model {}. Check context size, recent 429s, and upstream latency spikes.",
                    ttft, req.model
                )),
            });
        }
    }

    // --- Stall ---
    if req.stall_count > 0 {
        out.push(DetectedAnomaly {
            kind: AnomalyKind::Stall,
            severity: Severity::Warning,
            summary: format!("Stream stalled {} time(s)", req.stall_count),
            hypothesis: Some("Stream stalls may indicate network instability or upstream throttling.".to_string()),
        });
    }

    // --- Status-code based rules ---
    if let Some(status) = req.status_code {
        match status {
            429 => {
                out.push(DetectedAnomaly {
                    kind: AnomalyKind::RateLimited,
                    severity: Severity::Warning,
                    summary: "Rate limited (429)".to_string(),
                    hypothesis: Some("API rate limit exceeded. Consider spacing requests or upgrading your plan.".to_string()),
                });
            }
            529 => {
                out.push(DetectedAnomaly {
                    kind: AnomalyKind::Overload,
                    severity: Severity::Error,
                    summary: "API overloaded (529)".to_string(),
                    hypothesis: Some("The upstream API service is temporarily unavailable due to high load.".to_string()),
                });
            }
            s if (500..=599).contains(&s) => {
                out.push(DetectedAnomaly {
                    kind: AnomalyKind::ApiError,
                    severity: Severity::Error,
                    summary: format!("Server error ({}): {}", s, req.error_summary.as_deref().unwrap_or("unknown")),
                    hypothesis: Some("Upstream API returned a server error. This typically indicates service issues.".to_string()),
                });
            }
            s if (400..=499).contains(&s) => {
                out.push(DetectedAnomaly {
                    kind: AnomalyKind::ClientError,
                    severity: Severity::Warning,
                    summary: format!("Client error ({}): {}", s, req.error_summary.as_deref().unwrap_or("unknown")),
                    hypothesis: Some("The request was rejected by the API. Check request format and parameters.".to_string()),
                });
            }
            _ => {}
        }
    }

    // --- MaxTokensHit ---
    if req.stop_reason.as_deref() == Some("max_tokens") {
        out.push(DetectedAnomaly {
            kind: AnomalyKind::MaxTokensHit,
            severity: Severity::Info,
            summary: "Response hit max_tokens limit — output was truncated".to_string(),
            hypothesis: None,
        });
    }

    // --- InterruptedStream ---
    if req.stream && req.stop_reason.is_none() && req.error_summary.is_some() {
        out.push(DetectedAnomaly {
            kind: AnomalyKind::InterruptedStream,
            severity: Severity::Warning,
            summary: format!("Stream interrupted: {}", req.error_summary.as_deref().unwrap_or("unknown")),
            hypothesis: Some("The streaming response was interrupted before completion.".to_string()),
        });
    }

    // --- Trend-based rules (need ≥10 recent samples) ---
    if recent.len() >= 10 {
        // HighTokens: output > 2× model average
        if let Some(output) = req.output_tokens {
            let avg_output = recent.iter()
                .filter_map(|r| r.output_tokens)
                .sum::<u64>() as f64 / recent.len() as f64;
            if avg_output > 0.0 && output as f64 > avg_output * 2.0 {
                out.push(DetectedAnomaly {
                    kind: AnomalyKind::HighTokens,
                    severity: Severity::Info,
                    summary: format!("Output {} tokens is {:.0}% above model average of {:.0}",
                        output, ((output as f64 / avg_output) - 1.0) * 100.0, avg_output),
                    hypothesis: Some("High token output may indicate verbose responses or insufficient constraints.".to_string()),
                });
            }
        }

        // CacheMiss: cache_read = 0 when model avg has cache hits
        if let Some(cache_read) = req.cache_read_tokens {
            let avg_cache = recent.iter()
                .filter_map(|r| r.cache_read_tokens)
                .sum::<u64>() as f64 / recent.len() as f64;
            if cache_read == 0 && avg_cache > 0.0 {
                let pct_with_cache = recent.iter()
                    .filter(|r| r.cache_read_tokens.unwrap_or(0) > 0)
                    .count() as f64 / recent.len() as f64 * 100.0;
                out.push(DetectedAnomaly {
                    kind: AnomalyKind::CacheMiss,
                    severity: Severity::Info,
                    summary: format!("No prompt cache hits. {:.0}% of recent requests had cache hits.", pct_with_cache),
                    hypothesis: None,
                });
            }
        }
    }

    out
}
```

Also remove the `#[allow(dead_code)]` annotation from the `stall_threshold_s` field in `AnalyzerRules`, and remove the `compute_health_score()` function entirely.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib analyzer`
Expected: All tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/analyzer.rs
git commit -m "feat: implement all 11 anomaly detection rules"
```

---

### Task 2: Create Explanation Generator Module

**Files:**
- Create: `src/explain.rs`
- Modify: `src/main.rs` (add `mod explain;`)

- [ ] **Step 1: Create `src/explain.rs` with the full explanation generator**

```rust
use crate::analyzer::DetectedAnomaly;
use crate::types::{AnomalyKind, RequestRecord};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Explanation {
    pub rank: u32,
    pub anomaly_kind: String,
    pub summary: String,
    pub evidence_json: serde_json::Value,
}

pub fn generate_explanations(
    req: &RequestRecord,
    anomalies: &[DetectedAnomaly],
    recent: &[RequestRecord],
) -> Vec<Explanation> {
    let mut explanations = Vec::new();

    for (i, anomaly) in anomalies.iter().enumerate() {
        let evidence = build_evidence(req, &anomaly.kind, recent);
        let summary = match anomaly.kind {
            AnomalyKind::SlowTtft => {
                let ttft = req.ttft_ms.unwrap_or(0.0);
                let avg = compute_avg_ttft(recent);
                if avg > 0.0 {
                    format!("TTFT was {:.0}ms, {:.0}% above model average of {:.0}ms. Common causes: large context window, API congestion, cold model start.", ttft, ((ttft / avg) - 1.0) * 100.0, avg)
                } else {
                    format!("TTFT was {:.0}ms, exceeding the configured threshold.", ttft)
                }
            }
            AnomalyKind::Stall => {
                format!("Stream stalled {} time(s). May indicate network instability or upstream throttling.", req.stall_count)
            }
            AnomalyKind::ApiError => {
                let status = req.status_code.unwrap_or(0);
                let err = req.error_summary.as_deref().unwrap_or("unknown");
                format!("Server returned {}. Error: {}. This typically indicates upstream service issues.", status, err)
            }
            AnomalyKind::RateLimited => {
                "Rate limited (429). You've exceeded the API rate limit. Consider spacing requests or upgrading your plan.".to_string()
            }
            AnomalyKind::Overload => {
                "API overloaded (529). The upstream service is temporarily unavailable.".to_string()
            }
            AnomalyKind::HighTokens => {
                let tokens = req.output_tokens.unwrap_or(0);
                let avg = compute_avg_output_tokens(recent);
                format!("Output was {} tokens, {:.0}% above model average of {:.0}. May indicate verbose responses or insufficient constraints.", tokens, ((tokens as f64 / avg) - 1.0) * 100.0, avg)
            }
            AnomalyKind::MaxTokensHit => {
                "Response hit max_tokens limit. The model's output was truncated.".to_string()
            }
            AnomalyKind::InterruptedStream => {
                let reason = req.error_summary.as_deref().unwrap_or("unknown cause");
                format!("Stream was interrupted before completion: {}.", reason)
            }
            AnomalyKind::CacheMiss => {
                let pct = compute_cache_hit_pct(recent);
                format!("No prompt cache hits. {:.0}% of recent requests for this model had cache hits.", pct)
            }
            AnomalyKind::Timeout => {
                let dur = req.duration_ms.unwrap_or(0.0);
                format!("Request took {:.0}ms with no successful response.", dur)
            }
            AnomalyKind::ClientError => {
                let status = req.status_code.unwrap_or(0);
                let err = req.error_summary.as_deref().unwrap_or("unknown");
                format!("Client error {}: {}.", status, err)
            }
        };

        explanations.push(Explanation {
            rank: (i + 1) as u32,
            anomaly_kind: format!("{:?}", anomaly.kind),
            summary,
            evidence_json: evidence,
        });
    }

    explanations
}

fn build_evidence(req: &RequestRecord, kind: &AnomalyKind, recent: &[RequestRecord]) -> serde_json::Value {
    let mut ev = serde_json::Map::new();
    ev.insert("model".to_string(), serde_json::json!(req.model));

    match kind {
        AnomalyKind::SlowTtft => {
            ev.insert("ttft_ms".to_string(), serde_json::json!(req.ttft_ms));
            ev.insert("avg_ttft_ms".to_string(), serde_json::json!(compute_avg_ttft(recent)));
        }
        AnomalyKind::HighTokens => {
            ev.insert("output_tokens".to_string(), serde_json::json!(req.output_tokens));
            ev.insert("avg_output_tokens".to_string(), serde_json::json!(compute_avg_output_tokens(recent)));
        }
        AnomalyKind::Stall => {
            ev.insert("stall_count".to_string(), serde_json::json!(req.stall_count));
        }
        _ => {
            ev.insert("status_code".to_string(), serde_json::json!(req.status_code));
            if let Some(ref err) = req.error_summary {
                ev.insert("error".to_string(), serde_json::json!(err));
            }
        }
    }

    serde_json::Value::Object(ev)
}

fn compute_avg_ttft(recent: &[RequestRecord]) -> f64 {
    let vals: Vec<f64> = recent.iter().filter_map(|r| r.ttft_ms).collect();
    if vals.is_empty() { return 0.0; }
    vals.iter().sum::<f64>() / vals.len() as f64
}

fn compute_avg_output_tokens(recent: &[RequestRecord]) -> f64 {
    let vals: Vec<u64> = recent.iter().filter_map(|r| r.output_tokens).collect();
    if vals.is_empty() { return 0.0; }
    vals.iter().sum::<u64>() as f64 / vals.len() as f64
}

fn compute_cache_hit_pct(recent: &[RequestRecord]) -> f64 {
    if recent.is_empty() { return 0.0; }
    let with_cache = recent.iter().filter(|r| r.cache_read_tokens.unwrap_or(0) > 0).count();
    with_cache as f64 / recent.len() as f64 * 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Severity;

    fn sample_anomaly(kind: AnomalyKind) -> DetectedAnomaly {
        DetectedAnomaly {
            kind,
            severity: Severity::Warning,
            summary: "test".to_string(),
            hypothesis: None,
        }
    }

    fn sample_request() -> RequestRecord {
        // Same as analyzer::tests::sample_request()
        RequestRecord {
            id: "test-1".to_string(), session_id: None, timestamp: chrono::Utc::now(),
            method: "POST".to_string(), path: "/v1/messages".to_string(),
            model: "claude-opus-4-1".to_string(), stream: true,
            status_code: Some(200), status_kind: crate::types::RequestStatusKind::Success,
            ttft_ms: Some(4200.0), duration_ms: Some(3000.0),
            input_tokens: Some(100), output_tokens: Some(200),
            cache_read_tokens: Some(50), cache_creation_tokens: None, thinking_tokens: None,
            request_size_bytes: 1024, response_size_bytes: 2048,
            stall_count: 0, stall_details_json: "[]".to_string(),
            error_summary: None, stop_reason: Some("end_turn".to_string()),
            content_block_types_json: "[]".to_string(),
            anomalies_json: "[]".to_string(), analyzed: false,
        }
    }

    #[test]
    fn generates_explanation_for_each_anomaly() {
        let req = sample_request();
        let anomalies = vec![sample_anomaly(AnomalyKind::SlowTtft), sample_anomaly(AnomalyKind::Stall)];
        let explanations = generate_explanations(&req, &anomalies, &[]);
        assert_eq!(explanations.len(), 2);
        assert_eq!(explanations[0].rank, 1);
        assert_eq!(explanations[1].rank, 2);
    }

    #[test]
    fn rate_limited_explanation_is_actionable() {
        let mut req = sample_request();
        req.status_code = Some(429);
        let anomalies = vec![sample_anomaly(AnomalyKind::RateLimited)];
        let explanations = generate_explanations(&req, &anomalies, &[]);
        assert!(explanations[0].summary.contains("429"));
    }
}
```

- [ ] **Step 2: Add `mod explain;` to `src/main.rs`**

Add after the other `mod` declarations at the top of `src/main.rs`:

```rust
mod explain;
```

- [ ] **Step 3: Run tests**

Run: `cargo test --lib explain`
Expected: All tests PASS.

- [ ] **Step 4: Commit**

```bash
git add src/explain.rs src/main.rs
git commit -m "feat: add rule-based explanation generator module"
```

---

### Task 3: Expand Correlation Engine

**Files:**
- Modify: `src/correlation.rs`

- [ ] **Step 1: Expand `src/correlation.rs` with the correlation engine**

Keep the existing `PayloadPolicy` enum. Add correlation logic after it:

```rust
use crate::types::RequestRecord;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CorrelationLink {
    pub link_type: String,
    pub confidence: f64,
    pub local_event_id: Option<String>,
    pub reason: String,
}

/// Generate correlation links for a request by matching against local events.
/// `events` are local_events from the stats store (timestamp-matched).
pub fn find_correlations(
    req: &RequestRecord,
    events: &[(String, i64, Option<String>, String)], // (event_id, event_time_ms, session_hint, event_kind)
) -> Vec<CorrelationLink> {
    let mut links = Vec::new();
    let req_time_ms = req.timestamp.timestamp_millis();

    for (event_id, event_time_ms, session_hint, event_kind) in events {
        let time_diff_ms = (req_time_ms - event_time_ms).unsigned_abs();

        // Rule 1: Temporal match — event within ±5 seconds of request
        if time_diff_ms <= 5000 {
            let confidence = 1.0 - (time_diff_ms as f64 / 5000.0) * 0.5; // 1.0 at 0ms, 0.5 at 5s
            links.push(CorrelationLink {
                link_type: "temporal".to_string(),
                confidence,
                local_event_id: Some(event_id.clone()),
                reason: format!("Event occurred {:.1}s from request", time_diff_ms as f64 / 1000.0),
            });
        }

        // Rule 2: Session match
        if let (Some(req_session), Some(event_session)) = (&req.session_id, session_hint) {
            if req_session == event_session {
                links.push(CorrelationLink {
                    link_type: "session".to_string(),
                    confidence: 0.9,
                    local_event_id: Some(event_id.clone()),
                    reason: format!("Same session: {}", req_session),
                });
            }
        }

        // Rule 3: Config drift — settings change near request time
        if event_kind == "config_change" && time_diff_ms <= 60_000 {
            links.push(CorrelationLink {
                link_type: "config_drift".to_string(),
                confidence: 0.7,
                local_event_id: Some(event_id.clone()),
                reason: format!("Settings changed {:.0}s before/after request", time_diff_ms as f64 / 1000.0),
            });
        }
    }

    // Dedup by event_id, keeping highest confidence
    links.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));
    let mut seen = std::collections::HashSet::new();
    links.retain(|l| {
        if let Some(ref id) = l.local_event_id {
            seen.insert(id.clone())
        } else {
            true
        }
    });

    links
}

#[cfg(test)]
mod correlation_tests {
    use super::*;
    use crate::types::{RequestStatusKind};
    use chrono::Utc;

    fn sample_request() -> RequestRecord {
        RequestRecord {
            id: "req-1".to_string(), session_id: Some("sess-1".to_string()),
            timestamp: Utc::now(), method: "POST".to_string(),
            path: "/v1/messages".to_string(), model: "claude-opus-4-1".to_string(),
            stream: true, status_code: Some(200), status_kind: RequestStatusKind::Success,
            ttft_ms: Some(500.0), duration_ms: Some(3000.0),
            input_tokens: Some(100), output_tokens: Some(200),
            cache_read_tokens: None, cache_creation_tokens: None, thinking_tokens: None,
            request_size_bytes: 1024, response_size_bytes: 2048,
            stall_count: 0, stall_details_json: "[]".to_string(),
            error_summary: None, stop_reason: Some("end_turn".to_string()),
            content_block_types_json: "[]".to_string(),
            anomalies_json: "[]".to_string(), analyzed: false,
        }
    }

    #[test]
    fn temporal_match_within_5s() {
        let req = sample_request();
        let req_ms = req.timestamp.timestamp_millis();
        let events = vec![("evt-1".to_string(), req_ms + 2000, None, "tool_use".to_string())];
        let links = find_correlations(&req, &events);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].link_type, "temporal");
        assert!(links[0].confidence > 0.5);
    }

    #[test]
    fn no_match_beyond_5s() {
        let req = sample_request();
        let req_ms = req.timestamp.timestamp_millis();
        let events = vec![("evt-1".to_string(), req_ms + 10000, None, "tool_use".to_string())];
        let links = find_correlations(&req, &events);
        assert!(links.is_empty());
    }

    #[test]
    fn session_match() {
        let req = sample_request();
        let req_ms = req.timestamp.timestamp_millis();
        let events = vec![("evt-1".to_string(), req_ms, Some("sess-1".to_string()), "tool_use".to_string())];
        let links = find_correlations(&req, &events);
        assert!(links.iter().any(|l| l.link_type == "session"));
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib correlation`
Expected: All tests PASS.

- [ ] **Step 3: Commit**

```bash
git add src/correlation.rs
git commit -m "feat: expand correlation engine with temporal, session, and config-drift rules"
```

---

### Task 4: Add Store Methods for Tool Usage and Anomaly Lookup

**Files:**
- Modify: `src/store.rs`

- [ ] **Step 1: Add `get_tool_usage_for_request()` and `get_anomaly_by_id()` methods**

Add these public methods to the `impl Store` block in `src/store.rs`:

```rust
pub fn get_tool_usage_for_request(&self, request_id: &str) -> Result<Vec<serde_json::Value>, rusqlite::Error> {
    let conn = self.conn.lock();
    let mut stmt = conn.prepare(
        "SELECT id, tool_name, tool_input_json, success, is_error FROM tool_usage WHERE request_id = ?1"
    )?;
    let rows = stmt.query_map(rusqlite::params![request_id], |row| {
        Ok(serde_json::json!({
            "id": row.get::<_, String>(0)?,
            "tool_name": row.get::<_, String>(1)?,
            "tool_input": row.get::<_, Option<String>>(2)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
            "success": row.get::<_, Option<bool>>(3)?,
            "is_error": row.get::<_, Option<bool>>(4)?,
        }))
    })?;
    rows.collect()
}

pub fn insert_tool_usage(&self, request_id: &str, tool_name: &str, tool_input_json: &str) -> Result<(), rusqlite::Error> {
    let conn = self.conn.lock();
    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO tool_usage (id, request_id, tool_name, tool_input_json) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![id, request_id, tool_name, tool_input_json],
    )?;
    Ok(())
}

pub fn get_anomaly_by_id(&self, anomaly_id: &str) -> Result<Option<serde_json::Value>, rusqlite::Error> {
    let conn = self.conn.lock();
    let mut stmt = conn.prepare(
        "SELECT id, request_id, kind, severity, summary, hypothesis, evidence_json, created_at_ms FROM anomalies WHERE id = ?1"
    )?;
    let result = stmt.query_row(rusqlite::params![anomaly_id], |row| {
        Ok(serde_json::json!({
            "id": row.get::<_, String>(0)?,
            "request_id": row.get::<_, String>(1)?,
            "kind": row.get::<_, String>(2)?,
            "severity": row.get::<_, String>(3)?,
            "summary": row.get::<_, String>(4)?,
            "hypothesis": row.get::<_, Option<String>>(5)?,
            "evidence": row.get::<_, Option<String>>(6)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
            "detected_at_ms": row.get::<_, i64>(7)?,
        }))
    });
    match result {
        Ok(val) => Ok(Some(val)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn list_all_model_stats(&self) -> Result<Vec<serde_json::Value>, rusqlite::Error> {
    let conn = self.conn.lock();
    let mut stmt = conn.prepare(
        "SELECT model, COUNT(*) as count, \
         AVG(CASE WHEN ttft_ms IS NOT NULL THEN ttft_ms END) as avg_ttft, \
         SUM(CASE WHEN status_code >= 400 THEN 1 ELSE 0 END) as error_count, \
         MAX(timestamp_ms) as last_seen \
         FROM requests GROUP BY model ORDER BY count DESC"
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(serde_json::json!({
            "model": row.get::<_, String>(0)?,
            "request_count": row.get::<_, i64>(1)?,
            "avg_ttft_ms": row.get::<_, Option<f64>>(2)?,
            "error_count": row.get::<_, i64>(3)?,
            "last_seen_ms": row.get::<_, Option<i64>>(4)?,
        }))
    })?;
    rows.collect()
}

/// Get local events within a time window (for correlation engine)
pub fn get_local_events_near(&self, timestamp_ms: i64, window_ms: i64) -> Result<Vec<(String, i64, Option<String>, String)>, rusqlite::Error> {
    let conn = self.conn.lock();
    let mut stmt = conn.prepare(
        "SELECT id, event_time_ms, session_hint, event_kind FROM local_events \
         WHERE event_time_ms BETWEEN ?1 AND ?2 ORDER BY event_time_ms"
    )?;
    let rows = stmt.query_map(
        rusqlite::params![timestamp_ms - window_ms, timestamp_ms + window_ms],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    )?;
    rows.collect()
}
```

Note: `get_local_events_near` queries `local_events` from the stats DB (proxy.db), but `Store` operates on proxy-v2.db. The local_events table is in `StatsStore`. Adjust accordingly — if `local_events` is in the stats store, this method should go on `StatsStore` instead, or the correlation engine should receive events from `StatsStore`. Check which DB has the `local_events` table and adjust the implementation.

- [ ] **Step 2: Also remove dead code from `store.rs`**

Remove `increment_model_sample_count()` (redundant with `persist_analyzed_request()`).

Remove the `sessions` table creation from `initialize_schema()` (dead schema — stats.rs handles sessions).

- [ ] **Step 3: Run tests**

Run: `cargo test --lib store`
Expected: All tests PASS.

- [ ] **Step 4: Commit**

```bash
git add src/store.rs
git commit -m "feat: add tool_usage, anomaly lookup, and model stats store methods; remove dead code"
```

---

### Task 5: Add Store to Dashboard State and Wire Stub Handlers

**Files:**
- Modify: `src/dashboard.rs`
- Modify: `src/main.rs`

This is the key wiring task — changes the dashboard from `Arc<StatsStore>` only to a combined state with both `Arc<StatsStore>` and `Arc<Store>`.

- [ ] **Step 1: Create a DashboardState struct in `src/dashboard.rs`**

Add near the top of `src/dashboard.rs` (after imports):

```rust
#[derive(Clone)]
struct DashboardState {
    stats: Arc<StatsStore>,
    store: Arc<Store>,
}
```

- [ ] **Step 2: Update `build_dashboard_app` to accept both stores**

Change the signature and state:

```rust
fn build_dashboard_app(stats: Arc<StatsStore>, store: Arc<Store>) -> Router {
    let state = DashboardState { stats, store };
    Router::new()
        // ... all routes unchanged ...
        .with_state(state)
}
```

Update `run_dashboard` to accept both:

```rust
pub async fn run_dashboard(stats: Arc<StatsStore>, store: Arc<Store>, port: u16) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let app = build_dashboard_app(stats, store);
    // ... rest unchanged
}
```

- [ ] **Step 3: Update ALL handler signatures**

Change every `State(store): State<Arc<StatsStore>>` to `State(state): State<DashboardState>` and use `state.stats` where `store` was used before. For handlers that need the v2 store, use `state.store`.

This is a mechanical find-and-replace. Every handler that currently has `State(store): State<Arc<StatsStore>>` becomes `State(state): State<DashboardState>`, and every reference to `store.` becomes `state.stats.`.

- [ ] **Step 4: Wire the 6 stub handlers to real data**

**`api_request_tools`** — add `State(state)` parameter, call store:
```rust
async fn api_request_tools(Path(id): Path<String>, State(state): State<DashboardState>) -> impl IntoResponse {
    match state.store.get_tool_usage_for_request(&id) {
        Ok(tools) => Json(serde_json::json!(tools)),
        Err(_) => Json(serde_json::json!([])),
    }
}
```

**`api_model_profile`** — query observed stats:
```rust
async fn api_model_profile(Path(name): Path<String>, State(state): State<DashboardState>) -> impl IntoResponse {
    let sample_count = state.store.get_model_profile_sample_count(&name).ok().flatten().unwrap_or(0);
    let observed = state.store.get_model_profile_observed(&name).ok().flatten();
    Json(serde_json::json!({
        "model": name,
        "sample_count": sample_count,
        "observed": observed,
    }))
}
```

**`api_model_comparison`** — return observed vs global:
```rust
async fn api_model_comparison(Path(name): Path<String>, State(state): State<DashboardState>) -> impl IntoResponse {
    let observed = state.store.get_model_profile_observed(&name).ok().flatten();
    Json(serde_json::json!({
        "model": name,
        "observed": observed,
    }))
}
```

**`api_model_config`** — return actual model list:
```rust
async fn api_model_config(State(state): State<DashboardState>) -> impl IntoResponse {
    match state.store.list_all_model_stats() {
        Ok(models) => Json(serde_json::json!({"models": models})),
        Err(_) => Json(serde_json::json!({"models": []})),
    }
}
```

**`api_anomaly_detail`** — query by ID:
```rust
async fn api_anomaly_detail(Path(id): Path<String>, State(state): State<DashboardState>) -> impl IntoResponse {
    match state.store.get_anomaly_by_id(&id) {
        Ok(Some(anomaly)) => (StatusCode::OK, Json(anomaly)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": format!("anomaly {id} not found")}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    }
}
```

- [ ] **Step 5: Update `main.rs` to pass Store to dashboard**

In `main.rs`, the `Store` is already created for the analyzer worker. Share it with the dashboard:

```rust
// Before the dashboard spawn, create the store earlier and share it:
let v2_store = match Store::new(&data_dir.join("proxy-v2.db")) {
    Ok(s) => Arc::new(s),
    Err(err) => {
        eprintln!("V2 store initialization failed: {err}");
        write_log(&format!("V2 store initialization failed: {err}"));
        std::process::exit(1);
    }
};

// Analyzer worker uses v2_store.clone()
let analyzer_store = v2_store.clone();
// ...

// Dashboard uses both:
let store_dash = store.clone();
let v2_dash = v2_store.clone();
tokio::spawn(async move {
    if let Err(err) = dashboard::run_dashboard(store_dash, v2_dash, dash_port).await {
        eprintln!("Dashboard startup failed: {err}");
    }
});
```

- [ ] **Step 6: Run tests**

Run: `cargo test`
Expected: All tests PASS (may need to update test helpers that call `build_dashboard_app`).

- [ ] **Step 7: Commit**

```bash
git add src/dashboard.rs src/main.rs
git commit -m "feat: add Store to dashboard state and wire all stub API handlers"
```

---

### Task 6: Wire Correlation + Explanation into Analyzer Worker

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Expand `run_analyzer_tick_with_rules` to run correlation and explanation engines**

After anomaly detection and persistence, add correlation and explanation generation:

```rust
async fn run_analyzer_tick_with_rules(
    store: Arc<Store>,
    stats_store: Option<Arc<StatsStore>>,  // needed for local_events
    rules: &AnalyzerRules,
) -> Result<(), rusqlite::Error> {
    let pending = store.list_unanalyzed_requests(200)?;

    for req in pending {
        let recent = store.list_recent_requests_for_model(&req.model, 50)?;
        let anomalies = analyzer::detect_anomalies(&req, rules, &recent);
        let sample_count = store.persist_analyzed_request(&req.id, &req.model, &anomalies)?;

        if model_profile::should_auto_tune(sample_count) {
            let observed = store.compute_model_observed_stats(&req.model)?;
            store.upsert_model_observed(&req.model, &observed)?;
        }

        // Generate explanations for requests with anomalies
        if !anomalies.is_empty() {
            let explanations = explain::generate_explanations(&req, &anomalies, &recent);
            let _ = store.replace_explanations_for_request(&req.id, &explanations);
        }

        // Run correlation engine if stats_store available (for local_events)
        if let Some(ref ss) = stats_store {
            let req_time_ms = req.timestamp.timestamp_millis();
            let events = ss.get_local_events_near(req_time_ms, 5000);
            if let Ok(events) = events {
                let event_tuples: Vec<_> = events.iter().map(|e| {
                    (e.id.clone(), e.event_time_ms, e.session_hint.clone(), e.event_kind.clone())
                }).collect();
                let links = correlation::find_correlations(&req, &event_tuples);
                let _ = ss.replace_correlations_for_request(&req.id, &links);
            }
        }
    }

    Ok(())
}
```

Note: The exact field names on local event structs and the `replace_explanations_for_request`/`replace_correlations_for_request` signatures need to match what already exists in stats.rs. Check the existing method signatures and adapt the call sites accordingly. The `replace_*` methods already exist but may need their `#[cfg(test)]` gate removed.

- [ ] **Step 2: Pass `stats_store` clone into the analyzer worker spawn**

Update the tokio::spawn block to include the stats store:

```rust
let analyzer_store = v2_store.clone();
let stats_for_worker = store.clone(); // the StatsStore
let worker_rules = analyzer_rules.clone();
tokio::spawn(async move {
    let mut ticker = interval(Duration::from_secs(5));
    loop {
        ticker.tick().await;
        if let Err(err) = run_analyzer_tick_with_rules(
            analyzer_store.clone(),
            Some(stats_for_worker.clone()),
            &worker_rules,
        ).await {
            eprintln!("Analyzer tick failed: {err}");
        }
    }
});
```

- [ ] **Step 3: Update `run_analyzer_tick` (the public test helper) signature**

```rust
pub async fn run_analyzer_tick(store: Arc<Store>) -> Result<(), rusqlite::Error> {
    let rules = AnalyzerRules { slow_ttft_threshold_ms: 3000.0, stall_threshold_s: 0.5 };
    run_analyzer_tick_with_rules(store, None, &rules).await
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: All tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat: wire correlation and explanation engines into analyzer worker"
```

---

### Task 7: Build Conformance Tab Frontend

**Files:**
- Modify: `src/dashboard/tabs/conformance.js`

- [ ] **Step 1: Implement the conformance tab**

Replace the placeholder content with:

```javascript
// Conformance tab — model scoreboard

function initConformanceTab() {
  loadConformanceData();
}

async function loadConformanceData() {
  const scoreboard = document.getElementById('conformance-scoreboard');
  const notes = document.getElementById('conformance-notes');

  try {
    const res = await fetch('/api/model-config');
    const data = await res.json();
    const models = data.models || [];

    if (models.length === 0) {
      scoreboard.innerHTML = '<div class="empty-state" style="padding:20px 12px"><h3 style="font-size:14px">No model data yet</h3><p>Model statistics appear after requests are proxied and analyzed</p></div>';
      return;
    }

    let html = '<table><thead><tr><th>Model</th><th>Requests</th><th>Avg TTFT</th><th>Errors</th><th>Error Rate</th><th>Status</th></tr></thead><tbody>';

    for (const m of models) {
      const errorRate = m.request_count > 0 ? ((m.error_count / m.request_count) * 100).toFixed(1) : '0.0';
      const avgTtft = m.avg_ttft_ms != null ? m.avg_ttft_ms.toFixed(0) + 'ms' : '—';
      const lastSeen = m.last_seen_ms ? new Date(m.last_seen_ms).toLocaleTimeString() : '—';

      // Fetch profile for sample count
      let sampleCount = 0;
      let profileStatus = 'collecting';
      try {
        const profileRes = await fetch('/api/models/' + encodeURIComponent(m.model) + '/profile');
        const profile = await profileRes.json();
        sampleCount = profile.sample_count || 0;
        if (sampleCount >= 50) profileStatus = 'profiled';
      } catch (e) { /* ignore */ }

      const statusBadge = profileStatus === 'profiled'
        ? '<span style="color:var(--green);font-size:11px">● Profiled</span>'
        : '<span style="color:var(--yellow);font-size:11px">● Collecting (' + sampleCount + '/50)</span>';

      const errorRateColor = parseFloat(errorRate) > 10 ? 'var(--red)' : parseFloat(errorRate) > 5 ? 'var(--yellow)' : 'var(--green)';

      html += '<tr>';
      html += '<td style="font-family:var(--mono);font-size:12px">' + esc(m.model) + '</td>';
      html += '<td>' + m.request_count + '</td>';
      html += '<td>' + avgTtft + '</td>';
      html += '<td>' + m.error_count + '</td>';
      html += '<td style="color:' + errorRateColor + '">' + errorRate + '%</td>';
      html += '<td>' + statusBadge + '</td>';
      html += '</tr>';
    }

    html += '</tbody></table>';
    scoreboard.innerHTML = html;

    // Update notes
    const totalRequests = models.reduce((s, m) => s + m.request_count, 0);
    const totalErrors = models.reduce((s, m) => s + m.error_count, 0);
    const overallErrorRate = totalRequests > 0 ? ((totalErrors / totalRequests) * 100).toFixed(1) : '0.0';
    notes.innerHTML = '<div style="font-size:12px;color:var(--text-2);line-height:1.6">'
      + '<p><strong>' + models.length + '</strong> model(s) observed across <strong>' + totalRequests + '</strong> requests.</p>'
      + '<p>Overall error rate: <strong>' + overallErrorRate + '%</strong></p>'
      + '<p style="margin-top:8px;font-size:11px">Models are automatically profiled after 50 analyzed requests. Profiled models show observed TTFT averages and error rates.</p>'
      + '</div>';

  } catch (err) {
    scoreboard.innerHTML = '<div class="empty-state" style="padding:20px 12px"><h3 style="font-size:14px">Failed to load</h3><p>' + esc(err.message) + '</p></div>';
  }
}
```

- [ ] **Step 2: Wire `initConformanceTab` into `app.js`**

In `src/dashboard/app.js`, find where tabs are initialized (the DOMContentLoaded handler). Add a call to `initConformanceTab()` when the conformance tab is activated. Look for the tab switching logic and add:

```javascript
// In the tab click handler, after switching active tab:
if (tabName === 'conformance') initConformanceTab();
```

- [ ] **Step 3: Verify in browser**

Navigate to the dashboard, click "Model Conformance" tab. Should show model statistics table.

- [ ] **Step 4: Commit**

```bash
git add src/dashboard/tabs/conformance.js src/dashboard/app.js
git commit -m "feat: implement conformance tab with model scoreboard"
```

---

### Task 8: Build Anomalies Tab Frontend

**Files:**
- Modify: `src/dashboard/tabs/anomalies.js`

- [ ] **Step 1: Implement the anomalies tab**

Replace the single constant with full rendering logic:

```javascript
const EMPTY_ANOMALIES_HTML = '<div class="empty-state"><h3>No anomalies detected</h3><p>Issues will appear here in real-time</p></div>';

function renderAnomalies() {
  const container = document.getElementById('anomaly-list');
  if (!statsSnapshot || !statsSnapshot.stats || !statsSnapshot.stats.anomalies || statsSnapshot.stats.anomalies.length === 0) {
    container.innerHTML = EMPTY_ANOMALIES_HTML;
    return;
  }

  const anomalies = statsSnapshot.stats.anomalies;
  let html = '';

  for (const a of anomalies) {
    const severityClass = a.severity === 'error' || a.severity === 'critical' ? 'red'
      : a.severity === 'warning' ? 'yellow' : 'blue';
    const severityColor = 'var(--' + severityClass + ')';
    const time = a.detected_at ? new Date(a.detected_at).toLocaleTimeString() : '';
    const kind = a.kind || 'unknown';

    html += '<div class="card" style="margin-bottom:8px;padding:12px;cursor:pointer" onclick="focusAnomalyRequest(\'' + esc(a.request_id || '') + '\')">';
    html += '<div style="display:flex;align-items:center;gap:8px;margin-bottom:6px">';
    html += '<span style="display:inline-block;padding:2px 8px;border-radius:4px;font-size:10px;font-weight:600;text-transform:uppercase;letter-spacing:0.05em;background:' + severityColor + '20;color:' + severityColor + ';border:1px solid ' + severityColor + '40">' + esc(a.severity || 'info') + '</span>';
    html += '<span style="font-family:var(--mono);font-size:11px;color:var(--text-2)">' + esc(kind) + '</span>';
    html += '<span style="margin-left:auto;font-size:11px;color:var(--text-2)">' + esc(time) + '</span>';
    html += '</div>';
    html += '<div style="font-size:13px;color:var(--text-1)">' + esc(a.summary || '') + '</div>';
    if (a.hypothesis) {
      html += '<div style="font-size:11px;color:var(--text-2);margin-top:4px">' + esc(a.hypothesis) + '</div>';
    }
    html += '</div>';
  }

  container.innerHTML = html;
}

function focusAnomalyRequest(requestId) {
  if (!requestId) return;
  // Switch to requests tab and filter by this request
  document.querySelector('[data-tab="requests"]').click();
  const searchInput = document.getElementById('search-input');
  if (searchInput) {
    searchInput.value = requestId;
    searchInput.dispatchEvent(new Event('input'));
  }
}
```

- [ ] **Step 2: Wire `renderAnomalies` into the WebSocket update cycle**

In `src/dashboard/tabs/overview.js` or wherever `updateOverview()` is called after a WebSocket message, add a call to `renderAnomalies()` so the anomalies tab updates in real-time. Find the function that processes incoming WebSocket stats snapshots and add:

```javascript
if (typeof renderAnomalies === 'function') renderAnomalies();
```

- [ ] **Step 3: Commit**

```bash
git add src/dashboard/tabs/anomalies.js src/dashboard/tabs/overview.js
git commit -m "feat: implement anomalies tab with severity badges and click-to-focus"
```

---

### Task 9: Extract Tool Usage from SSE Streaming

**Files:**
- Modify: `src/proxy.rs`

- [ ] **Step 1: Add tool_use extraction to SSE processing**

In the `process_sse_line` function in `src/proxy.rs`, after extracting `usage` and `stop_reason`, add content_block_start detection:

```rust
// Add a new parameter to track tool uses:
fn process_sse_line(
    line: &str,
    usage: &mut UsageData,
    stop_reason: &mut Option<String>,
    tool_uses: &mut Vec<(String, String)>,  // (tool_name, tool_input_json)
) {
    if !line.starts_with("data: ") { return; }
    if let Ok(data) = serde_json::from_str::<serde_json::Value>(&line[6..]) {
        // Existing usage extraction...

        // Tool use extraction from content_block_start
        if let Some(cb) = data.get("content_block") {
            if cb.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                let name = cb.get("name").and_then(|n| n.as_str()).unwrap_or("unknown").to_string();
                let input = cb.get("input").map(|v| v.to_string()).unwrap_or_else(|| "{}".to_string());
                tool_uses.push((name, input));
            }
        }
    }
}
```

Then in the streaming task, pass a `tool_uses` vec and write to store after stream completes:

```rust
let mut tool_uses = Vec::new();
// ... pass &mut tool_uses to process_sse_line calls ...
// After stream completes, persist tool uses:
for (tool_name, tool_input) in &tool_uses {
    let _ = v2_store.insert_tool_usage(&entry_id, tool_name, tool_input);
}
```

Note: The proxy streaming task currently doesn't have access to the v2 `Store`. You'll need to pass an `Arc<Store>` into the proxy state or into the streaming task. Check the `ProxyState` struct and add `store: Arc<Store>` alongside the existing `store: Arc<StatsStore>`.

- [ ] **Step 2: Enable unknown field tracking in live proxy**

Change the `process_sse_text_chunk` call in the live streaming path to pass `Some(&mut unknown_stats)` instead of using the variant that passes `None`. Then persist the unknown stats to the request entry.

- [ ] **Step 3: Run tests**

Run: `cargo test`
Expected: All tests PASS.

- [ ] **Step 4: Commit**

```bash
git add src/proxy.rs src/main.rs
git commit -m "feat: extract tool_use from SSE and enable unknown field tracking"
```

---

### Task 10: Dead Code Cleanup

**Files:**
- Modify: `src/model_profile.rs`
- Modify: `src/types.rs`
- Modify: `src/store.rs`

- [ ] **Step 1: Remove dead code from `model_profile.rs`**

Remove `fingerprint_parameter_names()` function entirely (140 params, never used).
Keep: `ModelConfig`, `resolve_behavior_class()`, `should_auto_tune()`.
Remove `#[allow(dead_code)]` from the module declaration in `main.rs` if all remaining items are now used.

- [ ] **Step 2: Remove unused types from `types.rs`**

Remove: `ModelProfileAssignment`, `CategoryConformanceScore`, `ModelConformanceSummary`.
Remove `#[allow(dead_code)]` from the module declaration in `main.rs` if appropriate.

- [ ] **Step 3: Run tests**

Run: `cargo test`
Expected: All tests PASS (no test uses these removed items).

- [ ] **Step 4: Run `cargo fmt`**

Run: `cargo fmt`

- [ ] **Step 5: Commit**

```bash
git add src/model_profile.rs src/types.rs src/store.rs src/main.rs
git commit -m "chore: remove dead code — unused types, fingerprint scaffolding, redundant methods"
```

---

### Task 11: Update Documentation

**Files:**
- Modify: `ARCHITECTURE.md`
- Modify: `README.md`

- [ ] **Step 1: Update ARCHITECTURE.md**

Update the source modules table with new line counts. Add `explain.rs` module. Update the analyzer worker description to include correlation and explanation passes. Update the dashboard architecture section to note the dual-store state. Remove mention of Alpine.js if still present.

- [ ] **Step 2: Update README.md**

Update the dashboard description to reflect that all 5 tabs are now functional. Add a note about the correlation and explanation features.

- [ ] **Step 3: Commit**

```bash
git add ARCHITECTURE.md README.md
git commit -m "docs: update architecture and readme for fully-wired features"
```

---

## Verification Checklist

After all tasks are complete:

1. `cargo test` — all tests pass (existing + new)
2. `cargo build --release` — clean build, no warnings
3. Run proxy: `claude-proxy.exe --target https://api.anthropic.com --auto-configure --open-browser`
4. Make API requests through Claude Code
5. Verify **Overview tab** — health score, stat cards, charts working
6. Verify **Requests tab** — table populated, click a request → modal shows tool usage, correlation links, explanations
7. Verify **Model Conformance tab** — model scoreboard shows models with stats, sample counts
8. Verify **Anomalies tab** — anomaly cards with severity badges appear for problematic requests, click-to-focus works
9. Verify **Sessions tab** — sessions listed and drillable
10. Browser console — zero JS errors
