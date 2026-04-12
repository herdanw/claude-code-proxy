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
                if avg > 0.0 {
                    format!("Output was {} tokens, {:.0}% above model average of {:.0}. May indicate verbose responses or insufficient constraints.", tokens, ((tokens as f64 / avg) - 1.0) * 100.0, avg)
                } else {
                    format!("Output was {} tokens, which is unusually high.", tokens)
                }
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
    use crate::types::{RequestStatusKind, Severity};

    fn sample_anomaly(kind: AnomalyKind) -> DetectedAnomaly {
        DetectedAnomaly {
            kind,
            severity: Severity::Warning,
            summary: "test".to_string(),
            hypothesis: None,
        }
    }

    fn sample_request() -> RequestRecord {
        RequestRecord {
            id: "test-1".to_string(), session_id: None, timestamp: chrono::Utc::now(),
            method: "POST".to_string(), path: "/v1/messages".to_string(),
            model: "claude-opus-4-1".to_string(), stream: true,
            status_code: Some(200), status_kind: RequestStatusKind::Success,
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

    #[test]
    fn empty_anomalies_produces_empty_explanations() {
        let req = sample_request();
        let explanations = generate_explanations(&req, &[], &[]);
        assert!(explanations.is_empty());
    }

    #[test]
    fn evidence_includes_model() {
        let req = sample_request();
        let anomalies = vec![sample_anomaly(AnomalyKind::Stall)];
        let explanations = generate_explanations(&req, &anomalies, &[]);
        assert_eq!(explanations[0].evidence_json["model"], "claude-opus-4-1");
    }
}
