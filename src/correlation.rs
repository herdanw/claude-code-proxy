use serde::{Deserialize, Serialize};
use std::str::FromStr;

use crate::types::RequestRecord;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadPolicy {
    MetadataOnly,
    Redacted,
    Full,
}

impl FromStr for PayloadPolicy {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "metadata" | "metadata_only" => Ok(Self::MetadataOnly),
            "redacted" => Ok(Self::Redacted),
            "full" => Ok(Self::Full),
            other => Err(format!("Unsupported payload policy: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrelationLink {
    pub link_type: String,
    pub confidence: f64,
    pub local_event_id: Option<String>,
    pub reason: String,
}

/// Generate correlation links for a request by matching against local events.
/// events: Vec of (event_id, event_time_ms, session_hint, event_kind)
pub fn find_correlations(
    req: &RequestRecord,
    events: &[(String, i64, Option<String>, String)],
) -> Vec<CorrelationLink> {
    let mut links = Vec::new();
    let req_time_ms = req.timestamp.timestamp_millis();

    for (event_id, event_time_ms, session_hint, event_kind) in events {
        let time_diff_ms = (req_time_ms - event_time_ms).unsigned_abs();

        // Rule 1: Temporal match — event within ±5 seconds
        if time_diff_ms <= 5000 {
            let confidence = 1.0 - (time_diff_ms as f64 / 5000.0) * 0.5;
            links.push(CorrelationLink {
                link_type: "temporal".to_string(),
                confidence,
                local_event_id: Some(event_id.clone()),
                reason: format!(
                    "Event occurred {:.1}s from request",
                    time_diff_ms as f64 / 1000.0
                ),
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
                reason: format!(
                    "Settings changed {:.0}s before/after request",
                    time_diff_ms as f64 / 1000.0
                ),
            });
        }
    }

    // Dedup by event_id, keeping highest confidence
    links.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
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
mod tests {
    use super::*;

    #[test]
    fn parse_payload_policy_from_str() {
        use std::str::FromStr;

        assert_eq!(
            PayloadPolicy::from_str("metadata").unwrap(),
            PayloadPolicy::MetadataOnly
        );
        assert_eq!(
            PayloadPolicy::from_str("redacted").unwrap(),
            PayloadPolicy::Redacted
        );
        assert_eq!(
            PayloadPolicy::from_str("full").unwrap(),
            PayloadPolicy::Full
        );
        assert!(PayloadPolicy::from_str("unknown").is_err());
    }

    use crate::types::RequestStatusKind;
    use chrono::Utc;

    fn sample_request() -> RequestRecord {
        RequestRecord {
            id: "req-1".to_string(),
            session_id: Some("sess-1".to_string()),
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
        let events = vec![(
            "evt-1".to_string(),
            req_ms - 10000,
            Some("sess-1".to_string()),
            "tool_use".to_string(),
        )];
        let links = find_correlations(&req, &events);
        assert!(links.iter().any(|l| l.link_type == "session"));
    }

    #[test]
    fn config_drift_within_60s() {
        let req = sample_request();
        let req_ms = req.timestamp.timestamp_millis();
        let events = vec![(
            "evt-1".to_string(),
            req_ms - 30000,
            None,
            "config_change".to_string(),
        )];
        let links = find_correlations(&req, &events);
        assert!(links.iter().any(|l| l.link_type == "config_drift"));
    }
}
