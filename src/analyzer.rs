use crate::types::{AnomalyKind, RequestRecord, Severity};

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

pub fn detect_anomalies(
    req: &RequestRecord,
    rules: &AnalyzerRules,
    _recent: &[RequestRecord],
) -> Vec<DetectedAnomaly> {
    let mut out = Vec::new();

    if let Some(ttft) = req.ttft_ms {
        if ttft > rules.slow_ttft_threshold_ms {
            out.push(DetectedAnomaly {
                kind: AnomalyKind::SlowTtft,
                severity: if ttft > 8000.0 {
                    Severity::Error
                } else {
                    Severity::Warning
                },
                summary: format!(
                    "TTFT {:.0}ms exceeded threshold {:.0}ms",
                    ttft, rules.slow_ttft_threshold_ms
                ),
                hypothesis: Some(format!(
                    "TTFT {:.0}ms is elevated for model {}. Check context size, recent 429s, and upstream latency spikes.",
                    ttft, req.model
                )),
            });
        }
    }

    out
}

pub fn compute_health_score(
    error_count: usize,
    warning_count: usize,
    info_count: usize,
    critical_count: usize,
) -> i32 {
    (100 - (error_count as i32 * 5)
        - (warning_count as i32 * 2)
        - (info_count as i32)
        - (critical_count as i32 * 10))
        .max(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AnomalyKind, RequestRecord, RequestStatusKind};
    use chrono::Utc;

    fn sample_request_with_ttft(id: &str, model: &str, ttft_ms: f64) -> RequestRecord {
        RequestRecord {
            id: id.to_string(),
            session_id: None,
            timestamp: Utc::now(),
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            model: model.to_string(),
            stream: true,
            status_code: Some(200),
            status_kind: RequestStatusKind::Success,
            ttft_ms: Some(ttft_ms),
            duration_ms: Some(5000.0),
            input_tokens: Some(100),
            output_tokens: Some(200),
            cache_read_tokens: None,
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

    #[test]
    fn detects_slow_ttft_and_generates_hypothesis() {
        let req = sample_request_with_ttft("req-1", "claude-sonnet-4-5", 4200.0);
        let rules = AnalyzerRules {
            slow_ttft_threshold_ms: 3000.0,
            stall_threshold_s: 0.5,
        };

        let anomalies = detect_anomalies(&req, &rules, &[]);
        assert_eq!(anomalies.len(), 1);
        assert_eq!(anomalies[0].kind, AnomalyKind::SlowTtft);
        assert!(anomalies[0].hypothesis.as_ref().unwrap().contains("4200"));
    }
}
