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
    recent: &[RequestRecord],
) -> Vec<DetectedAnomaly> {
    let mut out = Vec::new();

    // SlowTtft
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

    // Stall
    if req.stall_count > 0 {
        out.push(DetectedAnomaly {
            kind: AnomalyKind::Stall,
            severity: Severity::Warning,
            summary: format!("Request had {} stall(s)", req.stall_count),
            hypothesis: Some(format!(
                "Model {} experienced {} stall(s) exceeding {:.1}s threshold. Possible upstream congestion.",
                req.model, req.stall_count, rules.stall_threshold_s
            )),
        });
    }

    // Status-code based rules
    if let Some(status) = req.status_code {
        match status {
            429 => {
                out.push(DetectedAnomaly {
                    kind: AnomalyKind::RateLimited,
                    severity: Severity::Warning,
                    summary: "Rate limited (429)".to_string(),
                    hypothesis: Some("Request was rate-limited by the API. Consider backing off or checking usage limits.".to_string()),
                });
            }
            529 => {
                out.push(DetectedAnomaly {
                    kind: AnomalyKind::Overload,
                    severity: Severity::Error,
                    summary: "API overloaded (529)".to_string(),
                    hypothesis: Some("The API is temporarily overloaded. Retry with exponential backoff.".to_string()),
                });
            }
            500..=599 => {
                out.push(DetectedAnomaly {
                    kind: AnomalyKind::ApiError,
                    severity: Severity::Error,
                    summary: format!("API server error ({})", status),
                    hypothesis: Some(format!("Server returned {}. This is an upstream issue.", status)),
                });
            }
            400..=499 => {
                out.push(DetectedAnomaly {
                    kind: AnomalyKind::ClientError,
                    severity: Severity::Warning,
                    summary: format!("Client error ({})", status),
                    hypothesis: Some(format!("Status {} indicates a client-side issue. Check request format and parameters.", status)),
                });
            }
            _ => {}
        }
    }

    // MaxTokensHit
    if req.stop_reason.as_deref() == Some("max_tokens") {
        out.push(DetectedAnomaly {
            kind: AnomalyKind::MaxTokensHit,
            severity: Severity::Info,
            summary: "Response stopped due to max_tokens limit".to_string(),
            hypothesis: Some("Output was truncated at the max_tokens limit. Consider increasing max_tokens if full output is needed.".to_string()),
        });
    }

    // InterruptedStream
    if req.stream && req.stop_reason.is_none() && req.error_summary.is_some() {
        out.push(DetectedAnomaly {
            kind: AnomalyKind::InterruptedStream,
            severity: Severity::Warning,
            summary: "Stream interrupted before completion".to_string(),
            hypothesis: Some(format!(
                "Streaming response ended without a stop_reason. Error: {}",
                req.error_summary.as_deref().unwrap_or("unknown")
            )),
        });
    }

    // Trend-based rules (require ≥10 recent samples)
    if recent.len() >= 10 {
        // Timeout: duration > 2× model avg AND no successful response
        if let Some(duration) = req.duration_ms {
            let avg_duration = {
                let durations: Vec<f64> = recent.iter().filter_map(|r| r.duration_ms).collect();
                if durations.is_empty() {
                    0.0
                } else {
                    durations.iter().sum::<f64>() / durations.len() as f64
                }
            };
            let is_failure = req.status_code.map_or(true, |s| s >= 400);
            if avg_duration > 0.0 && duration > 2.0 * avg_duration && is_failure {
                out.push(DetectedAnomaly {
                    kind: AnomalyKind::Timeout,
                    severity: Severity::Error,
                    summary: format!(
                        "Duration {:.0}ms is >{:.0}× model average ({:.0}ms) with failed status",
                        duration, duration / avg_duration, avg_duration
                    ),
                    hypothesis: Some("Request took significantly longer than average and failed. Likely a timeout.".to_string()),
                });
            }
        }

        // HighTokens: output_tokens > 2× model average
        if let Some(output) = req.output_tokens {
            let avg_output = {
                let outputs: Vec<u64> = recent.iter().filter_map(|r| r.output_tokens).collect();
                if outputs.is_empty() {
                    0.0
                } else {
                    outputs.iter().sum::<u64>() as f64 / outputs.len() as f64
                }
            };
            if avg_output > 0.0 && output as f64 > 2.0 * avg_output {
                out.push(DetectedAnomaly {
                    kind: AnomalyKind::HighTokens,
                    severity: Severity::Info,
                    summary: format!(
                        "Output tokens {} is >{:.1}× model average ({:.0})",
                        output, output as f64 / avg_output, avg_output
                    ),
                    hypothesis: Some("Unusually high token output. May indicate verbose response or unexpected behavior.".to_string()),
                });
            }
        }

        // CacheMiss: cache_read_tokens = 0 when model avg cache_read > 0
        let cache_read = req.cache_read_tokens.unwrap_or(0);
        let avg_cache = {
            let caches: Vec<u64> = recent.iter().filter_map(|r| r.cache_read_tokens).collect();
            if caches.is_empty() {
                0.0
            } else {
                caches.iter().sum::<u64>() as f64 / caches.len() as f64
            }
        };
        if cache_read == 0 && avg_cache > 0.0 {
            out.push(DetectedAnomaly {
                kind: AnomalyKind::CacheMiss,
                severity: Severity::Info,
                summary: format!(
                    "No cache read tokens when model average is {:.0}",
                    avg_cache
                ),
                hypothesis: Some("This request had zero cache reads while the model typically uses caching. Check if system prompt or context changed.".to_string()),
            });
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AnomalyKind, RequestRecord, RequestStatusKind};
    use chrono::Utc;

    fn sample_request() -> RequestRecord {
        RequestRecord {
            id: "req-1".to_string(),
            session_id: None,
            timestamp: Utc::now(),
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            model: "claude-sonnet-4-5".to_string(),
            stream: true,
            status_code: Some(200),
            status_kind: RequestStatusKind::Success,
            ttft_ms: Some(500.0),
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

    fn default_rules() -> AnalyzerRules {
        AnalyzerRules {
            slow_ttft_threshold_ms: 3000.0,
            stall_threshold_s: 0.5,
        }
    }

    #[test]
    fn detects_slow_ttft_and_generates_hypothesis() {
        let mut req = sample_request();
        req.ttft_ms = Some(4200.0);
        let anomalies = detect_anomalies(&req, &default_rules(), &[]);
        assert_eq!(anomalies.len(), 1);
        assert_eq!(anomalies[0].kind, AnomalyKind::SlowTtft);
        assert!(anomalies[0].hypothesis.as_ref().unwrap().contains("4200"));
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
        let anomalies = detect_anomalies(&req, &default_rules(), &[]);
        assert!(anomalies.iter().any(|a| a.kind == AnomalyKind::ApiError));
    }

    #[test]
    fn detects_client_error_not_429() {
        let mut req = sample_request();
        req.status_code = Some(400);
        let anomalies = detect_anomalies(&req, &default_rules(), &[]);
        assert!(anomalies.iter().any(|a| a.kind == AnomalyKind::ClientError));
        assert!(!anomalies.iter().any(|a| a.kind == AnomalyKind::RateLimited));
    }

    #[test]
    fn detects_rate_limited() {
        let mut req = sample_request();
        req.status_code = Some(429);
        let anomalies = detect_anomalies(&req, &default_rules(), &[]);
        assert!(anomalies.iter().any(|a| a.kind == AnomalyKind::RateLimited));
        assert!(!anomalies.iter().any(|a| a.kind == AnomalyKind::ClientError));
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
        let req = {
            let mut r = sample_request();
            r.output_tokens = Some(5000);
            r
        };
        let recent: Vec<RequestRecord> = (0..15)
            .map(|_| {
                let mut r = sample_request();
                r.output_tokens = Some(200);
                r
            })
            .collect();
        let anomalies = detect_anomalies(&req, &default_rules(), &recent);
        assert!(anomalies.iter().any(|a| a.kind == AnomalyKind::HighTokens));
    }

    #[test]
    fn no_high_tokens_without_enough_samples() {
        let req = {
            let mut r = sample_request();
            r.output_tokens = Some(5000);
            r
        };
        let recent: Vec<RequestRecord> = (0..5)
            .map(|_| {
                let mut r = sample_request();
                r.output_tokens = Some(200);
                r
            })
            .collect();
        let anomalies = detect_anomalies(&req, &default_rules(), &recent);
        assert!(!anomalies.iter().any(|a| a.kind == AnomalyKind::HighTokens));
    }

    #[test]
    fn detects_cache_miss() {
        let req = {
            let mut r = sample_request();
            r.cache_read_tokens = Some(0);
            r
        };
        let recent: Vec<RequestRecord> = (0..15)
            .map(|_| {
                let mut r = sample_request();
                r.cache_read_tokens = Some(500);
                r
            })
            .collect();
        let anomalies = detect_anomalies(&req, &default_rules(), &recent);
        assert!(anomalies.iter().any(|a| a.kind == AnomalyKind::CacheMiss));
    }

    #[test]
    fn healthy_request_produces_no_anomalies() {
        let req = sample_request();
        let anomalies = detect_anomalies(&req, &default_rules(), &[]);
        assert!(anomalies.is_empty(), "Expected no anomalies, got: {:?}", anomalies.iter().map(|a| &a.kind).collect::<Vec<_>>());
    }
}
