use crate::stats::{CorrelationLinkType, LocalEvent, RequestCorrelation, RequestEntry, SourceKind};

#[derive(Debug, Clone, Copy)]
pub struct CorrelationConfig {
    pub temporal_window_ms: i64,
    pub max_links: usize,
    pub session_hint_confidence: f64,
    pub temporal_confidence: f64,
}

impl Default for CorrelationConfig {
    fn default() -> Self {
        Self {
            temporal_window_ms: 60_000,
            max_links: 1,
            session_hint_confidence: 0.95,
            temporal_confidence: 0.7,
        }
    }
}

pub fn correlate_request(
    request: &RequestEntry,
    events: &[LocalEvent],
    config: &CorrelationConfig,
) -> Vec<RequestCorrelation> {
    if events.is_empty() || config.max_links == 0 {
        return Vec::new();
    }

    let request_ms = request.timestamp.timestamp_millis();

    if let Some(session_id) = request.session_id.as_deref() {
        let mut session_matches: Vec<&LocalEvent> = events
            .iter()
            .filter(|event| event.session_hint.as_deref() == Some(session_id))
            .collect();

        if !session_matches.is_empty() {
            session_matches.sort_by_key(|event| (request_ms - event.event_time_ms).abs());
            return session_matches
                .into_iter()
                .take(config.max_links)
                .map(|event| RequestCorrelation {
                    id: correlation_id(&request.id, &event.id, CorrelationLinkType::SessionHint),
                    request_id: request.id.clone(),
                    local_event_id: event.id.clone(),
                    link_type: CorrelationLinkType::SessionHint,
                    confidence: config.session_hint_confidence,
                    reason: format!(
                        "session_hint matched request session_id '{}'",
                        session_id
                    ),
                    created_at_ms: chrono::Utc::now().timestamp_millis(),
                })
                .collect();
        }
    }

    let mut temporal_matches: Vec<&LocalEvent> = events
        .iter()
        .filter(|event| (request_ms - event.event_time_ms).abs() <= config.temporal_window_ms)
        .collect();

    temporal_matches.sort_by_key(|event| (request_ms - event.event_time_ms).abs());

    temporal_matches
        .into_iter()
        .take(config.max_links)
        .map(|event| {
            let link_type = match event.source_kind {
                SourceKind::Config => CorrelationLinkType::ConfigDrift,
                SourceKind::ShellSnapshot => CorrelationLinkType::CommandProximity,
                _ => CorrelationLinkType::Temporal,
            };

            let reason = match link_type {
                CorrelationLinkType::ConfigDrift => "config drift near request timestamp".to_string(),
                CorrelationLinkType::CommandProximity => {
                    "shell command activity near request timestamp".to_string()
                }
                _ => format!("temporal proximity within {} ms", config.temporal_window_ms),
            };

            RequestCorrelation {
                id: correlation_id(&request.id, &event.id, link_type),
                request_id: request.id.clone(),
                local_event_id: event.id.clone(),
                link_type,
                confidence: config.temporal_confidence,
                reason,
                created_at_ms: chrono::Utc::now().timestamp_millis(),
            }
        })
        .collect()
}

fn correlation_id(request_id: &str, event_id: &str, link_type: CorrelationLinkType) -> String {
    let link_type_text = match link_type {
        CorrelationLinkType::SessionHint => "session_hint",
        CorrelationLinkType::Temporal => "temporal",
        CorrelationLinkType::ConfigDrift => "config_drift",
        CorrelationLinkType::CommandProximity => "command_proximity",
    };

    format!(
        "corr:{}:{}:{}:{}:{}",
        request_id.len(),
        request_id,
        event_id.len(),
        event_id,
        link_type_text
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::correlation::PayloadPolicy;
    use crate::stats::{RequestStatus, SourceKind};
    use chrono::{TimeZone, Utc};

    fn sample_request(id: &str, session_id: Option<&str>, timestamp_ms: i64) -> RequestEntry {
        RequestEntry {
            id: id.to_string(),
            timestamp: Utc.timestamp_millis_opt(timestamp_ms).single().unwrap(),
            session_id: session_id.map(|s| s.to_string()),
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            model: "claude-opus-4.6".to_string(),
            stream: true,
            status: RequestStatus::Success(200),
            duration_ms: 1000.0,
            ttft_ms: Some(120.0),
            input_tokens: Some(10),
            output_tokens: Some(20),
            cache_read_tokens: None,
            cache_creation_tokens: None,
            thinking_tokens: None,
            request_size_bytes: 100,
            response_size_bytes: 200,
            stalls: vec![],
            error: None,
            anomalies: vec![],
        }
    }

    fn sample_event(id: &str, session_hint: Option<&str>, event_time_ms: i64) -> LocalEvent {
        LocalEvent {
            id: id.to_string(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "project/a/session.json".to_string(),
            event_time_ms,
            session_hint: session_hint.map(|s| s.to_string()),
            event_kind: "session_touch".to_string(),
            model_hint: None,
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({}),
        }
    }

    #[test]
    fn correlation_id_is_stable_and_unambiguous() {
        let first = correlation_id("a-b", "c", CorrelationLinkType::Temporal);
        let second = correlation_id("a", "b-c", CorrelationLinkType::Temporal);
        let repeat = correlation_id("a-b", "c", CorrelationLinkType::Temporal);

        assert_eq!(first, repeat);
        assert_ne!(first, second);
    }

    #[test]
    fn correlate_prefers_session_hint_then_temporal_link() {
        let request_ms = Utc::now().timestamp_millis();
        let config = CorrelationConfig::default();

        let request_with_session = sample_request("req-1", Some("session-abc"), request_ms);
        let events = vec![
            sample_event("evt-session", Some("session-abc"), request_ms - 20_000),
            sample_event("evt-nearby", Some("other-session"), request_ms - 500),
        ];

        let session_links = correlate_request(&request_with_session, &events, &config);
        assert_eq!(session_links.len(), 1);
        assert_eq!(session_links[0].local_event_id, "evt-session");
        assert_eq!(session_links[0].link_type, CorrelationLinkType::SessionHint);
        assert_eq!(session_links[0].confidence, config.session_hint_confidence);
        assert!(session_links[0].reason.contains("session_hint matched"));

        let request_without_session = sample_request("req-2", None, request_ms);
        let temporal_links = correlate_request(&request_without_session, &events, &config);
        assert_eq!(temporal_links.len(), 1);
        assert_eq!(temporal_links[0].local_event_id, "evt-nearby");
        assert_eq!(temporal_links[0].link_type, CorrelationLinkType::Temporal);
        assert_eq!(temporal_links[0].confidence, config.temporal_confidence);
        assert!(temporal_links[0].reason.contains("temporal proximity"));
    }

    #[test]
    fn correlate_maps_shell_and_config_events_to_special_link_types() {
        let request_ms = Utc::now().timestamp_millis();
        let request = sample_request("req-special-links", None, request_ms);

        let mut shell_event = sample_event("evt-shell", None, request_ms - 200);
        shell_event.source_kind = SourceKind::ShellSnapshot;
        shell_event.event_kind = "shell_snapshot".into();

        let mut config_event = sample_event("evt-config", None, request_ms - 100);
        config_event.source_kind = SourceKind::Config;
        config_event.event_kind = "config_change".into();

        let links = correlate_request(&request, &[shell_event, config_event], &CorrelationConfig::default());

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].local_event_id, "evt-config");
        assert_eq!(links[0].link_type, CorrelationLinkType::ConfigDrift);
    }
}
