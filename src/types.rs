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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelProfileAssignment {
    pub model_name: String,
    pub behavior_class: Option<String>,
    pub sample_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryConformanceScore {
    pub category: String,
    pub score_percent: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConformanceSummary {
    pub model_name: String,
    pub overall_score_percent: f64,
    pub category_scores: Vec<CategoryConformanceScore>,
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
}
