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

impl RequestStatusKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Success => "success",
            Self::ClientError => "client_error",
            Self::ServerError => "server_error",
            Self::Timeout => "timeout",
            Self::ConnectionError => "connection_error",
            Self::ProxyError => "proxy_error",
        }
    }
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

// ─── Forward-compatibility: unknown field tracking ───

/// Known SSE event types in the Claude Messages API streaming protocol.
pub const KNOWN_SSE_EVENTS: &[&str] = &[
    "message_start",
    "content_block_start",
    "content_block_delta",
    "content_block_stop",
    "message_delta",
    "message_stop",
    "ping",
    "error",
];

/// Known stop reasons returned by the Claude Messages API.
pub const KNOWN_STOP_REASONS: &[&str] = &["end_turn", "max_tokens", "stop_sequence", "tool_use"];

/// Tracks unknown / unrecognised protocol fields observed during streaming,
/// enabling forward-compatible detection of API changes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UnknownFieldStats {
    pub unknown_sse_event_types: Vec<String>,
    pub unknown_content_block_types: Vec<String>,
    pub unknown_request_fields: Vec<String>,
    pub unknown_header_patterns: Vec<String>,
    pub unknown_stop_reasons: Vec<String>,
    pub unknown_delta_types: Vec<String>,
}

impl UnknownFieldStats {
    /// Record an SSE event type if it is not in the known set.
    /// Deduplicates: the same value is stored at most once.
    pub fn record_unknown_event(&mut self, event_type: &str) {
        if !KNOWN_SSE_EVENTS.contains(&event_type)
            && !self.unknown_sse_event_types.iter().any(|e| e == event_type)
        {
            self.unknown_sse_event_types.push(event_type.to_string());
        }
    }

    /// Record a stop reason if it is not in the known set.
    /// Deduplicates: the same value is stored at most once.
    pub fn record_unknown_stop_reason(&mut self, reason: &str) {
        if !KNOWN_STOP_REASONS.contains(&reason)
            && !self.unknown_stop_reasons.iter().any(|r| r == reason)
        {
            self.unknown_stop_reasons.push(reason.to_string());
        }
    }
}

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

    #[test]
    fn unknown_sse_events_are_recorded() {
        let mut stats = UnknownFieldStats::default();
        stats.record_unknown_event("new_fancy_event");
        assert_eq!(stats.unknown_sse_event_types, vec!["new_fancy_event"]);
    }

    #[test]
    fn known_sse_events_are_not_recorded() {
        let mut stats = UnknownFieldStats::default();
        for &known in KNOWN_SSE_EVENTS {
            stats.record_unknown_event(known);
        }
        assert!(stats.unknown_sse_event_types.is_empty());
    }

    #[test]
    fn unknown_stop_reasons_are_recorded() {
        let mut stats = UnknownFieldStats::default();
        stats.record_unknown_stop_reason("cancelled");
        assert_eq!(stats.unknown_stop_reasons, vec!["cancelled"]);
    }

    #[test]
    fn known_stop_reasons_are_not_recorded() {
        let mut stats = UnknownFieldStats::default();
        for &known in KNOWN_STOP_REASONS {
            stats.record_unknown_stop_reason(known);
        }
        assert!(stats.unknown_stop_reasons.is_empty());
    }

    #[test]
    fn duplicate_unknown_events_are_deduplicated() {
        let mut stats = UnknownFieldStats::default();
        stats.record_unknown_event("exotic_event");
        stats.record_unknown_event("exotic_event");
        assert_eq!(stats.unknown_sse_event_types.len(), 1);
    }

    #[test]
    fn duplicate_unknown_stop_reasons_are_deduplicated() {
        let mut stats = UnknownFieldStats::default();
        stats.record_unknown_stop_reason("cancelled");
        stats.record_unknown_stop_reason("cancelled");
        assert_eq!(stats.unknown_stop_reasons.len(), 1);
    }
}
