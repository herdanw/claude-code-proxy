use crate::stats::{
    AnomalyKind, CorrelationLinkType, Explanation, LocalEvent, RequestCorrelation, RequestEntry,
};

#[derive(Debug, Clone)]
pub struct ExplanationContext {
    pub correlations: Vec<RequestCorrelation>,
    pub local_events: Vec<LocalEvent>,
}

pub fn explain_request(request: &RequestEntry, context: &ExplanationContext) -> Vec<Explanation> {
    let mut explanations = Vec::new();

    let has_slow_ttft = request
        .anomalies
        .iter()
        .any(|anomaly| matches!(anomaly.kind, AnomalyKind::SlowTtft));

    if has_slow_ttft {
        if let Some(config_link) = context
            .correlations
            .iter()
            .find(|link| matches!(link.link_type, CorrelationLinkType::ConfigDrift))
        {
            explanations.push(Explanation {
                id: format!("exp:{}:slow_ttft_config_drift", request.id),
                request_id: request.id.clone(),
                anomaly_kind: "slow_ttft".into(),
                rank: 1,
                confidence: config_link.confidence.clamp(0.0, 1.0),
                summary: format!("Slow TTFT may be related to model/config drift: {}", config_link.reason),
                evidence_json: serde_json::json!({
                    "correlation_id": config_link.id,
                    "link_type": "config_drift",
                    "reason": config_link.reason,
                }),
                created_at_ms: chrono::Utc::now().timestamp_millis(),
            });
        }
    }

    if explanations.is_empty() {
        explanations.push(Explanation {
            id: format!("exp:{}:fallback", request.id),
            request_id: request.id.clone(),
            anomaly_kind: request
                .anomalies
                .first()
                .map(|a| format!("{:?}", a.kind).to_ascii_lowercase())
                .unwrap_or_else(|| "unknown".into()),
            rank: 1,
            confidence: 0.4,
            summary: "No high-confidence contextual explanation available yet.".into(),
            evidence_json: serde_json::json!({
                "correlation_count": context.correlations.len(),
                "local_event_count": context.local_events.len(),
            }),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        });
    }

    explanations
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::{
        Anomaly, AnomalyKind, CorrelationLinkType, RequestCorrelation, RequestEntry, RequestStatus,
        Severity,
    };

    fn sample_entry() -> RequestEntry {
        RequestEntry {
            id: "sample-request".into(),
            timestamp: chrono::Utc::now(),
            session_id: Some("session-1".into()),
            method: "POST".into(),
            path: "/v1/messages".into(),
            model: "claude-opus-4.6".into(),
            stream: true,
            status: RequestStatus::Success(200),
            duration_ms: 5_000.0,
            ttft_ms: Some(500.0),
            input_tokens: Some(100),
            output_tokens: Some(200),
            cache_read_tokens: None,
            cache_creation_tokens: None,
            thinking_tokens: None,
            stop_reason: None,
            request_size_bytes: 512,
            response_size_bytes: 1024,
            stalls: Vec::new(),
            error: None,
            anomalies: Vec::new(),
        }
    }

    #[test]
    fn explain_slow_ttft_with_recent_config_drift() {
        let mut request = sample_entry();
        request.id = "req-1".into();
        request.ttft_ms = Some(12_500.0);
        request.anomalies = vec![Anomaly {
            kind: AnomalyKind::SlowTtft,
            message: "slow ttft".into(),
            severity: Severity::Warning,
        }];

        let context = ExplanationContext {
            correlations: vec![RequestCorrelation {
                id: "corr-1".into(),
                request_id: "req-1".into(),
                local_event_id: "evt-1".into(),
                link_type: CorrelationLinkType::ConfigDrift,
                confidence: 0.91,
                reason: "model changed 2m earlier".into(),
                created_at_ms: chrono::Utc::now().timestamp_millis(),
            }],
            local_events: vec![],
        };

        let explanations = explain_request(&request, &context);
        assert!(!explanations.is_empty());
        assert!(explanations[0].summary.to_ascii_lowercase().contains("model"));
    }
}
