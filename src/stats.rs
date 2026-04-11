use chrono::{DateTime, Utc};
use parking_lot::{Mutex, RwLock};
use rusqlite::{params, params_from_iter, types::Value, Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::str::FromStr;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

use crate::correlation::PayloadPolicy;

// ─── Entry Types ───

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEntry {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub session_id: Option<String>,
    pub method: String,
    pub path: String,
    pub model: String,
    pub stream: bool,
    pub status: RequestStatus,
    pub duration_ms: f64,
    pub ttft_ms: Option<f64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_creation_tokens: Option<u64>,
    pub thinking_tokens: Option<u64>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    pub request_size_bytes: u64,
    pub response_size_bytes: u64,
    pub stalls: Vec<StallEvent>,
    pub error: Option<String>,
    pub anomalies: Vec<Anomaly>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEntryWithAnomalyMeta {
    #[serde(flatten)]
    pub entry: RequestEntry,
    pub distance_ms: Option<i64>,
    pub within_window: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomalyFocusedEntries {
    pub entries: Vec<RequestEntryWithAnomalyMeta>,
    pub preselected_request_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum RequestStatus {
    Success(u16),
    ClientError(u16),
    ServerError(u16),
    Timeout,
    ConnectionError,
    ProxyError,
    Pending,
}

impl RequestStatus {
    pub fn from_code(code: u16) -> Self {
        match code {
            200..=299 => Self::Success(code),
            400..=499 => Self::ClientError(code),
            500..=599 => Self::ServerError(code),
            _ => Self::ProxyError,
        }
    }

    pub fn is_error(&self) -> bool {
        !matches!(self, Self::Success(_) | Self::Pending)
    }

    pub fn code_str(&self) -> String {
        match self {
            Self::Success(c) | Self::ClientError(c) | Self::ServerError(c) => c.to_string(),
            Self::Timeout => "TIMEOUT".into(),
            Self::ConnectionError => "CONN_ERR".into(),
            Self::ProxyError => "ERROR".into(),
            Self::Pending => "PENDING".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StallEvent {
    pub timestamp: DateTime<Utc>,
    pub duration_s: f64,
    pub bytes_before: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anomaly {
    pub kind: AnomalyKind,
    pub message: String,
    pub severity: Severity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AnomalyKind {
    SlowTtft,
    StreamStall,
    HighErrorRate,
    Timeout,
    ConnectionFailure,
    RateLimited,
    ModelError,
    GatewayError,
    SlowResponse,
    LargeResponse,
    RepeatedErrors,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Severity {
    Warning,
    Error,
    Critical,
}

// ─── Live Stats Summary ───

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveStats {
    pub total_requests: u64,
    pub total_errors: u64,
    pub success_rate: f64,
    pub avg_ttft_ms: f64,
    pub median_ttft_ms: f64,
    pub p95_ttft_ms: f64,
    pub p99_ttft_ms: f64,
    pub worst_ttft_ms: f64,
    pub avg_duration_ms: f64,
    pub total_stalls: u64,
    pub total_stall_time_s: f64,
    pub total_timeouts: u64,
    pub total_connection_errors: u64,
    pub total_4xx: u64,
    pub total_5xx: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub health_score: f64,
    pub health_label: String,
    pub uptime_s: f64,
    pub requests_per_minute: f64,
    pub errors_last_5min: u64,
    pub model_breakdown: Vec<(String, u64)>,
    pub error_breakdown: Vec<(String, u64)>,
    pub recent_anomalies: Vec<Anomaly>,
    // Timeseries for charts (last 60 data points, 1 per minute)
    pub ttft_timeseries: Vec<TimeseriesPoint>,
    pub error_timeseries: Vec<TimeseriesPoint>,
    pub request_timeseries: Vec<TimeseriesPoint>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StatsMode {
    Live,
    Historical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsSnapshot {
    pub mode: StatsMode,
    pub stats: LiveStats,
    pub coverage_start: Option<DateTime<Utc>>,
    pub coverage_end: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeseriesPoint {
    pub timestamp: DateTime<Utc>,
    pub value: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClearMode {
    MemoryOnly,
    StatsAndLogs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClearSummary {
    pub mode: ClearMode,
    pub cleared_entries: usize,
    pub deleted_persisted_entries: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    ClaudeProject,
    ShellSnapshot,
    Config,
    Git,
}

impl SourceKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeProject => "claude_project",
            Self::ShellSnapshot => "shell_snapshot",
            Self::Config => "config",
            Self::Git => "git",
        }
    }
}

impl FromStr for SourceKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "claude_project" => Ok(Self::ClaudeProject),
            "shell_snapshot" => Ok(Self::ShellSnapshot),
            "config" => Ok(Self::Config),
            "git" => Ok(Self::Git),
            other => Err(format!("Unsupported source kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalEvent {
    pub id: String,
    pub source_kind: SourceKind,
    pub source_path: String,
    pub event_time_ms: i64,
    pub session_hint: Option<String>,
    pub event_kind: String,
    pub model_hint: Option<String>,
    pub payload_policy: PayloadPolicy,
    pub payload_json: serde_json::Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CorrelationLinkType {
    SessionHint,
    Temporal,
    ConfigDrift,
    CommandProximity,
}

impl CorrelationLinkType {
    fn as_str(self) -> &'static str {
        match self {
            Self::SessionHint => "session_hint",
            Self::Temporal => "temporal",
            Self::ConfigDrift => "config_drift",
            Self::CommandProximity => "command_proximity",
        }
    }
}

impl FromStr for CorrelationLinkType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "session_hint" => Ok(Self::SessionHint),
            "temporal" => Ok(Self::Temporal),
            "config_drift" => Ok(Self::ConfigDrift),
            "command_proximity" => Ok(Self::CommandProximity),
            other => Err(format!("Unsupported correlation link type: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestCorrelation {
    pub id: String,
    pub request_id: String,
    pub local_event_id: String,
    pub link_type: CorrelationLinkType,
    pub confidence: f64,
    pub reason: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Explanation {
    pub id: String,
    pub request_id: String,
    pub anomaly_kind: String,
    pub rank: i64,
    pub confidence: f64,
    pub summary: String,
    pub evidence_json: serde_json::Value,
    pub created_at_ms: i64,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsHistoryItem {
    pub id: String,
    pub saved_at_ms: i64,
    pub content_hash: String,
    pub settings_json: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestBodyRecord {
    pub request_body: String,
    pub response_body: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeSession {
    pub session_id: String,
    pub project_path: String,
    pub project_paths: Vec<String>,
    pub last_modified_ms: i64,
    pub last_local_activity_ms: i64,
    pub last_proxy_activity_ms: i64,
    pub request_count: u64,
    pub in_flight_requests: u64,
    pub is_live: bool,
    pub has_proxy_requests: bool,
    pub has_local_evidence: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedSessionSummary {
    pub session_id: String,
    pub presence: String,
    pub proxy_request_count: u64,
    pub local_event_count: u64,
    pub proxy_error_count: u64,
    pub proxy_stall_count: u64,
    pub last_proxy_activity_ms: Option<i64>,
    pub last_local_activity_ms: Option<i64>,
    pub last_activity_ms: i64,
    pub project_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct SessionDeleteDbRowsByTable {
    pub request_bodies: u64,
    pub request_correlations: u64,
    pub explanations: u64,
    pub requests: u64,
    pub local_events: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDeleteDbOutcome {
    pub blocked_live: bool,
    pub deleted_db_rows_by_table: SessionDeleteDbRowsByTable,
}

// ─── Stats Store ───

pub struct StatsStore {
    entries: RwLock<VecDeque<RequestEntry>>,
    max_entries: usize,
    #[allow(dead_code)]
    storage_dir: PathBuf,
    db_path: PathBuf,
    db: Mutex<Connection>,
    start_time: RwLock<DateTime<Utc>>,
    pub stall_threshold: f64,
    pub slow_ttft_threshold: f64,
    pub max_body_size: usize,
    claude_dir: PathBuf,
    // WebSocket broadcast channel
    pub broadcast_tx: tokio::sync::broadcast::Sender<String>,
    #[cfg(test)]
    pre_commit_live_guard_flip_pending: AtomicBool,
}

impl StatsStore {
    pub fn new(
        max_entries: usize,
        storage_dir: PathBuf,
        stall_threshold: f64,
        slow_ttft_threshold: f64,
        max_body_size: usize,
        claude_dir: PathBuf,
    ) -> Self {
        let _ = std::fs::create_dir_all(&storage_dir);
        let db_path = storage_dir.join("proxy.db");
        let db = Connection::open(&db_path).expect("Failed to open SQLite database");
        let (broadcast_tx, _) = tokio::sync::broadcast::channel(1000);
        let store = Self {
            entries: RwLock::new(VecDeque::with_capacity(max_entries)),
            max_entries,
            storage_dir,
            db_path,
            db: Mutex::new(db),
            start_time: RwLock::new(Utc::now()),
            stall_threshold,
            slow_ttft_threshold,
            max_body_size,
            claude_dir,
            broadcast_tx,
            #[cfg(test)]
            pre_commit_live_guard_flip_pending: AtomicBool::new(false),
        };

        store.initialize_database();
        store
    }

    pub fn add_entry(&self, mut entry: RequestEntry) {
        // Detect anomalies
        self.detect_anomalies(&mut entry);

        // Persist to SQLite
        self.write_to_db(&entry);

        // Broadcast to WebSocket clients
        if let Ok(json) = serde_json::to_string(&entry) {
            let msg = format!("{{\"type\":\"entry\",\"data\":{json}}}");
            let _ = self.broadcast_tx.send(msg);
        }

        // Print to console
        self.print_entry(&entry);

        // Store in memory
        {
            let mut entries = self.entries.write();
            entries.push_back(entry);
            while entries.len() > self.max_entries {
                entries.pop_front();
            }
        }

        let stats = self.get_live_stats_snapshot();
        if let Ok(json) = serde_json::to_string(&stats) {
            let msg = format!("{{\"type\":\"stats\",\"data\":{json}}}");
            let _ = self.broadcast_tx.send(msg);
        }
    }

    pub fn database_path(&self) -> &Path {
        &self.db_path
    }

    pub fn claude_dir(&self) -> &Path {
        &self.claude_dir
    }

    #[cfg(test)]
    pub fn persisted_entry_count(&self) -> usize {
        let conn = self.db.lock();
        conn.query_row("SELECT COUNT(*) FROM requests", [], |row| {
            row.get::<_, i64>(0)
        })
        .map(|count| count.max(0) as usize)
        .unwrap_or(0)
    }

    #[cfg(test)]
    pub fn trigger_pre_commit_live_guard_flip_for_test(&self) {
        self.pre_commit_live_guard_flip_pending
            .store(true, Ordering::SeqCst);
    }

    pub fn get_live_stats_snapshot(&self) -> StatsSnapshot {
        let now = Utc::now();
        let coverage_start = Some(*self.start_time.read());

        StatsSnapshot {
            mode: StatsMode::Live,
            stats: self.get_live_stats(),
            coverage_start,
            coverage_end: Some(now),
        }
    }

    pub fn get_historical_stats_snapshot(&self) -> StatsSnapshot {
        let entries = self.load_entries_from_db(
            "SELECT entry_json FROM requests ORDER BY timestamp_ms ASC",
            Vec::new(),
        );

        let coverage_start = entries.first().map(|entry| entry.timestamp);
        let coverage_end = entries.last().map(|entry| entry.timestamp);
        let entry_refs: Vec<&RequestEntry> = entries.iter().collect();
        let stats = self.build_stats_from_refs(
            &entry_refs,
            coverage_start,
            coverage_end,
            StatsMode::Historical,
        );

        StatsSnapshot {
            mode: StatsMode::Historical,
            stats,
            coverage_start,
            coverage_end,
        }
    }

    fn detect_anomalies(&self, entry: &mut RequestEntry) {
        // Slow TTFT
        if let Some(ttft) = entry.ttft_ms {
            if ttft > self.slow_ttft_threshold * 1000.0 {
                let sev = if ttft > 60000.0 {
                    Severity::Critical
                } else if ttft > 30000.0 {
                    Severity::Error
                } else {
                    Severity::Warning
                };
                entry.anomalies.push(Anomaly {
                    kind: AnomalyKind::SlowTtft,
                    message: format!(
                        "TTFT {:.1}s exceeds threshold {:.1}s",
                        ttft / 1000.0,
                        self.slow_ttft_threshold
                    ),
                    severity: sev,
                });
            }
        }

        // Stalls
        for stall in &entry.stalls {
            let sev = if stall.duration_s > 60.0 {
                Severity::Critical
            } else if stall.duration_s > 30.0 {
                Severity::Error
            } else {
                Severity::Warning
            };
            entry.anomalies.push(Anomaly {
                kind: AnomalyKind::StreamStall,
                message: format!("Stream stall: {:.1}s gap", stall.duration_s),
                severity: sev,
            });
        }

        // Timeout
        if matches!(entry.status, RequestStatus::Timeout) {
            entry.anomalies.push(Anomaly {
                kind: AnomalyKind::Timeout,
                message: format!("Request timed out after {:.1}s", entry.duration_ms / 1000.0),
                severity: Severity::Critical,
            });
        }

        // Connection error
        if matches!(entry.status, RequestStatus::ConnectionError) {
            entry.anomalies.push(Anomaly {
                kind: AnomalyKind::ConnectionFailure,
                message: entry.error.clone().unwrap_or_default(),
                severity: Severity::Critical,
            });
        }

        // Rate limited (429)
        if matches!(entry.status, RequestStatus::ClientError(429)) {
            entry.anomalies.push(Anomaly {
                kind: AnomalyKind::RateLimited,
                message: "Rate limited (429)".into(),
                severity: Severity::Error,
            });
        }

        // Gateway errors (502, 503, 504)
        if matches!(entry.status, RequestStatus::ServerError(502..=504)) {
            entry.anomalies.push(Anomaly {
                kind: AnomalyKind::GatewayError,
                message: format!("Gateway error: {}", entry.status.code_str()),
                severity: Severity::Error,
            });
        }

        // Model not found / forbidden
        if matches!(entry.status, RequestStatus::ClientError(403 | 404)) {
            if let Some(err) = &entry.error {
                if err.contains("Model") || err.contains("model") {
                    entry.anomalies.push(Anomaly {
                        kind: AnomalyKind::ModelError,
                        message: format!(
                            "Model error: {}",
                            err.chars().take(100).collect::<String>()
                        ),
                        severity: Severity::Error,
                    });
                }
            }
        }

        // Slow total response
        if entry.duration_ms > 120000.0 && matches!(entry.status, RequestStatus::Success(_)) {
            entry.anomalies.push(Anomaly {
                kind: AnomalyKind::SlowResponse,
                message: format!("Response took {:.1}s", entry.duration_ms / 1000.0),
                severity: Severity::Warning,
            });
        }

        // Check for repeated errors (look at recent entries)
        let entries = self.entries.read();
        let recent_errors = entries
            .iter()
            .rev()
            .take(10)
            .filter(|e| e.status.is_error())
            .count();
        if recent_errors >= 5 && entry.status.is_error() {
            entry.anomalies.push(Anomaly {
                kind: AnomalyKind::RepeatedErrors,
                message: format!("{} errors in last 10 requests", recent_errors + 1),
                severity: Severity::Critical,
            });
        }
    }

    pub fn get_live_stats(&self) -> LiveStats {
        let entries = self.entries.read();
        let start_time = Some(*self.start_time.read());
        let end_time = Some(Utc::now());
        let entry_refs: Vec<&RequestEntry> = entries.iter().collect();

        self.build_stats_from_refs(&entry_refs, start_time, end_time, StatsMode::Live)
    }

    pub fn get_entries(
        &self,
        limit: usize,
        offset: usize,
        filter: &EntryFilter,
        sort_by: Option<&str>,
        sort_order: Option<&str>,
    ) -> Vec<RequestEntry> {
        let (where_clause, params) = build_entry_where_clause(filter);

        let order_col = match sort_by {
            Some("ttft_ms") => "ttft_ms",
            Some("duration_ms") => "duration_ms",
            Some("status_code") => "status_code",
            Some("model") => "model",
            Some("input_tokens") => "input_tokens",
            Some("output_tokens") => "output_tokens",
            _ => "timestamp_ms",
        };
        let order_dir = match sort_order {
            Some("asc") => "ASC",
            _ => "DESC",
        };

        let query = format!(
            "SELECT entry_json FROM requests{where_clause} ORDER BY {order_col} {order_dir} LIMIT ? OFFSET ?"
        );

        let mut params = params;
        params.push(Value::Integer(limit.min(i64::MAX as usize) as i64));
        params.push(Value::Integer(offset.min(i64::MAX as usize) as i64));

        self.load_entries_from_db(&query, params)
    }

    pub fn get_entries_with_anomaly_focus(
        &self,
        limit: usize,
        offset: usize,
        filter: &EntryFilter,
        anomaly_ts_ms: i64,
        window_ms: i64,
    ) -> AnomalyFocusedEntries {
        let safe_window_ms = window_ms.max(0);
        let lower_bound = anomaly_ts_ms.saturating_sub(safe_window_ms);
        let upper_bound = anomaly_ts_ms.saturating_add(safe_window_ms);

        let (where_clause, base_params) = build_entry_where_clause(filter);
        let anomaly_where_prefix = if where_clause.is_empty() {
            " WHERE "
        } else {
            " AND "
        };

        // Preselection intentionally uses the first ranked row before pagination offset
        // so nearest-request selection is stable across pages.
        let preselect_query = format!(
            "SELECT id FROM requests{where_clause}{anomaly_where_prefix}timestamp_ms BETWEEN ? AND ? \
             ORDER BY ABS(timestamp_ms - ?) ASC, timestamp_ms DESC, id DESC LIMIT 1 OFFSET 0"
        );

        let mut preselect_params = base_params.clone();
        preselect_params.push(Value::Integer(lower_bound));
        preselect_params.push(Value::Integer(upper_bound));
        preselect_params.push(Value::Integer(anomaly_ts_ms));

        let conn = self.db.lock();

        let preselected_request_id = conn.prepare(&preselect_query).ok().and_then(|mut stmt| {
            stmt.query_row(params_from_iter(preselect_params), |row| {
                row.get::<_, String>(0)
            })
            .ok()
        });

        let query = format!(
            "SELECT entry_json, ABS(timestamp_ms - ?) AS distance_ms FROM requests{where_clause}{anomaly_where_prefix}timestamp_ms BETWEEN ? AND ? \
             ORDER BY distance_ms ASC, timestamp_ms DESC, id DESC LIMIT ? OFFSET ?"
        );

        let mut params = base_params;
        params.insert(0, Value::Integer(anomaly_ts_ms));
        params.push(Value::Integer(lower_bound));
        params.push(Value::Integer(upper_bound));
        params.push(Value::Integer(limit.min(i64::MAX as usize) as i64));
        params.push(Value::Integer(offset.min(i64::MAX as usize) as i64));

        let mut stmt = match conn.prepare(&query) {
            Ok(stmt) => stmt,
            Err(err) => {
                eprintln!("  Failed to prepare anomaly-focused SQLite history query: {err}");
                return AnomalyFocusedEntries {
                    entries: Vec::new(),
                    preselected_request_id,
                };
            }
        };

        let rows = match stmt.query_map(params_from_iter(params), |row| {
            let entry_json: String = row.get(0)?;
            let distance_ms: i64 = row.get(1)?;
            Ok((entry_json, distance_ms))
        }) {
            Ok(rows) => rows,
            Err(err) => {
                eprintln!("  Failed to query anomaly-focused SQLite history: {err}");
                return AnomalyFocusedEntries {
                    entries: Vec::new(),
                    preselected_request_id,
                };
            }
        };

        let mut focused_entries = Vec::new();
        for row in rows {
            let (entry_json, distance_ms) = match row {
                Ok(value) => value,
                Err(err) => {
                    eprintln!("  Failed to read anomaly-focused SQLite history row: {err}");
                    continue;
                }
            };

            match serde_json::from_str::<RequestEntry>(&entry_json) {
                Ok(entry) => focused_entries.push(RequestEntryWithAnomalyMeta {
                    entry,
                    distance_ms: Some(distance_ms),
                    within_window: Some(true),
                }),
                Err(err) => {
                    eprintln!("  Failed to decode anomaly-focused SQLite history entry JSON: {err}")
                }
            }
        }

        AnomalyFocusedEntries {
            entries: focused_entries,
            preselected_request_id,
        }
    }

    pub fn get_recent_requests_for_correlation(&self, limit: usize) -> Vec<RequestEntry> {
        self.get_entries(limit, 0, &EntryFilter::default(), None, None)
    }

    pub fn get_sessions(&self) -> Vec<SessionSummary> {
        let entries = self.load_entries_from_db(
            "SELECT entry_json FROM requests ORDER BY timestamp_ms ASC",
            Vec::new(),
        );
        let mut sessions = std::collections::HashMap::<String, Vec<&RequestEntry>>::new();

        for e in entries.iter() {
            let sid = e.session_id.clone().unwrap_or_else(|| "unknown".into());
            sessions.entry(sid).or_default().push(e);
        }

        let mut result: Vec<SessionSummary> = sessions
            .into_iter()
            .map(|(id, reqs)| {
                let errors = reqs.iter().filter(|r| r.status.is_error()).count() as u64;
                let stalls: u64 = reqs.iter().map(|r| r.stalls.len() as u64).sum();
                let ttfts: Vec<f64> = reqs.iter().filter_map(|r| r.ttft_ms).collect();
                let avg_ttft = if ttfts.is_empty() {
                    0.0
                } else {
                    ttfts.iter().sum::<f64>() / ttfts.len() as f64
                };
                let input_tokens: u64 = reqs.iter().filter_map(|r| r.input_tokens).sum();
                let output_tokens: u64 = reqs.iter().filter_map(|r| r.output_tokens).sum();

                SessionSummary {
                    session_id: id,
                    request_count: reqs.len() as u64,
                    error_count: errors,
                    stall_count: stalls,
                    avg_ttft_ms: avg_ttft,
                    total_input_tokens: input_tokens,
                    total_output_tokens: output_tokens,
                    first_request: reqs.first().map(|r| r.timestamp),
                    last_request: reqs.last().map(|r| r.timestamp),
                    model: reqs.last().map(|r| r.model.clone()).unwrap_or_default(),
                }
            })
            .collect();

        result.sort_by(|a, b| b.last_request.cmp(&a.last_request));
        result
    }

    fn collect_session_project_files(
        &self,
        session_id: &str,
        project_path_filter: Option<&str>,
    ) -> (Vec<(String, String, String)>, bool) {
        let projects_root = self.claude_dir.join("projects");
        let mut matched_files = Vec::new();
        let mut bounds_reached = false;

        let mut project_roots = std::fs::read_dir(&projects_root)
            .ok()
            .into_iter()
            .flat_map(|entries| entries.flatten())
            .filter_map(|entry| {
                let path = entry.path();
                if !path.is_dir() {
                    return None;
                }

                let name = path.file_name()?.to_str()?.to_string();
                Some((name, path))
            })
            .collect::<Vec<_>>();

        project_roots.sort_by(|a, b| a.0.cmp(&b.0));

        if let Some(filter) = project_path_filter {
            project_roots.retain(|(name, _)| name == filter);
        }

        let mut roots_scanned = 0usize;
        let mut files_scanned = 0usize;

        for (project_name, project_path) in project_roots {
            if roots_scanned >= MAX_SESSION_PROJECT_ROOTS_SCANNED {
                bounds_reached = true;
                break;
            }
            roots_scanned += 1;

            let mut stack = vec![(project_path.clone(), 0usize)];
            let mut project_files = Vec::new();

            while let Some((dir, depth)) = stack.pop() {
                if depth > MAX_SESSION_PROJECT_SCAN_DEPTH {
                    bounds_reached = true;
                    continue;
                }

                let mut dir_entries = match std::fs::read_dir(&dir) {
                    Ok(entries) => entries
                        .flatten()
                        .map(|entry| entry.path())
                        .collect::<Vec<_>>(),
                    Err(_) => continue,
                };
                dir_entries.sort_by(|a, b| {
                    normalize_source_path(&a.to_string_lossy())
                        .cmp(&normalize_source_path(&b.to_string_lossy()))
                });

                let mut subdirs = Vec::new();
                for path in dir_entries {
                    if path.is_dir() {
                        if depth >= MAX_SESSION_PROJECT_SCAN_DEPTH {
                            bounds_reached = true;
                        } else {
                            subdirs.push(path);
                        }
                        continue;
                    }

                    let extension = path.extension().and_then(|ext| ext.to_str());
                    if extension != Some("json") && extension != Some("jsonl") {
                        continue;
                    }

                    project_files.push(path);
                }

                for subdir in subdirs.into_iter().rev() {
                    stack.push((subdir, depth + 1));
                }
            }

            project_files.sort_by(|a, b| {
                normalize_source_path(&a.to_string_lossy())
                    .cmp(&normalize_source_path(&b.to_string_lossy()))
            });

            for file_path in project_files {
                if files_scanned >= MAX_SESSION_FILES_SCANNED {
                    bounds_reached = true;
                    break;
                }
                files_scanned += 1;

                let Ok(metadata) = std::fs::metadata(&file_path) else {
                    continue;
                };
                if metadata.len() > MAX_SESSION_ARTIFACT_FILE_BYTES {
                    continue;
                }

                let Ok(text) = std::fs::read_to_string(&file_path) else {
                    continue;
                };

                let extension = file_path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .unwrap_or_default();

                let has_session = if extension == "jsonl" {
                    text.lines().any(|line| {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            return false;
                        }

                        let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                            return false;
                        };

                        extract_session_id_value(&json) == Some(session_id)
                    })
                } else {
                    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
                        continue;
                    };

                    extract_session_id_value(&json) == Some(session_id)
                };

                if !has_session {
                    continue;
                }

                let source_path = normalize_source_path(&file_path.to_string_lossy());
                matched_files.push((project_name.clone(), source_path, text));
            }

            if bounds_reached && files_scanned >= MAX_SESSION_FILES_SCANNED {
                break;
            }
        }

        matched_files.sort_by(|a, b| a.1.cmp(&b.1));
        (matched_files, bounds_reached)
    }

    pub fn get_session_details(
        &self,
        session_id: &str,
        project_path: Option<&str>,
        limit: usize,
        include_full_text: bool,
    ) -> SessionDetailsResponse {
        let cap = limit.min(i64::MAX as usize);

        let mut request_params = vec![Value::Text(session_id.to_string())];
        let mut requests = self.load_entries_from_db(
            "SELECT entry_json FROM requests WHERE session_id = ? ORDER BY timestamp_ms DESC, id DESC",
            std::mem::take(&mut request_params),
        );
        requests.sort_by(|a, b| {
            b.timestamp
                .timestamp_millis()
                .cmp(&a.timestamp.timestamp_millis())
                .then_with(|| b.id.cmp(&a.id))
        });

        let mut local_events = self.get_local_events(Some(session_id), usize::MAX);

        let (matched_project_files, conversation_scan_truncated) =
            self.collect_session_project_files(session_id, project_path);

        let mut project_paths_set = std::collections::BTreeSet::new();
        let mut conversation_items = Vec::new();

        for (project_name, source_path, source_text) in matched_project_files {
            project_paths_set.insert(project_name);
            conversation_items.extend(extract_conversation_items_from_source_text(
                session_id,
                &source_path,
                &source_text,
            ));
        }

        if !include_full_text {
            for item in &mut conversation_items {
                item.full_text = None;
                item.expandable = false;
            }
        }

        conversation_items.sort_by(|a, b| {
            b.timestamp_ms
                .cmp(&a.timestamp_ms)
                .then_with(|| a.id.cmp(&b.id))
        });

        local_events.sort_by(|a, b| {
            b.event_time_ms
                .cmp(&a.event_time_ms)
                .then_with(|| a.id.cmp(&b.id))
        });

        let project_paths: Vec<String> = project_paths_set.into_iter().collect();
        let selected_project_path = project_path
            .map(|value| value.to_string())
            .filter(|value| project_paths.iter().any(|path| path == value))
            .or_else(|| {
                if project_paths.len() == 1 {
                    project_paths.first().cloned()
                } else {
                    None
                }
            });

        let proxy_request_count_total = requests.len() as u64;
        let proxy_error_count_total = requests
            .iter()
            .filter(|entry| entry.status.is_error())
            .count() as u64;
        let proxy_stall_count_total = requests.iter().map(|entry| entry.stalls.len() as u64).sum();
        let local_event_count_total = local_events.len() as u64;
        let conversation_message_count_total = conversation_items.len() as u64;

        let last_proxy_activity_ms = requests
            .iter()
            .map(|entry| entry.timestamp.timestamp_millis())
            .max();
        let last_local_activity_ms = local_events
            .iter()
            .map(|event| event.event_time_ms)
            .chain(conversation_items.iter().map(|item| item.timestamp_ms))
            .max();

        let has_proxy = proxy_request_count_total > 0;
        let has_local = local_event_count_total > 0 || conversation_message_count_total > 0;
        let presence = match (has_proxy, has_local) {
            (false, false) => SessionPresence::None,
            (true, false) => SessionPresence::ProxyOnly,
            (false, true) => SessionPresence::LocalOnly,
            (true, true) => SessionPresence::Both,
        };

        let mut request_summaries: Vec<SessionRequestSummary> = requests
            .into_iter()
            .map(|entry| SessionRequestSummary {
                id: entry.id,
                timestamp_ms: entry.timestamp.timestamp_millis(),
                status: Some(entry.status.code_str().to_string()),
                ttft_ms: entry.ttft_ms,
                duration_ms: Some(entry.duration_ms),
                path: Some(truncate_chars(&entry.path, MAX_REQUEST_PATH_CHARS)),
                model: Some(truncate_chars(&entry.model, MAX_REQUEST_MODEL_CHARS)),
            })
            .collect();

        request_summaries.sort_by(|a, b| {
            b.timestamp_ms
                .cmp(&a.timestamp_ms)
                .then_with(|| b.id.cmp(&a.id))
        });

        let mut timeline = Vec::new();

        for request in &request_summaries {
            let label = request
                .path
                .as_deref()
                .map(|path| format!("request {path}"))
                .unwrap_or_else(|| "request".to_string());
            timeline.push(SessionTimelineItem {
                id: request.id.clone(),
                timestamp_ms: request.timestamp_ms,
                kind: "request".to_string(),
                label: truncate_chars(&label, MAX_TIMELINE_LABEL_CHARS),
                request_id: Some(request.id.clone()),
                local_event_id: None,
                conversation_id: None,
            });
        }

        for event in &local_events {
            timeline.push(SessionTimelineItem {
                id: event.id.clone(),
                timestamp_ms: event.event_time_ms,
                kind: "local_event".to_string(),
                label: truncate_chars(&event.event_kind, MAX_TIMELINE_LABEL_CHARS),
                request_id: None,
                local_event_id: Some(event.id.clone()),
                conversation_id: None,
            });
        }

        for conversation in &conversation_items {
            timeline.push(SessionTimelineItem {
                id: conversation.id.clone(),
                timestamp_ms: conversation.timestamp_ms,
                kind: "conversation".to_string(),
                label: truncate_chars(
                    &format!("{} message", conversation.role),
                    MAX_TIMELINE_LABEL_CHARS,
                ),
                request_id: None,
                local_event_id: None,
                conversation_id: Some(conversation.id.clone()),
            });
        }

        timeline.sort_by(|a, b| {
            b.timestamp_ms
                .cmp(&a.timestamp_ms)
                .then_with(|| a.kind.cmp(&b.kind))
                .then_with(|| a.id.cmp(&b.id))
        });

        let truncated_sections = SessionTruncatedSections {
            requests: request_summaries.len() > cap,
            timeline: timeline.len() > cap,
            conversation: conversation_items.len() > cap || conversation_scan_truncated,
            full_text: false,
        };

        if request_summaries.len() > cap {
            request_summaries.truncate(cap);
        }
        if timeline.len() > cap {
            timeline.truncate(cap);
        }
        if conversation_items.len() > cap {
            conversation_items.truncate(cap);
        }

        let payload_cap = if include_full_text {
            SESSION_DETAILS_PAYLOAD_CAP_WITH_FULL_TEXT_BYTES
        } else {
            SESSION_DETAILS_PAYLOAD_CAP_NO_FULL_TEXT_BYTES
        };

        let mut response = SessionDetailsResponse {
            session_id: session_id.to_string(),
            presence,
            project_paths,
            selected_project_path,
            summary: SessionDetailsSummary {
                proxy_request_count_total,
                proxy_error_count_total,
                proxy_stall_count_total,
                local_event_count_total,
                conversation_message_count_total,
                last_proxy_activity_ms,
                last_local_activity_ms,
            },
            requests: request_summaries,
            timeline,
            conversation: conversation_items,
            truncated_sections,
        };

        enforce_session_details_payload_cap(&mut response, payload_cap, include_full_text);

        response
    }

    pub fn get_session_graph(&self, session_id: &str, limit: usize) -> Option<SessionGraph> {
        let cap = limit.clamp(1, 1000);

        let requests = self.get_entries(
            cap,
            0,
            &EntryFilter {
                session_id: Some(session_id.to_string()),
                ..EntryFilter::default()
            },
            None,
            None,
        );

        if requests.is_empty() {
            return None;
        }

        let local_events = self.get_local_events(Some(session_id), cap);

        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        for request in &requests {
            nodes.push(SessionGraphNode {
                id: request.id.clone(),
                kind: "request".into(),
                label: format!("{} {}", request.method, request.path),
                timestamp_ms: request.timestamp.timestamp_millis(),
            });
        }

        for event in &local_events {
            nodes.push(SessionGraphNode {
                id: event.id.clone(),
                kind: "local_event".into(),
                label: event.event_kind.clone(),
                timestamp_ms: event.event_time_ms,
            });
        }

        for request in &requests {
            let links = self.get_correlations_for_request(&request.id, cap);
            let explanations = self.get_explanations_for_request(&request.id, cap);

            for explanation in explanations {
                nodes.push(SessionGraphNode {
                    id: explanation.id.clone(),
                    kind: "explanation".into(),
                    label: explanation.summary.clone(),
                    timestamp_ms: explanation.created_at_ms,
                });
                edges.push(SessionGraphEdge {
                    from: request.id.clone(),
                    to: explanation.id,
                    kind: "explains".into(),
                });
            }

            for link in links {
                edges.push(SessionGraphEdge {
                    from: request.id.clone(),
                    to: link.local_event_id,
                    kind: "correlates".into(),
                });
            }
        }

        Some(SessionGraph {
            session_id: session_id.to_string(),
            nodes,
            edges,
        })
    }

    fn load_entries_from_db(&self, query: &str, params: Vec<Value>) -> Vec<RequestEntry> {
        let conn = self.db.lock();
        let mut stmt = match conn.prepare(query) {
            Ok(stmt) => stmt,
            Err(err) => {
                eprintln!("  Failed to prepare SQLite history query: {err}");
                return Vec::new();
            }
        };

        let rows = match stmt.query_map(params_from_iter(params), |row| row.get::<_, String>(0)) {
            Ok(rows) => rows,
            Err(err) => {
                eprintln!("  Failed to query SQLite history: {err}");
                return Vec::new();
            }
        };

        let mut entries = Vec::new();
        for row in rows {
            let entry_json = match row {
                Ok(value) => value,
                Err(err) => {
                    eprintln!("  Failed to read SQLite history row: {err}");
                    continue;
                }
            };

            match serde_json::from_str::<RequestEntry>(&entry_json) {
                Ok(entry) => entries.push(entry),
                Err(err) => eprintln!("  Failed to decode SQLite history entry JSON: {err}"),
            }
        }

        entries
    }

    fn build_stats_from_refs(
        &self,
        entries: &[&RequestEntry],
        coverage_start: Option<DateTime<Utc>>,
        coverage_end: Option<DateTime<Utc>>,
        mode: StatsMode,
    ) -> LiveStats {
        let total = entries.len() as u64;
        let coverage_end = coverage_end.unwrap_or_else(Utc::now);
        let uptime = match mode {
            StatsMode::Live => coverage_start
                .map(|start| (coverage_end - start).num_seconds().max(0) as f64)
                .unwrap_or(0.0),
            StatsMode::Historical => match (coverage_start, coverage_end) {
                (Some(start), end) => (end - start).num_seconds().max(0) as f64,
                (None, _) => 0.0,
            },
        };

        if total == 0 {
            return LiveStats {
                total_requests: 0,
                total_errors: 0,
                success_rate: 100.0,
                avg_ttft_ms: 0.0,
                median_ttft_ms: 0.0,
                p95_ttft_ms: 0.0,
                p99_ttft_ms: 0.0,
                worst_ttft_ms: 0.0,
                avg_duration_ms: 0.0,
                total_stalls: 0,
                total_stall_time_s: 0.0,
                total_timeouts: 0,
                total_connection_errors: 0,
                total_4xx: 0,
                total_5xx: 0,
                total_input_tokens: 0,
                total_output_tokens: 0,
                health_score: 100.0,
                health_label: "HEALTHY".into(),
                uptime_s: uptime,
                requests_per_minute: 0.0,
                errors_last_5min: 0,
                model_breakdown: vec![],
                error_breakdown: vec![],
                recent_anomalies: vec![],
                ttft_timeseries: vec![],
                error_timeseries: vec![],
                request_timeseries: vec![],
            };
        }

        let mut total_errors = 0u64;
        let mut total_4xx = 0u64;
        let mut total_5xx = 0u64;
        let mut total_timeouts = 0u64;
        let mut total_conn_errors = 0u64;
        let mut total_stalls = 0u64;
        let mut total_stall_time = 0.0f64;
        let mut total_input_tokens = 0u64;
        let mut total_output_tokens = 0u64;
        let mut ttfts: Vec<f64> = Vec::new();
        let mut durations: Vec<f64> = Vec::new();
        let mut model_counts = std::collections::HashMap::new();
        let mut error_counts = std::collections::HashMap::new();
        let mut all_anomalies: Vec<Anomaly> = Vec::new();

        let five_min_ago = coverage_end - chrono::Duration::minutes(5);
        let mut errors_last_5min = 0u64;

        for entry in entries {
            durations.push(entry.duration_ms);

            if let Some(ttft) = entry.ttft_ms {
                ttfts.push(ttft);
            }

            if entry.status.is_error() {
                total_errors += 1;
                *error_counts.entry(entry.status.code_str()).or_insert(0u64) += 1;
                if entry.timestamp > five_min_ago {
                    errors_last_5min += 1;
                }
            }

            match &entry.status {
                RequestStatus::ClientError(_) => total_4xx += 1,
                RequestStatus::ServerError(_) => total_5xx += 1,
                RequestStatus::Timeout => total_timeouts += 1,
                RequestStatus::ConnectionError => total_conn_errors += 1,
                _ => {}
            }

            total_stalls += entry.stalls.len() as u64;
            total_stall_time += entry
                .stalls
                .iter()
                .map(|stall| stall.duration_s)
                .sum::<f64>();
            total_input_tokens += entry.input_tokens.unwrap_or(0);
            total_output_tokens += entry.output_tokens.unwrap_or(0);

            if !entry.model.is_empty() {
                *model_counts.entry(entry.model.clone()).or_insert(0u64) += 1;
            }

            all_anomalies.extend(entry.anomalies.clone());
        }

        ttfts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let avg_ttft = if ttfts.is_empty() {
            0.0
        } else {
            ttfts.iter().sum::<f64>() / ttfts.len() as f64
        };
        let median_ttft = if ttfts.is_empty() {
            0.0
        } else {
            ttfts[ttfts.len() / 2]
        };
        let p95_ttft = if ttfts.is_empty() {
            0.0
        } else {
            ttfts[(ttfts.len() as f64 * 0.95) as usize]
        };
        let p99_ttft = if ttfts.is_empty() {
            0.0
        } else {
            ttfts[((ttfts.len() as f64 * 0.99) as usize).min(ttfts.len() - 1)]
        };
        let worst_ttft = ttfts.last().copied().unwrap_or(0.0);
        let avg_duration = durations.iter().sum::<f64>() / durations.len() as f64;
        let success_rate = ((total - total_errors) as f64 / total as f64) * 100.0;
        let rpm = if uptime > 0.0 {
            total as f64 / uptime * 60.0
        } else {
            0.0
        };

        let mut score = 100.0f64;
        score -= (total_stalls as f64 * 3.0).min(30.0);
        score -= ((100.0 - success_rate) * 2.0).min(20.0);
        score -= ((avg_ttft / 1000.0 - 3.0).max(0.0) * 5.0).min(30.0);
        score -= (total_timeouts as f64 * 10.0).min(20.0);
        score = score.max(0.0);

        let health_label = if score >= 80.0 {
            "HEALTHY"
        } else if score >= 50.0 {
            "DEGRADED"
        } else {
            "UNHEALTHY"
        }
        .to_string();

        let ttft_ts = self.build_timeseries(entries, 60, coverage_end, |entry| entry.ttft_ms);
        let error_ts = self.build_error_timeseries(entries, 60, coverage_end);
        let request_ts = self.build_count_timeseries(entries, 60, coverage_end);

        let mut model_breakdown: Vec<(String, u64)> = model_counts.into_iter().collect();
        model_breakdown.sort_by(|a, b| b.1.cmp(&a.1));

        let mut error_breakdown: Vec<(String, u64)> = error_counts.into_iter().collect();
        error_breakdown.sort_by(|a, b| b.1.cmp(&a.1));

        let recent_anomalies: Vec<Anomaly> = all_anomalies.into_iter().rev().take(20).collect();

        LiveStats {
            total_requests: total,
            total_errors,
            success_rate,
            avg_ttft_ms: avg_ttft,
            median_ttft_ms: median_ttft,
            p95_ttft_ms: p95_ttft,
            p99_ttft_ms: p99_ttft,
            worst_ttft_ms: worst_ttft,
            avg_duration_ms: avg_duration,
            total_stalls,
            total_stall_time_s: total_stall_time,
            total_timeouts,
            total_connection_errors: total_conn_errors,
            total_4xx,
            total_5xx,
            total_input_tokens,
            total_output_tokens,
            health_score: score,
            health_label,
            uptime_s: uptime,
            requests_per_minute: rpm,
            errors_last_5min,
            model_breakdown,
            error_breakdown,
            recent_anomalies,
            ttft_timeseries: ttft_ts,
            error_timeseries: error_ts,
            request_timeseries: request_ts,
        }
    }

    fn build_timeseries(
        &self,
        entries: &[&RequestEntry],
        minutes: usize,
        now: DateTime<Utc>,
        extractor: impl Fn(&RequestEntry) -> Option<f64>,
    ) -> Vec<TimeseriesPoint> {
        let mut result = Vec::with_capacity(minutes);

        for i in (0..minutes).rev() {
            let bucket_start = now - chrono::Duration::minutes(i as i64 + 1);
            let bucket_end = now - chrono::Duration::minutes(i as i64);

            let values: Vec<f64> = entries
                .iter()
                .filter(|e| e.timestamp >= bucket_start && e.timestamp < bucket_end)
                .filter_map(|e| extractor(e))
                .collect();

            let avg = if values.is_empty() {
                0.0
            } else {
                values.iter().sum::<f64>() / values.len() as f64
            };
            result.push(TimeseriesPoint {
                timestamp: bucket_end,
                value: avg,
            });
        }
        result
    }

    fn build_error_timeseries(
        &self,
        entries: &[&RequestEntry],
        minutes: usize,
        now: DateTime<Utc>,
    ) -> Vec<TimeseriesPoint> {
        let mut result = Vec::with_capacity(minutes);

        for i in (0..minutes).rev() {
            let bucket_start = now - chrono::Duration::minutes(i as i64 + 1);
            let bucket_end = now - chrono::Duration::minutes(i as i64);

            let count = entries
                .iter()
                .filter(|e| {
                    e.timestamp >= bucket_start && e.timestamp < bucket_end && e.status.is_error()
                })
                .count();

            result.push(TimeseriesPoint {
                timestamp: bucket_end,
                value: count as f64,
            });
        }
        result
    }

    fn build_count_timeseries(
        &self,
        entries: &[&RequestEntry],
        minutes: usize,
        now: DateTime<Utc>,
    ) -> Vec<TimeseriesPoint> {
        let mut result = Vec::with_capacity(minutes);

        for i in (0..minutes).rev() {
            let bucket_start = now - chrono::Duration::minutes(i as i64 + 1);
            let bucket_end = now - chrono::Duration::minutes(i as i64);

            let count = entries
                .iter()
                .filter(|e| e.timestamp >= bucket_start && e.timestamp < bucket_end)
                .count();

            result.push(TimeseriesPoint {
                timestamp: bucket_end,
                value: count as f64,
            });
        }
        result
    }

    fn initialize_database(&self) {
        let conn = self.db.lock();
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA temp_store = MEMORY;
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS requests (
                id TEXT PRIMARY KEY,
                timestamp TEXT NOT NULL,
                timestamp_ms INTEGER NOT NULL,
                session_id TEXT,
                method TEXT NOT NULL,
                path TEXT NOT NULL,
                model TEXT NOT NULL,
                stream INTEGER NOT NULL,
                status_kind TEXT NOT NULL,
                status_code INTEGER,
                duration_ms REAL NOT NULL,
                ttft_ms REAL,
                input_tokens INTEGER,
                output_tokens INTEGER,
                cache_read_tokens INTEGER,
                cache_creation_tokens INTEGER,
                thinking_tokens INTEGER,
                stop_reason TEXT,
                request_size_bytes INTEGER NOT NULL,
                response_size_bytes INTEGER NOT NULL,
                error TEXT,
                stalls_json TEXT NOT NULL,
                anomalies_json TEXT NOT NULL,
                entry_json TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_requests_timestamp_ms
                ON requests(timestamp_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_requests_session_id
                ON requests(session_id);
            CREATE INDEX IF NOT EXISTS idx_requests_model
                ON requests(model);
            CREATE INDEX IF NOT EXISTS idx_requests_status_kind_code
                ON requests(status_kind, status_code);

            CREATE TABLE IF NOT EXISTS local_events (
                id TEXT PRIMARY KEY,
                source_kind TEXT NOT NULL,
                source_path TEXT NOT NULL,
                event_time_ms INTEGER NOT NULL,
                session_hint TEXT,
                event_kind TEXT NOT NULL,
                model_hint TEXT,
                payload_policy TEXT NOT NULL,
                payload_json TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_local_events_event_time_ms
                ON local_events(event_time_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_local_events_session_hint
                ON local_events(session_hint);

            CREATE TABLE IF NOT EXISTS request_correlations (
                id TEXT PRIMARY KEY,
                request_id TEXT NOT NULL,
                local_event_id TEXT NOT NULL,
                link_type TEXT NOT NULL CHECK(link_type IN ('temporal','session_hint','config_drift','command_proximity')),
                confidence REAL NOT NULL CHECK(confidence >= 0.0 AND confidence <= 1.0),
                reason TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                FOREIGN KEY(request_id) REFERENCES requests(id),
                FOREIGN KEY(local_event_id) REFERENCES local_events(id)
            );

            CREATE INDEX IF NOT EXISTS idx_request_correlations_request_id
                ON request_correlations(request_id, created_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_request_correlations_local_event_id
                ON request_correlations(local_event_id);

            CREATE TABLE IF NOT EXISTS config_snapshots (
                id TEXT PRIMARY KEY,
                source_path TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                captured_at_ms INTEGER NOT NULL,
                payload_policy TEXT NOT NULL,
                payload_json TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_config_snapshots_source_path
                ON config_snapshots(source_path, captured_at_ms DESC);

            CREATE TABLE IF NOT EXISTS settings_history (
                id TEXT PRIMARY KEY,
                saved_at_ms INTEGER NOT NULL,
                content_hash TEXT NOT NULL,
                settings_json TEXT NOT NULL,
                source TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_settings_history_saved_at_ms
                ON settings_history(saved_at_ms DESC);

            CREATE TABLE IF NOT EXISTS ingestion_checkpoints (
                source_kind TEXT PRIMARY KEY,
                checkpoint TEXT NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS explanations (
                id TEXT PRIMARY KEY,
                request_id TEXT NOT NULL,
                anomaly_kind TEXT NOT NULL,
                rank INTEGER NOT NULL CHECK(rank >= 0),
                confidence REAL NOT NULL CHECK(confidence >= 0.0 AND confidence <= 1.0),
                summary TEXT NOT NULL,
                evidence_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                FOREIGN KEY(request_id) REFERENCES requests(id)
            );

            CREATE INDEX IF NOT EXISTS idx_explanations_request_id
                ON explanations(request_id, rank);

            CREATE TABLE IF NOT EXISTS request_bodies (
                request_id TEXT PRIMARY KEY,
                request_body TEXT NOT NULL,
                response_body TEXT NOT NULL,
                truncated INTEGER NOT NULL DEFAULT 0,
                FOREIGN KEY(request_id) REFERENCES requests(id) ON DELETE CASCADE
            );
            ",
        )
        .expect("Failed to initialize SQLite schema");

        if let Err(err) = conn.execute("ALTER TABLE requests ADD COLUMN stop_reason TEXT", []) {
            if !err.to_string().contains("duplicate column name") {
                panic!("Failed to migrate requests.stop_reason column: {err}");
            }
        }
    }

    fn write_to_db(&self, entry: &RequestEntry) {
        let entry_json = match serde_json::to_string(entry) {
            Ok(json) => json,
            Err(err) => {
                eprintln!("  Failed to serialize request entry for SQLite: {err}");
                return;
            }
        };

        let stalls_json = match serde_json::to_string(&entry.stalls) {
            Ok(json) => json,
            Err(err) => {
                eprintln!("  Failed to serialize stalls for SQLite: {err}");
                return;
            }
        };

        let anomalies_json = match serde_json::to_string(&entry.anomalies) {
            Ok(json) => json,
            Err(err) => {
                eprintln!("  Failed to serialize anomalies for SQLite: {err}");
                return;
            }
        };

        let (status_kind, status_code) = status_parts(&entry.status);
        let conn = self.db.lock();

        if let Err(err) = conn.execute(
            "
            INSERT OR REPLACE INTO requests (
                id,
                timestamp,
                timestamp_ms,
                session_id,
                method,
                path,
                model,
                stream,
                status_kind,
                status_code,
                duration_ms,
                ttft_ms,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_creation_tokens,
                thinking_tokens,
                stop_reason,
                request_size_bytes,
                response_size_bytes,
                error,
                stalls_json,
                anomalies_json,
                entry_json
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24
            )
            ",
            params![
                entry.id,
                entry.timestamp.to_rfc3339(),
                entry.timestamp.timestamp_millis(),
                entry.session_id,
                entry.method,
                entry.path,
                entry.model,
                entry.stream,
                status_kind,
                status_code,
                entry.duration_ms,
                entry.ttft_ms,
                opt_u64_to_i64(entry.input_tokens),
                opt_u64_to_i64(entry.output_tokens),
                opt_u64_to_i64(entry.cache_read_tokens),
                opt_u64_to_i64(entry.cache_creation_tokens),
                opt_u64_to_i64(entry.thinking_tokens),
                entry.stop_reason,
                entry.request_size_bytes as i64,
                entry.response_size_bytes as i64,
                entry.error,
                stalls_json,
                anomalies_json,
                entry_json,
            ],
        ) {
            eprintln!("  Failed to write request entry to SQLite: {err}");
        }
    }

    pub fn upsert_local_event(&self, event: &LocalEvent) -> bool {
        let payload_json = match serde_json::to_string(&event.payload_json) {
            Ok(value) => value,
            Err(err) => {
                eprintln!("  Failed to serialize local event payload for SQLite: {err}");
                return false;
            }
        };

        let conn = self.db.lock();
        if let Err(err) = conn.execute(
            "
            INSERT INTO local_events (
                id,
                source_kind,
                source_path,
                event_time_ms,
                session_hint,
                event_kind,
                model_hint,
                payload_policy,
                payload_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(id) DO UPDATE SET
                source_kind = excluded.source_kind,
                source_path = excluded.source_path,
                event_time_ms = excluded.event_time_ms,
                session_hint = excluded.session_hint,
                event_kind = excluded.event_kind,
                model_hint = excluded.model_hint,
                payload_policy = excluded.payload_policy,
                payload_json = excluded.payload_json
",
            params![
                event.id,
                event.source_kind.as_str(),
                event.source_path,
                event.event_time_ms,
                event.session_hint,
                event.event_kind,
                event.model_hint,
                match event.payload_policy {
                    PayloadPolicy::MetadataOnly => "metadata_only",
                    PayloadPolicy::Redacted => "redacted",
                    PayloadPolicy::Full => "full",
                },
                payload_json,
            ],
        ) {
            eprintln!("  Failed to upsert local event in SQLite: {err}");
            return false;
        }

        true
    }

    pub fn get_local_events(&self, session_hint: Option<&str>, limit: usize) -> Vec<LocalEvent> {
        let conn = self.db.lock();
        let limit = limit.min(i64::MAX as usize) as i64;

        let mut events = Vec::new();
        if let Some(session_hint) = session_hint {
            let mut stmt = match conn.prepare(
                "
                SELECT id, source_kind, source_path, event_time_ms, session_hint, event_kind, model_hint, payload_policy, payload_json
                FROM local_events
                WHERE session_hint = ?1
                ORDER BY event_time_ms DESC
                LIMIT ?2
                ",
            ) {
                Ok(stmt) => stmt,
                Err(err) => {
                    eprintln!("  Failed to prepare local events query: {err}");
                    return events;
                }
            };

            let rows = match stmt.query_map(params![session_hint, limit], |row| {
                let source_kind: String = row.get(1)?;
                let payload_policy: String = row.get(7)?;
                let payload_json_text: String = row.get(8)?;

                Ok(LocalEvent {
                    id: row.get(0)?,
                    source_kind: SourceKind::from_str(&source_kind).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
                        )
                    })?,
                    source_path: row.get(2)?,
                    event_time_ms: row.get(3)?,
                    session_hint: row.get(4)?,
                    event_kind: row.get(5)?,
                    model_hint: row.get(6)?,
                    payload_policy: PayloadPolicy::from_str(&payload_policy).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            7,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
                        )
                    })?,
                    payload_json: serde_json::from_str(&payload_json_text).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            8,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                })
            }) {
                Ok(rows) => rows,
                Err(err) => {
                    eprintln!("  Failed to query local events: {err}");
                    return events;
                }
            };

            for row in rows {
                match row {
                    Ok(event) => events.push(event),
                    Err(err) => eprintln!("  Failed to decode local event row: {err}"),
                }
            }
        } else {
            let mut stmt = match conn.prepare(
                "
                SELECT id, source_kind, source_path, event_time_ms, session_hint, event_kind, model_hint, payload_policy, payload_json
                FROM local_events
                ORDER BY event_time_ms DESC
                LIMIT ?1
                ",
            ) {
                Ok(stmt) => stmt,
                Err(err) => {
                    eprintln!("  Failed to prepare local events query: {err}");
                    return events;
                }
            };

            let rows = match stmt.query_map(params![limit], |row| {
                let source_kind: String = row.get(1)?;
                let payload_policy: String = row.get(7)?;
                let payload_json_text: String = row.get(8)?;

                Ok(LocalEvent {
                    id: row.get(0)?,
                    source_kind: SourceKind::from_str(&source_kind).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
                        )
                    })?,
                    source_path: row.get(2)?,
                    event_time_ms: row.get(3)?,
                    session_hint: row.get(4)?,
                    event_kind: row.get(5)?,
                    model_hint: row.get(6)?,
                    payload_policy: PayloadPolicy::from_str(&payload_policy).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            7,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
                        )
                    })?,
                    payload_json: serde_json::from_str(&payload_json_text).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            8,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                })
            }) {
                Ok(rows) => rows,
                Err(err) => {
                    eprintln!("  Failed to query local events: {err}");
                    return events;
                }
            };

            for row in rows {
                match row {
                    Ok(event) => events.push(event),
                    Err(err) => eprintln!("  Failed to decode local event row: {err}"),
                }
            }
        }

        events
    }

    pub fn get_recent_local_events_for_correlation(&self, limit: usize) -> Vec<LocalEvent> {
        self.get_local_events(None, limit)
    }

    pub fn get_local_events_for_request_correlations(
        &self,
        correlations: &[RequestCorrelation],
    ) -> Vec<LocalEvent> {
        if correlations.is_empty() {
            return Vec::new();
        }

        let conn = self.db.lock();
        let mut events = Vec::new();

        let mut stmt = match conn.prepare(
            "
            SELECT id, source_kind, source_path, event_time_ms, session_hint, event_kind, model_hint, payload_policy, payload_json
            FROM local_events
            WHERE id = ?1
            ",
        ) {
            Ok(stmt) => stmt,
            Err(err) => {
                eprintln!("  Failed to prepare local event by id query: {err}");
                return events;
            }
        };

        for correlation in correlations {
            let row = stmt.query_row(params![&correlation.local_event_id], |row| {
                let source_kind: String = row.get(1)?;
                let payload_policy: String = row.get(7)?;
                let payload_json_text: String = row.get(8)?;

                Ok(LocalEvent {
                    id: row.get(0)?,
                    source_kind: SourceKind::from_str(&source_kind).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
                        )
                    })?,
                    source_path: row.get(2)?,
                    event_time_ms: row.get(3)?,
                    session_hint: row.get(4)?,
                    event_kind: row.get(5)?,
                    model_hint: row.get(6)?,
                    payload_policy: PayloadPolicy::from_str(&payload_policy).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            7,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
                        )
                    })?,
                    payload_json: serde_json::from_str(&payload_json_text).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            8,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                })
            });

            match row {
                Ok(event) => events.push(event),
                Err(rusqlite::Error::QueryReturnedNoRows) => {}
                Err(err) => eprintln!("  Failed to load local event for correlation: {err}"),
            }
        }

        events
    }

    #[cfg(test)]
    pub fn upsert_request_correlation(&self, correlation: &RequestCorrelation) {
        let conn = self.db.lock();
        if let Err(err) = conn.execute(
            "
            INSERT INTO request_correlations (
                id,
                request_id,
                local_event_id,
                link_type,
                confidence,
                reason,
                created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(id) DO UPDATE SET
                request_id = excluded.request_id,
                local_event_id = excluded.local_event_id,
                link_type = excluded.link_type,
                confidence = excluded.confidence,
                reason = excluded.reason,
                created_at_ms = excluded.created_at_ms
            ",
            params![
                correlation.id,
                correlation.request_id,
                correlation.local_event_id,
                correlation.link_type.as_str(),
                correlation.confidence,
                correlation.reason,
                correlation.created_at_ms,
            ],
        ) {
            eprintln!("  Failed to upsert request correlation in SQLite: {err}");
        }
    }

    pub fn replace_correlations_for_request(
        &self,
        request_id: &str,
        correlations: &[RequestCorrelation],
    ) {
        let mut conn = self.db.lock();
        let tx = match conn.transaction() {
            Ok(tx) => tx,
            Err(err) => {
                eprintln!("  Failed to start correlation replacement transaction: {err}");
                return;
            }
        };

        if let Err(err) = tx.execute(
            "DELETE FROM request_correlations WHERE request_id = ?1",
            params![request_id],
        ) {
            eprintln!("  Failed to clear existing request correlations: {err}");
            return;
        }

        for correlation in correlations {
            if correlation.request_id != request_id {
                continue;
            }

            if let Err(err) = tx.execute(
                "
                INSERT INTO request_correlations (
                    id,
                    request_id,
                    local_event_id,
                    link_type,
                    confidence,
                    reason,
                    created_at_ms
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ",
                params![
                    correlation.id,
                    correlation.request_id,
                    correlation.local_event_id,
                    correlation.link_type.as_str(),
                    correlation.confidence,
                    correlation.reason,
                    correlation.created_at_ms,
                ],
            ) {
                eprintln!("  Failed to insert replaced request correlation: {err}");
                return;
            }
        }

        if let Err(err) = tx.commit() {
            eprintln!("  Failed to commit correlation replacement transaction: {err}");
        }
    }

    pub fn get_correlations_for_request(
        &self,
        request_id: &str,
        limit: usize,
    ) -> Vec<RequestCorrelation> {
        let conn = self.db.lock();
        let limit = limit.min(i64::MAX as usize) as i64;
        let mut correlations = Vec::new();

        let mut stmt = match conn.prepare(
            "
            SELECT id, request_id, local_event_id, link_type, confidence, reason, created_at_ms
            FROM request_correlations
            WHERE request_id = ?1
            ORDER BY created_at_ms DESC
            LIMIT ?2
            ",
        ) {
            Ok(stmt) => stmt,
            Err(err) => {
                eprintln!("  Failed to prepare request correlations query: {err}");
                return correlations;
            }
        };

        let rows = match stmt.query_map(params![request_id, limit], |row| {
            let link_type: String = row.get(3)?;

            Ok(RequestCorrelation {
                id: row.get(0)?,
                request_id: row.get(1)?,
                local_event_id: row.get(2)?,
                link_type: CorrelationLinkType::from_str(&link_type).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
                    )
                })?,
                confidence: row.get(4)?,
                reason: row.get(5)?,
                created_at_ms: row.get(6)?,
            })
        }) {
            Ok(rows) => rows,
            Err(err) => {
                eprintln!("  Failed to query request correlations: {err}");
                return correlations;
            }
        };

        for row in rows {
            match row {
                Ok(correlation) => correlations.push(correlation),
                Err(err) => eprintln!("  Failed to decode request correlation row: {err}"),
            }
        }

        correlations
    }

    #[cfg(test)]
    pub fn upsert_explanation(&self, explanation: &Explanation) {
        let evidence_json = match serde_json::to_string(&explanation.evidence_json) {
            Ok(value) => value,
            Err(err) => {
                eprintln!("  Failed to serialize explanation evidence for SQLite: {err}");
                return;
            }
        };

        let conn = self.db.lock();
        if let Err(err) = conn.execute(
            "
            INSERT INTO explanations (
                id,
                request_id,
                anomaly_kind,
                rank,
                confidence,
                summary,
                evidence_json,
                created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(id) DO UPDATE SET
                request_id = excluded.request_id,
                anomaly_kind = excluded.anomaly_kind,
                rank = excluded.rank,
                confidence = excluded.confidence,
                summary = excluded.summary,
                evidence_json = excluded.evidence_json,
                created_at_ms = excluded.created_at_ms
            ",
            params![
                explanation.id,
                explanation.request_id,
                explanation.anomaly_kind,
                explanation.rank,
                explanation.confidence,
                explanation.summary,
                evidence_json,
                explanation.created_at_ms,
            ],
        ) {
            eprintln!("  Failed to upsert explanation in SQLite: {err}");
        }
    }

    pub fn get_explanations_for_request(&self, request_id: &str, limit: usize) -> Vec<Explanation> {
        let conn = self.db.lock();
        let limit = limit.min(i64::MAX as usize) as i64;
        let mut explanations = Vec::new();

        let mut stmt = match conn.prepare(
            "
            SELECT id, request_id, anomaly_kind, rank, confidence, summary, evidence_json, created_at_ms
            FROM explanations
            WHERE request_id = ?1
            ORDER BY rank ASC, created_at_ms DESC
            LIMIT ?2
            ",
        ) {
            Ok(stmt) => stmt,
            Err(err) => {
                eprintln!("  Failed to prepare explanations query: {err}");
                return explanations;
            }
        };

        let rows = match stmt.query_map(params![request_id, limit], |row| {
            let evidence_json_text: String = row.get(6)?;
            Ok(Explanation {
                id: row.get(0)?,
                request_id: row.get(1)?,
                anomaly_kind: row.get(2)?,
                rank: row.get(3)?,
                confidence: row.get(4)?,
                summary: row.get(5)?,
                evidence_json: serde_json::from_str(&evidence_json_text).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        6,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                created_at_ms: row.get(7)?,
            })
        }) {
            Ok(rows) => rows,
            Err(err) => {
                eprintln!("  Failed to query explanations: {err}");
                return explanations;
            }
        };

        for row in rows {
            match row {
                Ok(explanation) => explanations.push(explanation),
                Err(err) => eprintln!("  Failed to decode explanation row: {err}"),
            }
        }

        explanations
    }

    pub fn replace_explanations_for_request(&self, request_id: &str, explanations: &[Explanation]) {
        let mut conn = self.db.lock();
        let tx = match conn.transaction() {
            Ok(tx) => tx,
            Err(err) => {
                eprintln!("  Failed to start explanation replacement transaction: {err}");
                return;
            }
        };

        if let Err(err) = tx.execute(
            "DELETE FROM explanations WHERE request_id = ?1",
            params![request_id],
        ) {
            eprintln!("  Failed to clear existing explanations: {err}");
            return;
        }

        for explanation in explanations {
            if explanation.request_id != request_id {
                continue;
            }

            let evidence_json = match serde_json::to_string(&explanation.evidence_json) {
                Ok(value) => value,
                Err(err) => {
                    eprintln!("  Failed to serialize explanation evidence for replacement: {err}");
                    return;
                }
            };

            if let Err(err) = tx.execute(
                "
                INSERT INTO explanations (
                    id,
                    request_id,
                    anomaly_kind,
                    rank,
                    confidence,
                    summary,
                    evidence_json,
                    created_at_ms
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                ",
                params![
                    explanation.id,
                    explanation.request_id,
                    explanation.anomaly_kind,
                    explanation.rank,
                    explanation.confidence,
                    explanation.summary,
                    evidence_json,
                    explanation.created_at_ms,
                ],
            ) {
                eprintln!("  Failed to insert replaced explanation: {err}");
                return;
            }
        }

        if let Err(err) = tx.commit() {
            eprintln!("  Failed to commit explanation replacement transaction: {err}");
        }
    }

    #[allow(dead_code)]
    pub fn insert_settings_history_snapshot(&self, snapshot: &SettingsHistoryItem) -> bool {
        let conn = self.db.lock();
        if let Err(err) = conn.execute(
            "
            INSERT INTO settings_history (
                id,
                saved_at_ms,
                content_hash,
                settings_json,
                source
            ) VALUES (?1, ?2, ?3, ?4, ?5)
            ",
            params![
                &snapshot.id,
                snapshot.saved_at_ms,
                &snapshot.content_hash,
                &snapshot.settings_json,
                &snapshot.source,
            ],
        ) {
            eprintln!("  Failed to insert settings history snapshot in SQLite: {err}");
            return false;
        }

        true
    }

    #[allow(dead_code)]
    pub fn list_settings_history_desc(&self, limit: usize) -> Vec<SettingsHistoryItem> {
        let conn = self.db.lock();
        let limit = limit.min(i64::MAX as usize) as i64;
        let mut history = Vec::new();

        let mut stmt = match conn.prepare(
            "
            SELECT id, saved_at_ms, content_hash, settings_json, source
            FROM settings_history
            ORDER BY saved_at_ms DESC
            LIMIT ?1
            ",
        ) {
            Ok(stmt) => stmt,
            Err(err) => {
                eprintln!("  Failed to prepare settings history query: {err}");
                return history;
            }
        };

        let rows = match stmt.query_map(params![limit], |row| {
            Ok(SettingsHistoryItem {
                id: row.get(0)?,
                saved_at_ms: row.get(1)?,
                content_hash: row.get(2)?,
                settings_json: row.get(3)?,
                source: row.get(4)?,
            })
        }) {
            Ok(rows) => rows,
            Err(err) => {
                eprintln!("  Failed to query settings history: {err}");
                return history;
            }
        };

        for row in rows {
            match row {
                Ok(item) => history.push(item),
                Err(err) => eprintln!("  Failed to decode settings history row: {err}"),
            }
        }

        history
    }

    #[allow(dead_code)]
    pub fn get_settings_history_item(&self, id: &str) -> Option<SettingsHistoryItem> {
        let conn = self.db.lock();
        conn.query_row(
            "
            SELECT id, saved_at_ms, content_hash, settings_json, source
            FROM settings_history
            WHERE id = ?1
            ",
            params![id],
            |row| {
                Ok(SettingsHistoryItem {
                    id: row.get(0)?,
                    saved_at_ms: row.get(1)?,
                    content_hash: row.get(2)?,
                    settings_json: row.get(3)?,
                    source: row.get(4)?,
                })
            },
        )
        .ok()
    }

    #[allow(dead_code)]
    pub fn delete_settings_history_item(&self, id: &str) -> bool {
        let conn = self.db.lock();
        match conn.execute("DELETE FROM settings_history WHERE id = ?1", params![id]) {
            Ok(changed) => changed > 0,
            Err(err) => {
                eprintln!("  Failed to delete settings history item in SQLite: {err}");
                false
            }
        }
    }

    pub fn delete_session_db_rows_with_live_guard(
        &self,
        session_id: &str,
    ) -> Result<SessionDeleteDbOutcome, String> {
        let trimmed = session_id.trim();
        if trimmed.is_empty() {
            return Err("session_id is required".to_string());
        }

        let mut conn = self.db.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|err| format!("failed to start transaction: {err}"))?;

        let initial_now_ms = Utc::now().timestamp_millis();
        if query_session_live_state_in_tx(&tx, trimmed, initial_now_ms)? {
            tx.rollback()
                .map_err(|err| format!("failed to rollback live-guard transaction: {err}"))?;
            return Ok(SessionDeleteDbOutcome {
                blocked_live: true,
                deleted_db_rows_by_table: SessionDeleteDbRowsByTable::default(),
            });
        }

        let mut deleted = SessionDeleteDbRowsByTable::default();

        deleted.request_bodies = tx
            .execute(
                "DELETE FROM request_bodies WHERE request_id IN (
                    SELECT id FROM requests WHERE session_id = ?1
                )",
                params![trimmed],
            )
            .map_err(|err| format!("failed to delete request_bodies: {err}"))?
            as u64;

        deleted.request_correlations = tx
            .execute(
                "DELETE FROM request_correlations WHERE request_id IN (
                    SELECT id FROM requests WHERE session_id = ?1
                )",
                params![trimmed],
            )
            .map_err(|err| format!("failed to delete request_correlations: {err}"))?
            as u64;

        deleted.explanations = tx
            .execute(
                "DELETE FROM explanations WHERE request_id IN (
                    SELECT id FROM requests WHERE session_id = ?1
                )",
                params![trimmed],
            )
            .map_err(|err| format!("failed to delete explanations: {err}"))?
            as u64;

        deleted.requests =
            tx.execute(
                "DELETE FROM requests WHERE session_id = ?1",
                params![trimmed],
            )
            .map_err(|err| format!("failed to delete requests: {err}"))? as u64;

        deleted.local_events =
            tx.execute(
                "DELETE FROM local_events WHERE session_hint = ?1",
                params![trimmed],
            )
            .map_err(|err| format!("failed to delete local_events: {err}"))? as u64;

        #[cfg(test)]
        if self
            .pre_commit_live_guard_flip_pending
            .swap(false, Ordering::SeqCst)
        {
            let now_ms = Utc::now().timestamp_millis();
            tx.execute(
                "INSERT INTO requests (
                    id,
                    timestamp,
                    timestamp_ms,
                    session_id,
                    method,
                    path,
                    model,
                    stream,
                    status_kind,
                    status_code,
                    duration_ms,
                    ttft_ms,
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_creation_tokens,
                    thinking_tokens,
                    request_size_bytes,
                    response_size_bytes,
                    error,
                    stalls_json,
                    anomalies_json,
                    entry_json
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23
                )",
                params![
                    format!("test-live-flip-{now_ms}"),
                    Utc::now().to_rfc3339(),
                    now_ms,
                    trimmed,
                    "POST",
                    "/v1/messages",
                    "claude-test",
                    false,
                    "pending",
                    Option::<i64>::None,
                    0.0f64,
                    Option::<f64>::None,
                    Option::<i64>::None,
                    Option::<i64>::None,
                    Option::<i64>::None,
                    Option::<i64>::None,
                    Option::<i64>::None,
                    0i64,
                    0i64,
                    Option::<String>::None,
                    "[]",
                    "[]",
                    "{}",
                ],
            )
            .map_err(|err| format!("failed to inject pre-commit live flip row: {err}"))?;
        }

        let pre_commit_now_ms = Utc::now().timestamp_millis();
        if query_session_live_state_in_tx(&tx, trimmed, pre_commit_now_ms)? {
            tx.rollback()
                .map_err(|err| format!("failed to rollback live-guard transaction: {err}"))?;
            return Ok(SessionDeleteDbOutcome {
                blocked_live: true,
                deleted_db_rows_by_table: SessionDeleteDbRowsByTable::default(),
            });
        }

        tx.commit()
            .map_err(|err| format!("failed to commit delete transaction: {err}"))?;

        Ok(SessionDeleteDbOutcome {
            blocked_live: false,
            deleted_db_rows_by_table: deleted,
        })
    }

    #[allow(dead_code)]
    pub fn clear_settings_history(&self) -> usize {
        let conn = self.db.lock();
        match conn.execute("DELETE FROM settings_history", []) {
            Ok(changed) => changed,
            Err(err) => {
                eprintln!("  Failed to clear settings history in SQLite: {err}");
                0
            }
        }
    }

    pub fn write_body(&self, request_id: &str, request_body: &str, response_body: &str) {
        let max = self.max_body_size;
        let (req_truncated, req_body) = if request_body.len() > max {
            (true, &request_body[..max])
        } else {
            (false, request_body)
        };
        let (resp_truncated, resp_body) = if response_body.len() > max {
            (true, &response_body[..max])
        } else {
            (false, response_body)
        };
        let truncated = if req_truncated || resp_truncated {
            1i64
        } else {
            0i64
        };

        let conn = self.db.lock();
        if let Err(err) = conn.execute(
            "INSERT OR REPLACE INTO request_bodies (request_id, request_body, response_body, truncated)
             VALUES (?1, ?2, ?3, ?4)",
            params![request_id, req_body, resp_body, truncated],
        ) {
            eprintln!("  Failed to write request body to SQLite: {err}");
        }
    }

    pub fn get_body(&self, request_id: &str) -> Option<RequestBodyRecord> {
        let conn = self.db.lock();
        conn.query_row(
            "SELECT request_body, response_body, truncated FROM request_bodies WHERE request_id = ?1",
            params![request_id],
            |row| {
                Ok(RequestBodyRecord {
                    request_body: row.get(0)?,
                    response_body: row.get(1)?,
                    truncated: row.get::<_, i64>(2)? != 0,
                })
            },
        )
        .ok()
    }

    pub fn get_claude_sessions(&self) -> Vec<ClaudeSession> {
        let projects_dir = self.claude_dir.join("projects");
        let mut sessions: std::collections::HashMap<String, ClaudeSession> =
            std::collections::HashMap::new();

        let mut project_roots = std::fs::read_dir(&projects_dir)
            .ok()
            .into_iter()
            .flat_map(|entries| entries.flatten())
            .filter_map(|entry| {
                let path = entry.path();
                if !path.is_dir() {
                    return None;
                }

                let project_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                Some((project_name, path))
            })
            .collect::<Vec<_>>();
        project_roots.sort_by(|a, b| a.0.cmp(&b.0));

        let mut roots_scanned = 0usize;
        let mut files_scanned = 0usize;

        for (project_name, project_path) in project_roots {
            if roots_scanned >= MAX_SESSION_PROJECT_ROOTS_SCANNED {
                break;
            }
            roots_scanned += 1;

            let mut stack = vec![(project_path.clone(), 0usize)];
            let mut project_files = Vec::new();

            while let Some((dir, depth)) = stack.pop() {
                if depth > MAX_SESSION_PROJECT_SCAN_DEPTH {
                    continue;
                }

                let mut dir_entries = match std::fs::read_dir(&dir) {
                    Ok(entries) => entries
                        .flatten()
                        .map(|entry| entry.path())
                        .collect::<Vec<_>>(),
                    Err(_) => continue,
                };
                dir_entries.sort_by(|a, b| {
                    normalize_source_path(&a.to_string_lossy())
                        .cmp(&normalize_source_path(&b.to_string_lossy()))
                });

                let mut subdirs = Vec::new();
                for path in dir_entries {
                    if path.is_dir() {
                        if depth < MAX_SESSION_PROJECT_SCAN_DEPTH {
                            subdirs.push(path);
                        }
                        continue;
                    }

                    let extension = path.extension().and_then(|e| e.to_str());
                    if extension != Some("json") && extension != Some("jsonl") {
                        continue;
                    }

                    project_files.push(path);
                }

                for subdir in subdirs.into_iter().rev() {
                    stack.push((subdir, depth + 1));
                }
            }

            project_files.sort_by(|a, b| {
                normalize_source_path(&a.to_string_lossy())
                    .cmp(&normalize_source_path(&b.to_string_lossy()))
            });

            for file_path in project_files {
                if files_scanned >= MAX_SESSION_FILES_SCANNED {
                    break;
                }
                files_scanned += 1;

                let Ok(metadata) = std::fs::metadata(&file_path) else {
                    continue;
                };
                if metadata.len() > MAX_SESSION_ARTIFACT_FILE_BYTES {
                    continue;
                }

                let modified_ms = metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);

                if let Ok(text) = std::fs::read_to_string(&file_path) {
                    let extension = file_path
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .unwrap_or_default();

                    if extension == "jsonl" {
                        for line in text.lines() {
                            let trimmed = line.trim();
                            if trimmed.is_empty() {
                                continue;
                            }

                            let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                                continue;
                            };

                            let sid = json
                                .get("sessionId")
                                .or_else(|| json.get("session_id"))
                                .or_else(|| json.get("session"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            if sid.is_empty() {
                                continue;
                            }

                            let message_activity_ms = extract_conversation_items_from_json(
                                &sid,
                                &project_name,
                                &json,
                            )
                            .into_iter()
                            .map(|item| item.timestamp_ms)
                            .max()
                            .unwrap_or(0);

                            let line_activity_ms = ["timestamp", "timestamp_ms", "created_at"]
                                .iter()
                                .filter_map(|key| json.get(*key))
                                .find_map(parse_timestamp_ms)
                                .unwrap_or(0);

                            let local_activity_ms = modified_ms.max(message_activity_ms.max(line_activity_ms));

                            let session = sessions.entry(sid.clone()).or_insert(ClaudeSession {
                                session_id: sid,
                                project_path: String::new(),
                                project_paths: Vec::new(),
                                last_modified_ms: 0,
                                last_local_activity_ms: 0,
                                last_proxy_activity_ms: 0,
                                request_count: 0,
                                in_flight_requests: 0,
                                is_live: false,
                                has_proxy_requests: false,
                                has_local_evidence: false,
                            });

                            if !project_name.is_empty()
                                && !session
                                    .project_paths
                                    .iter()
                                    .any(|path| path == &project_name)
                            {
                                session.project_paths.push(project_name.clone());
                            }
                            if modified_ms > session.last_modified_ms {
                                session.last_modified_ms = modified_ms;
                            }
                            if local_activity_ms > session.last_local_activity_ms {
                                session.last_local_activity_ms = local_activity_ms;
                            }
                            session.has_local_evidence = true;
                        }

                        continue;
                    }

                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                        let sid = json
                            .get("sessionId")
                            .or_else(|| json.get("session_id"))
                            .or_else(|| json.get("session"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if sid.is_empty() {
                            continue;
                        }

                        let message_activity_ms =
                            extract_conversation_items_from_json(&sid, &project_name, &json)
                                .into_iter()
                                .map(|item| item.timestamp_ms)
                                .max()
                                .unwrap_or(0);
                        let local_activity_ms = modified_ms.max(message_activity_ms);

                        let session = sessions.entry(sid.clone()).or_insert(ClaudeSession {
                            session_id: sid,
                            project_path: String::new(),
                            project_paths: Vec::new(),
                            last_modified_ms: 0,
                            last_local_activity_ms: 0,
                            last_proxy_activity_ms: 0,
                            request_count: 0,
                            in_flight_requests: 0,
                            is_live: false,
                            has_proxy_requests: false,
                            has_local_evidence: false,
                        });

                        if !project_name.is_empty()
                            && !session
                                .project_paths
                                .iter()
                                .any(|path| path == &project_name)
                        {
                            session.project_paths.push(project_name.clone());
                        }
                        if modified_ms > session.last_modified_ms {
                            session.last_modified_ms = modified_ms;
                        }
                        if local_activity_ms > session.last_local_activity_ms {
                            session.last_local_activity_ms = local_activity_ms;
                        }
                        session.has_local_evidence = true;
                    }
                }
            }

            if files_scanned >= MAX_SESSION_FILES_SCANNED {
                break;
            }
        }

        let now_ms = Utc::now().timestamp_millis();

        // Cross-reference with proxy request counts and activity.
        {
            let conn = self.db.lock();
            let counts: Vec<(String, i64, i64, i64)> = conn
                .prepare(
                    "SELECT session_id, COUNT(*), COALESCE(MAX(timestamp_ms), 0), SUM(CASE WHEN status_kind = 'pending' THEN 1 ELSE 0 END) FROM requests WHERE session_id IS NOT NULL GROUP BY session_id",
                )
                .and_then(|mut stmt| {
                    let rows = stmt.query_map([], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                        ))
                    })?;
                    Ok(rows.flatten().collect())
                })
                .unwrap_or_default();

            for (sid, count, max_proxy_activity_ms, in_flight_requests) in counts {
                let session = sessions.entry(sid.clone()).or_insert(ClaudeSession {
                    session_id: sid,
                    project_path: String::new(),
                    project_paths: Vec::new(),
                    last_modified_ms: 0,
                    last_local_activity_ms: 0,
                    last_proxy_activity_ms: 0,
                    request_count: 0,
                    in_flight_requests: 0,
                    is_live: false,
                    has_proxy_requests: false,
                    has_local_evidence: false,
                });
                session.request_count = count as u64;
                session.in_flight_requests = in_flight_requests.max(0) as u64;
                session.has_proxy_requests = count > 0;
                if max_proxy_activity_ms > session.last_proxy_activity_ms {
                    session.last_proxy_activity_ms = max_proxy_activity_ms;
                }
            }
        }

        // Local events also count as local evidence and local activity.
        {
            let conn = self.db.lock();
            let local_activity: Vec<(String, i64)> = conn
                .prepare(
                    "SELECT session_hint, COALESCE(MAX(event_time_ms), 0) FROM local_events WHERE session_hint IS NOT NULL GROUP BY session_hint",
                )
                .and_then(|mut stmt| {
                    let rows = stmt.query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                    })?;
                    Ok(rows.flatten().collect())
                })
                .unwrap_or_default();

            for (sid, max_local_event_ms) in local_activity {
                let session = sessions.entry(sid.clone()).or_insert(ClaudeSession {
                    session_id: sid,
                    project_path: String::new(),
                    project_paths: Vec::new(),
                    last_modified_ms: 0,
                    last_local_activity_ms: 0,
                    last_proxy_activity_ms: 0,
                    request_count: 0,
                    in_flight_requests: 0,
                    is_live: false,
                    has_proxy_requests: false,
                    has_local_evidence: false,
                });
                session.has_local_evidence = true;
                if max_local_event_ms > session.last_local_activity_ms {
                    session.last_local_activity_ms = max_local_event_ms;
                }
            }
        }

        let mut result: Vec<ClaudeSession> = sessions.into_values().collect();

        for session in &mut result {
            session.project_paths.sort();
            session.project_paths.dedup();
            session.project_path = session.project_paths.first().cloned().unwrap_or_default();
            session.last_modified_ms = session
                .last_local_activity_ms
                .max(session.last_proxy_activity_ms);
            session.is_live = compute_session_is_live(
                session.in_flight_requests,
                session.last_local_activity_ms,
                session.last_proxy_activity_ms,
                now_ms,
            );
        }

        result.sort_by(|a, b| {
            let a_activity = a.last_local_activity_ms.max(a.last_proxy_activity_ms);
            let b_activity = b.last_local_activity_ms.max(b.last_proxy_activity_ms);
            b_activity
                .cmp(&a_activity)
                .then_with(|| a.session_id.cmp(&b.session_id))
        });
        result
    }

    pub fn get_merged_sessions(&self) -> Vec<MergedSessionSummary> {
        let proxy = self.get_sessions();
        let local = self.get_claude_sessions();

        let mut by_id = std::collections::BTreeMap::<String, MergedSessionSummary>::new();

        for p in proxy {
            let last_proxy = p.last_request.map(|ts| ts.timestamp_millis());
            by_id.insert(
                p.session_id.clone(),
                MergedSessionSummary {
                    session_id: p.session_id,
                    presence: "proxy".to_string(),
                    proxy_request_count: p.request_count,
                    local_event_count: 0,
                    proxy_error_count: p.error_count,
                    proxy_stall_count: p.stall_count,
                    last_proxy_activity_ms: last_proxy,
                    last_local_activity_ms: None,
                    last_activity_ms: last_proxy.unwrap_or(0),
                    project_path: None,
                },
            );
        }

        for l in local {
            let local_ms = l.last_local_activity_ms.max(l.last_modified_ms);
            let row = by_id
                .entry(l.session_id.clone())
                .or_insert(MergedSessionSummary {
                    session_id: l.session_id.clone(),
                    presence: "local".to_string(),
                    proxy_request_count: 0,
                    local_event_count: 0,
                    proxy_error_count: 0,
                    proxy_stall_count: 0,
                    last_proxy_activity_ms: None,
                    last_local_activity_ms: None,
                    last_activity_ms: 0,
                    project_path: None,
                });

            if l.has_proxy_requests && row.proxy_request_count == 0 {
                row.proxy_request_count = l.request_count;
            }
            row.local_event_count = if l.has_local_evidence { 1 } else { 0 };
            row.last_local_activity_ms = if local_ms > 0 { Some(local_ms) } else { None };
            row.project_path = if l.project_path.is_empty() {
                None
            } else {
                Some(l.project_path)
            };

            row.presence = match (
                row.last_proxy_activity_ms.is_some() || l.has_proxy_requests,
                l.has_local_evidence,
            ) {
                (true, true) => "both".to_string(),
                (true, false) => "proxy".to_string(),
                (false, true) => "local".to_string(),
                (false, false) => "proxy".to_string(),
            };

            let proxy_ms = row
                .last_proxy_activity_ms
                .unwrap_or(0)
                .max(l.last_proxy_activity_ms);
            if proxy_ms > 0 {
                row.last_proxy_activity_ms = Some(proxy_ms);
            }
            row.last_activity_ms = row
                .last_proxy_activity_ms
                .unwrap_or(0)
                .max(row.last_local_activity_ms.unwrap_or(0));
        }

        let mut out = by_id.into_values().collect::<Vec<_>>();
        out.sort_by(|a, b| {
            b.last_activity_ms
                .cmp(&a.last_activity_ms)
                .then_with(|| a.session_id.cmp(&b.session_id))
        });
        out
    }

    pub fn set_ingestion_checkpoint(&self, source_kind: SourceKind, checkpoint: &str) {
        let conn = self.db.lock();
        if let Err(err) = conn.execute(
            "
            INSERT INTO ingestion_checkpoints (source_kind, checkpoint, updated_at_ms)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(source_kind) DO UPDATE SET
                checkpoint = excluded.checkpoint,
                updated_at_ms = excluded.updated_at_ms
            ",
            params![
                source_kind.as_str(),
                checkpoint,
                Utc::now().timestamp_millis()
            ],
        ) {
            eprintln!("  Failed to upsert ingestion checkpoint in SQLite: {err}");
        }
    }

    pub fn get_ingestion_checkpoint(&self, source_kind: SourceKind) -> Option<String> {
        let conn = self.db.lock();
        conn.query_row(
            "SELECT checkpoint FROM ingestion_checkpoints WHERE source_kind = ?1",
            [source_kind.as_str()],
            |row| row.get(0),
        )
        .ok()
    }

    pub fn load_from_db(&self) {
        let loaded = {
            let conn = self.db.lock();
            let mut stmt = conn
                .prepare("SELECT entry_json FROM requests ORDER BY timestamp_ms DESC LIMIT ?1")
                .expect("Failed to prepare SQLite load query");

            let rows = stmt
                .query_map([self.max_entries as i64], |row| row.get::<_, String>(0))
                .expect("Failed to load request entries from SQLite");

            let mut loaded = Vec::new();
            for row in rows {
                let entry_json = match row {
                    Ok(value) => value,
                    Err(err) => {
                        eprintln!("  Failed to read SQLite row: {err}");
                        continue;
                    }
                };

                match serde_json::from_str::<RequestEntry>(&entry_json) {
                    Ok(entry) => loaded.push(entry),
                    Err(err) => eprintln!("  Failed to decode SQLite entry JSON: {err}"),
                }
            }

            loaded
        };

        let mut entries = self.entries.write();
        entries.clear();
        for entry in loaded.into_iter().rev() {
            entries.push_back(entry);
        }

        if !entries.is_empty() {
            eprintln!(
                "  Loaded {} recent entr{} from SQLite ({})",
                entries.len(),
                if entries.len() == 1 { "y" } else { "ies" },
                self.db_path.display()
            );
        }
    }

    fn print_entry(&self, entry: &RequestEntry) {
        let red = "\x1b[91m";
        let green = "\x1b[92m";
        let yellow = "\x1b[93m";
        let cyan = "\x1b[96m";
        let dim = "\x1b[2m";
        let reset = "\x1b[0m";

        let (icon, status_color) = match &entry.status {
            RequestStatus::Success(_) => ("✓", green),
            RequestStatus::ClientError(_) => ("⚠", yellow),
            RequestStatus::ServerError(_) => ("✗", red),
            RequestStatus::Timeout => ("⏱", red),
            RequestStatus::ConnectionError => ("⚡", red),
            _ => ("?", yellow),
        };

        let dur_color = if entry.duration_ms > 30000.0 {
            red
        } else if entry.duration_ms > 10000.0 {
            yellow
        } else {
            green
        };

        let ts = entry.timestamp.format("%H:%M:%S");
        let status = entry.status.code_str();

        print!("  {status_color}{icon} {ts}  {status:<8}{reset}");
        print!("  {dur_color}{:>7.0}ms{reset}", entry.duration_ms);

        if let Some(ttft) = entry.ttft_ms {
            let ttft_color = if ttft > 10000.0 {
                red
            } else if ttft > 5000.0 {
                yellow
            } else {
                green
            };
            print!("  {ttft_color}TTFT:{:.0}ms{reset}", ttft);
        }

        if !entry.model.is_empty() {
            print!("  {cyan}{}{reset}", entry.model);
        }

        if let (Some(inp), Some(out)) = (entry.input_tokens, entry.output_tokens) {
            print!("  {dim}({inp}→{out} tok){reset}");
        }

        if !entry.stalls.is_empty() {
            let total: f64 = entry.stalls.iter().map(|s| s.duration_s).sum();
            print!(
                "  {red}[{} stall(s) {:.1}s]{reset}",
                entry.stalls.len(),
                total
            );
        }

        println!();

        if let Some(err) = &entry.error {
            println!("    {red}└─ {}{reset}", &err[..err.len().min(120)]);
        }

        for anomaly in &entry.anomalies {
            let sev_color = match anomaly.severity {
                Severity::Critical => red,
                Severity::Error => red,
                Severity::Warning => yellow,
            };
            println!("    {sev_color}└─ ⚠ {}{reset}", anomaly.message);
        }
    }

    pub fn clear_stats(&self) -> ClearSummary {
        self.clear(ClearMode::MemoryOnly)
    }

    pub fn clear_all(&self) -> ClearSummary {
        self.clear(ClearMode::StatsAndLogs)
    }

    fn clear(&self, mode: ClearMode) -> ClearSummary {
        let cleared_entries = {
            let mut entries = self.entries.write();
            let count = entries.len();
            entries.clear();
            count
        };

        *self.start_time.write() = Utc::now();

        let deleted_persisted_entries = match mode {
            ClearMode::MemoryOnly => {
                self.clear_volatile_tables();
                0
            }
            ClearMode::StatsAndLogs => self.clear_persisted_entries(),
        };

        let summary = ClearSummary {
            mode,
            cleared_entries,
            deleted_persisted_entries,
        };

        if let Ok(json) = serde_json::to_string(&summary) {
            let msg = format!("{{\"type\":\"reset\",\"data\":{json}}}");
            let _ = self.broadcast_tx.send(msg);
        }

        let stats = self.get_live_stats_snapshot();
        if let Ok(json) = serde_json::to_string(&stats) {
            let msg = format!("{{\"type\":\"stats\",\"data\":{json}}}");
            let _ = self.broadcast_tx.send(msg);
        }

        summary
    }

    fn clear_volatile_tables(&self) {
        let conn = self.db.lock();
        conn.execute("DELETE FROM request_correlations", []).ok();
        conn.execute("DELETE FROM explanations", []).ok();
        conn.execute("DELETE FROM local_events", []).ok();
        conn.execute("DELETE FROM ingestion_checkpoints", []).ok();
        conn.execute("DELETE FROM config_snapshots", []).ok();
    }

    fn clear_persisted_entries(&self) -> usize {
        {
            let conn = self.db.lock();
            conn.execute("DELETE FROM request_correlations", []).ok();
            conn.execute("DELETE FROM explanations", []).ok();
            conn.execute("DELETE FROM request_bodies", []).ok();
            conn.execute("DELETE FROM local_events", []).ok();
            conn.execute("DELETE FROM ingestion_checkpoints", []).ok();
            conn.execute("DELETE FROM config_snapshots", []).ok();
            conn.execute("DELETE FROM settings_history", []).ok();
            conn.execute("DELETE FROM requests", []).unwrap_or_default()
        }
    }
}

fn status_parts(status: &RequestStatus) -> (&'static str, Option<i64>) {
    match status {
        RequestStatus::Success(code) => ("success", Some(i64::from(*code))),
        RequestStatus::ClientError(code) => ("client_error", Some(i64::from(*code))),
        RequestStatus::ServerError(code) => ("server_error", Some(i64::from(*code))),
        RequestStatus::Timeout => ("timeout", None),
        RequestStatus::ConnectionError => ("connection_error", None),
        RequestStatus::ProxyError => ("proxy_error", None),
        RequestStatus::Pending => ("pending", None),
    }
}

fn opt_u64_to_i64(value: Option<u64>) -> Option<i64> {
    value.and_then(|v| i64::try_from(v).ok())
}

const SESSION_LIVE_ACTIVITY_WINDOW_MS: i64 = 120_000;
const MAX_PREVIEW_CHARS: usize = 220;
const MAX_FULL_TEXT_CHARS: usize = 16_384;
const MAX_TIMELINE_LABEL_CHARS: usize = 160;
const MAX_REQUEST_PATH_CHARS: usize = 240;
const MAX_REQUEST_MODEL_CHARS: usize = 80;
const MAX_CONVERSATION_SOURCE_PATH_CHARS: usize = 300;
const SESSION_DETAILS_PAYLOAD_CAP_NO_FULL_TEXT_BYTES: usize = 1_048_576;
const SESSION_DETAILS_PAYLOAD_CAP_WITH_FULL_TEXT_BYTES: usize = 4_194_304;
const MAX_SESSION_PROJECT_ROOTS_SCANNED: usize = 200;
const MAX_SESSION_FILES_SCANNED: usize = 2_000;
const MAX_SESSION_PROJECT_SCAN_DEPTH: usize = 8;
const MAX_SESSION_ARTIFACT_FILE_BYTES: u64 = 2 * 1024 * 1024;

fn query_session_live_state_in_tx(
    tx: &rusqlite::Transaction<'_>,
    session_id: &str,
    now_ms: i64,
) -> Result<bool, String> {
    let in_flight_requests: i64 = tx
        .query_row(
            "
            SELECT COALESCE(SUM(CASE WHEN status_kind = 'pending' THEN 1 ELSE 0 END), 0)
            FROM requests
            WHERE session_id = ?1
            ",
            params![session_id],
            |row| row.get(0),
        )
        .map_err(|err| format!("failed to query in-flight requests: {err}"))?;

    let last_proxy_activity_ms: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(timestamp_ms), 0) FROM requests WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )
        .map_err(|err| format!("failed to query proxy activity: {err}"))?;

    let last_local_activity_ms: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(event_time_ms), 0) FROM local_events WHERE session_hint = ?1",
            params![session_id],
            |row| row.get(0),
        )
        .map_err(|err| format!("failed to query local activity: {err}"))?;

    Ok(compute_session_is_live(
        in_flight_requests.max(0) as u64,
        last_local_activity_ms,
        last_proxy_activity_ms,
        now_ms,
    ))
}
fn compute_session_is_live(
    in_flight_requests: u64,
    last_local_activity_ms: i64,
    last_proxy_activity_ms: i64,
    now_ms: i64,
) -> bool {
    if in_flight_requests > 0 {
        return true;
    }

    let latest_activity_ms = last_local_activity_ms.max(last_proxy_activity_ms);
    latest_activity_ms >= now_ms - SESSION_LIVE_ACTIVITY_WINDOW_MS
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}
fn normalize_source_path(source_path: &str) -> String {
    source_path.trim().replace('\\', "/")
}

fn normalize_role(role: Option<&str>) -> String {
    role.map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn parse_timestamp_ms(value: &serde_json::Value) -> Option<i64> {
    if let Some(timestamp_ms) = value.as_i64() {
        return Some(timestamp_ms);
    }

    if let Some(timestamp_ms) = value.as_u64() {
        return i64::try_from(timestamp_ms).ok();
    }

    value.as_str().and_then(|raw| {
        raw.parse::<i64>().ok().or_else(|| {
            DateTime::parse_from_rfc3339(raw)
                .ok()
                .map(|dt| dt.timestamp_millis())
        })
    })
}

fn extract_message_text(content: &serde_json::Value) -> Option<String> {
    fn extract_part_text(part: &serde_json::Value) -> Option<String> {
        match part {
            serde_json::Value::String(text) => Some(text.clone()),
            serde_json::Value::Object(map) => ["content", "text", "message", "body"]
                .iter()
                .filter_map(|key| map.get(*key))
                .find_map(extract_message_text),
            _ => None,
        }
    }

    let normalized_text = match content {
        serde_json::Value::String(text) => text.trim().to_string(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(extract_part_text)
            .map(|part| part.trim().to_string())
            .filter(|part| !part.is_empty())
            .collect::<Vec<String>>()
            .join("\n"),
        serde_json::Value::Object(_) => extract_part_text(content).unwrap_or_default(),
        _ => String::new(),
    };

    if normalized_text.is_empty() {
        None
    } else {
        Some(normalized_text)
    }
}

fn extract_session_id_value<'a>(json: &'a serde_json::Value) -> Option<&'a str> {
    json.get("sessionId")
        .or_else(|| json.get("session_id"))
        .or_else(|| json.get("session"))
        .and_then(|value| value.as_str())
}

fn build_preview(full_text: &str) -> (String, bool) {
    let preview = truncate_chars(full_text, MAX_PREVIEW_CHARS);
    let is_truncated = full_text.chars().count() > MAX_PREVIEW_CHARS;
    (preview, is_truncated)
}

fn extract_conversation_items_from_source_text(
    session_id: &str,
    source_path: &str,
    text: &str,
) -> Vec<SessionConversationItem> {
    if source_path.to_ascii_lowercase().ends_with(".jsonl") {
        extract_conversation_items_from_jsonl(session_id, source_path, text)
    } else {
        let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
            return Vec::new();
        };

        extract_conversation_items_from_json(session_id, source_path, &json)
    }
}

fn extract_conversation_items_from_jsonl(
    session_id: &str,
    source_path: &str,
    text: &str,
) -> Vec<SessionConversationItem> {
    let mut conversation_items = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };

        let Some(line_session_id) = extract_session_id_value(&json) else {
            continue;
        };

        if line_session_id != session_id {
            continue;
        }

        conversation_items.extend(extract_conversation_items_from_json(
            session_id,
            source_path,
            &json,
        ));
    }

    conversation_items
}

fn extract_conversation_items_from_json(
    session_id: &str,
    source_path: &str,
    json: &serde_json::Value,
) -> Vec<SessionConversationItem> {
    #[derive(Copy, Clone)]
    enum CandidateSource {
        Events,
        Other,
    }

    fn extract_candidates<'a>(
        json: &'a serde_json::Value,
    ) -> Vec<(CandidateSource, &'a serde_json::Value)> {
        if let Some(items) = json.as_array() {
            return items
                .iter()
                .map(|item| {
                    let source = if item
                        .as_object()
                        .and_then(|obj| obj.get("message"))
                        .map(|value| value.is_object())
                        .unwrap_or(false)
                    {
                        CandidateSource::Events
                    } else {
                        CandidateSource::Other
                    };
                    (source, item)
                })
                .collect();
        }

        let mut candidates = Vec::new();
        if let Some(object) = json.as_object() {
            if object
                .get("message")
                .and_then(|value| value.as_object())
                .is_some()
            {
                candidates.push((CandidateSource::Events, json));
            }

            if is_message_like(object)
                && object
                    .get("message")
                    .and_then(|value| value.as_object())
                    .is_none()
            {
                candidates.push((CandidateSource::Other, json));
            }

            for key in ["messages", "conversation", "events"] {
                if let Some(items) = object.get(key).and_then(|value| value.as_array()) {
                    let source = if key == "events" {
                        CandidateSource::Events
                    } else {
                        CandidateSource::Other
                    };
                    candidates.extend(items.iter().map(|item| (source, item)));
                }
            }
        }

        candidates
    }

    fn extract_event_message_object<'a>(
        event: &'a serde_json::Value,
    ) -> Option<&'a serde_json::Map<String, serde_json::Value>> {
        let event_obj = event.as_object()?;

        if let Some(message_obj) = event_obj.get("message").and_then(|value| value.as_object()) {
            return Some(message_obj);
        }

        Some(event_obj)
    }

    fn extract_role_with_fallback(
        primary_obj: &serde_json::Map<String, serde_json::Value>,
        fallback_obj: Option<&serde_json::Map<String, serde_json::Value>>,
    ) -> String {
        let role = primary_obj
            .get("role")
            .or_else(|| primary_obj.get("author"))
            .or_else(|| primary_obj.get("sender"))
            .and_then(|value| value.as_str())
            .or_else(|| {
                fallback_obj.and_then(|outer| {
                    outer
                        .get("role")
                        .or_else(|| outer.get("author"))
                        .or_else(|| outer.get("sender"))
                        .and_then(|value| value.as_str())
                })
            });

        normalize_role(role)
    }

    fn extract_timestamp_with_fallback(
        primary_obj: &serde_json::Map<String, serde_json::Value>,
        fallback_obj: Option<&serde_json::Map<String, serde_json::Value>>,
    ) -> i64 {
        primary_obj
            .get("timestamp_ms")
            .or_else(|| primary_obj.get("created_at_ms"))
            .or_else(|| primary_obj.get("timestamp"))
            .and_then(parse_timestamp_ms)
            .or_else(|| {
                fallback_obj.and_then(|outer| {
                    outer
                        .get("timestamp_ms")
                        .or_else(|| outer.get("created_at_ms"))
                        .or_else(|| outer.get("timestamp"))
                        .and_then(parse_timestamp_ms)
                })
            })
            .unwrap_or(0)
    }

    fn extract_text(message_obj: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
        ["content", "text", "message", "body"]
            .iter()
            .filter_map(|key| message_obj.get(*key))
            .find_map(extract_message_text)
    }

    fn is_message_like(message_obj: &serde_json::Map<String, serde_json::Value>) -> bool {
        let has_role_like = ["role", "author", "sender"]
            .iter()
            .any(|key| message_obj.contains_key(*key));
        let has_text_key = ["content", "text", "message", "body"]
            .iter()
            .any(|key| message_obj.contains_key(*key));
        has_role_like || has_text_key
    }

    fn build_conversation_id(
        session_id: &str,
        normalized_source_path: &str,
        message_index: usize,
        timestamp_ms: i64,
        normalized_role: &str,
    ) -> String {
        let seed = format!(
            "{session_id}\n{normalized_source_path}\n{message_index}\n{timestamp_ms}\n{normalized_role}"
        );
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in seed.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }

        format!("conv-{hash:016x}")
    }

    let normalized_source_path = truncate_chars(
        &normalize_source_path(source_path),
        MAX_CONVERSATION_SOURCE_PATH_CHARS,
    );

    let mut conversation_items = Vec::new();

    for (source, candidate) in extract_candidates(json) {
        let (message_obj, fallback_obj) = match source {
            CandidateSource::Events => {
                let Some(outer_obj) = candidate.as_object() else {
                    continue;
                };
                let Some(message_obj) = extract_event_message_object(candidate) else {
                    continue;
                };
                let fallback_obj = if std::ptr::eq(message_obj, outer_obj) {
                    None
                } else {
                    Some(outer_obj)
                };
                (message_obj, fallback_obj)
            }
            CandidateSource::Other => {
                let Some(message_obj) = candidate.as_object() else {
                    continue;
                };
                (message_obj, None)
            }
        };

        if !is_message_like(message_obj) {
            continue;
        }

        let Some(text) = extract_text(message_obj) else {
            continue;
        };

        let role = extract_role_with_fallback(message_obj, fallback_obj);
        let timestamp_ms = extract_timestamp_with_fallback(message_obj, fallback_obj);

        let full_text = truncate_chars(&text, MAX_FULL_TEXT_CHARS);
        let (preview, text_truncated) = build_preview(&full_text);
        let message_index = conversation_items.len();

        conversation_items.push(SessionConversationItem {
            id: build_conversation_id(
                session_id,
                &normalized_source_path,
                message_index,
                timestamp_ms,
                &role,
            ),
            timestamp_ms,
            role,
            preview,
            full_text: Some(full_text),
            text_truncated,
            source_path: normalized_source_path.clone(),
            source_kind: "claude_project".to_string(),
            expandable: text_truncated,
        });
    }

    conversation_items
}

fn enforce_session_details_payload_cap(
    response: &mut SessionDetailsResponse,
    payload_cap: usize,
    include_full_text: bool,
) {
    let mut trim_stage = 0u8;

    while serde_json::to_vec(response)
        .map(|payload| payload.len() > payload_cap)
        .unwrap_or(false)
    {
        let mut trimmed = false;

        for _ in 0..4 {
            match trim_stage {
                0 => {
                    if !response.timeline.is_empty() {
                        response.timeline.pop();
                        response.truncated_sections.timeline = true;
                        trimmed = true;
                    }
                }
                1 => {
                    if !response.conversation.is_empty() {
                        response.conversation.pop();
                        response.truncated_sections.conversation = true;
                        trimmed = true;
                    }
                }
                2 => {
                    if !response.requests.is_empty() {
                        response.requests.pop();
                        response.truncated_sections.requests = true;
                        trimmed = true;
                    }
                }
                _ => {
                    if include_full_text {
                        if let Some(item) = response
                            .conversation
                            .iter_mut()
                            .rev()
                            .find(|item| item.full_text.is_some())
                        {
                            item.full_text = None;
                            item.expandable = false;
                            response.truncated_sections.full_text = true;
                            trimmed = true;
                        }
                    }
                }
            }

            trim_stage = (trim_stage + 1) % 4;

            if trimmed {
                break;
            }
        }

        if !trimmed {
            break;
        }
    }
}

fn build_entry_where_clause(filter: &EntryFilter) -> (String, Vec<Value>) {
    let mut clauses = Vec::new();
    let mut params = Vec::new();

    if let Some(search) = filter
        .search
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        let pattern = format!("%{}%", search.to_ascii_lowercase());
        clauses.push(
            "(LOWER(path) LIKE ? OR LOWER(model) LIKE ? OR LOWER(COALESCE(error, '')) LIKE ? OR LOWER(COALESCE(session_id, 'unknown')) LIKE ? OR LOWER(status_kind) LIKE ? OR CAST(COALESCE(status_code, '') AS TEXT) LIKE ?)".to_string(),
        );

        for _ in 0..5 {
            params.push(Value::Text(pattern.clone()));
        }
        params.push(Value::Text(format!("%{}%", search)));
    }

    if let Some(status) = filter.status.as_deref() {
        match status {
            "success" => clauses.push("status_kind = 'success'".to_string()),
            "error" => clauses.push(
                "status_kind IN ('client_error', 'server_error', 'timeout', 'connection_error', 'proxy_error')".to_string(),
            ),
            "4xx" => clauses.push("status_kind = 'client_error'".to_string()),
            "5xx" => clauses.push("status_kind = 'server_error'".to_string()),
            "timeout" => clauses.push("status_kind = 'timeout'".to_string()),
            "stall" => clauses.push("stalls_json <> '[]'".to_string()),
            _ => {}
        }
    }

    if let Some(model) = filter
        .model
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        clauses.push("LOWER(model) LIKE ?".to_string());
        params.push(Value::Text(format!("%{}%", model.to_ascii_lowercase())));
    }

    if let Some(session_id) = filter
        .session_id
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        clauses.push("session_id = ?".to_string());
        params.push(Value::Text(session_id.to_string()));
    }

    if filter.session_id_null == Some(true) {
        clauses.push("session_id IS NULL".to_string());
    }

    if filter.has_stalls == Some(true) {
        clauses.push("stalls_json <> '[]'".to_string());
    }

    if filter.has_anomalies == Some(true) {
        clauses.push("anomalies_json <> '[]'".to_string());
    }

    if let Some(min_ttft_ms) = filter.min_ttft_ms {
        clauses.push("COALESCE(ttft_ms, 0) >= ?".to_string());
        params.push(Value::Real(min_ttft_ms));
    }

    if let Some(min_duration_ms) = filter.min_duration_ms {
        clauses.push("duration_ms >= ?".to_string());
        params.push(Value::Real(min_duration_ms));
    }

    if clauses.is_empty() {
        (String::new(), params)
    } else {
        (format!(" WHERE {}", clauses.join(" AND ")), params)
    }
}

// ─── Filters ───

#[derive(Debug, Default, Deserialize)]
pub struct EntryFilter {
    pub search: Option<String>,
    pub status: Option<String>, // "success", "error", "4xx", "5xx", "timeout"
    pub model: Option<String>,
    pub session_id: Option<String>,
    pub session_id_null: Option<bool>,
    pub has_stalls: Option<bool>,
    pub has_anomalies: Option<bool>,
    pub min_ttft_ms: Option<f64>,
    pub min_duration_ms: Option<f64>,
}

impl EntryFilter {
    #[allow(dead_code)]
    pub fn matches(&self, e: &RequestEntry) -> bool {
        if let Some(ref search) = self.search {
            let s = search.to_lowercase();
            let haystack = format!(
                "{} {} {} {} {}",
                e.path,
                e.model,
                e.error.as_deref().unwrap_or(""),
                e.status.code_str(),
                e.session_id.as_deref().unwrap_or("")
            )
            .to_lowercase();
            if !haystack.contains(&s) {
                return false;
            }
        }

        if let Some(ref status) = self.status {
            let matches = match status.as_str() {
                "success" => matches!(e.status, RequestStatus::Success(_)),
                "error" => e.status.is_error(),
                "4xx" => matches!(e.status, RequestStatus::ClientError(_)),
                "5xx" => matches!(e.status, RequestStatus::ServerError(_)),
                "timeout" => matches!(e.status, RequestStatus::Timeout),
                "stall" => !e.stalls.is_empty(),
                _ => true,
            };
            if !matches {
                return false;
            }
        }

        if let Some(ref model) = self.model {
            if !e.model.contains(model) {
                return false;
            }
        }

        if let Some(ref sid) = self.session_id {
            if e.session_id.as_deref() != Some(sid.as_str()) {
                return false;
            }
        }

        if self.session_id_null == Some(true) && e.session_id.is_some() {
            return false;
        }

        if self.has_stalls == Some(true) && e.stalls.is_empty() {
            return false;
        }

        if self.has_anomalies == Some(true) && e.anomalies.is_empty() {
            return false;
        }

        if let Some(min_ttft) = self.min_ttft_ms {
            if e.ttft_ms.unwrap_or(0.0) < min_ttft {
                return false;
            }
        }

        if let Some(min_dur) = self.min_duration_ms {
            if e.duration_ms < min_dur {
                return false;
            }
        }

        true
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionPresence {
    None,
    ProxyOnly,
    LocalOnly,
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDetailsResponse {
    pub session_id: String,
    pub presence: SessionPresence,
    pub project_paths: Vec<String>,
    pub selected_project_path: Option<String>,
    pub summary: SessionDetailsSummary,
    pub requests: Vec<SessionRequestSummary>,
    pub timeline: Vec<SessionTimelineItem>,
    pub conversation: Vec<SessionConversationItem>,
    pub truncated_sections: SessionTruncatedSections,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDetailsSummary {
    pub proxy_request_count_total: u64,
    pub proxy_error_count_total: u64,
    pub proxy_stall_count_total: u64,
    pub local_event_count_total: u64,
    pub conversation_message_count_total: u64,
    pub last_proxy_activity_ms: Option<i64>,
    pub last_local_activity_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRequestSummary {
    pub id: String,
    pub timestamp_ms: i64,
    pub status: Option<String>,
    pub ttft_ms: Option<f64>,
    pub duration_ms: Option<f64>,
    pub path: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTimelineItem {
    pub id: String,
    pub timestamp_ms: i64,
    pub kind: String,
    pub label: String,
    pub request_id: Option<String>,
    pub local_event_id: Option<String>,
    pub conversation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConversationItem {
    pub id: String,
    pub timestamp_ms: i64,
    pub role: String,
    pub preview: String,
    pub full_text: Option<String>,
    pub text_truncated: bool,
    pub source_path: String,
    pub source_kind: String,
    pub expandable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTruncatedSections {
    pub requests: bool,
    pub timeline: bool,
    pub conversation: bool,
    pub full_text: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub request_count: u64,
    pub error_count: u64,
    pub stall_count: u64,
    pub avg_ttft_ms: f64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub first_request: Option<DateTime<Utc>>,
    pub last_request: Option<DateTime<Utc>>,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionGraph {
    pub session_id: String,
    pub nodes: Vec<SessionGraphNode>,
    pub edges: Vec<SessionGraphEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionGraphNode {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub timestamp_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionGraphEdge {
    pub from: String,
    pub to: String,
    pub kind: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry() -> RequestEntry {
        RequestEntry {
            id: "test-entry".into(),
            timestamp: Utc::now(),
            session_id: Some("session-1".into()),
            method: "POST".into(),
            path: "/v1/messages".into(),
            model: "claude-test".into(),
            stream: false,
            status: RequestStatus::Success(200),
            duration_ms: 1200.0,
            ttft_ms: Some(250.0),
            input_tokens: Some(10),
            output_tokens: Some(20),
            cache_read_tokens: None,
            cache_creation_tokens: None,
            thinking_tokens: None,
            stop_reason: None,
            request_size_bytes: 128,
            response_size_bytes: 256,
            stalls: Vec::new(),
            error: None,
            anomalies: Vec::new(),
        }
    }

    fn has_database_file(storage_dir: &std::path::Path) -> bool {
        storage_dir.join("proxy.db").exists()
    }

    fn test_store() -> StatsStore {
        let log_dir =
            std::env::temp_dir().join(format!("claude-proxy-test-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();
        StatsStore::new(50, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir)
    }

    #[test]
    fn extract_message_text_joins_supported_content_parts_in_order() {
        let content = serde_json::json!([
            {"text": "A"},
            "B",
            {"content": "C"},
            {"unsupported": "D"}
        ]);

        assert_eq!(extract_message_text(&content).as_deref(), Some("A\nB\nC"));
    }

    #[test]
    fn events_array_includes_key_based_message_like_items_without_type_gate() {
        let json = serde_json::json!({
            "events": [
                {
                    "type": "message",
                    "message": {
                        "role": "user",
                        "timestamp_ms": 1000,
                        "content": "Hello"
                    }
                },
                {
                    "type": "custom_event",
                    "message": {
                        "role": "assistant",
                        "timestamp_ms": "1002",
                        "content": [
                            {"text": "A"},
                            "",
                            {"content": "B"},
                            {"ignored": "C"}
                        ]
                    }
                },
                {
                    "type": "tool_result",
                    "payload": {
                        "tool": "bash",
                        "status": "ok"
                    }
                },
                {
                    "type": "message",
                    "message": {
                        "role": "assistant",
                        "timestamp_ms": 1003,
                        "content": ["", "   "]
                    }
                }
            ]
        });

        let items =
            extract_conversation_items_from_json("session-123", "proj\\path\\session.json", &json);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].role, "user");
        assert_eq!(items[0].full_text.as_deref(), Some("Hello"));
        assert_eq!(items[1].role, "assistant");
        assert_eq!(items[1].full_text.as_deref(), Some("A\nB"));
    }

    #[test]
    fn top_level_array_event_envelope_preserves_nested_message_metadata() {
        let json = serde_json::json!([
            {
                "type": "custom_event",
                "message": {
                    "role": "user",
                    "timestamp_ms": 321,
                    "content": "Top-level array envelope"
                }
            }
        ]);

        let items = extract_conversation_items_from_json(
            "session-top-array",
            "proj\\top-array.json",
            &json,
        );

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].role, "user");
        assert_eq!(items[0].timestamp_ms, 321);
        assert_eq!(
            items[0].full_text.as_deref(),
            Some("Top-level array envelope")
        );
    }

    #[test]
    fn events_key_matrix_accepts_non_empty_author_sender_text_message_body() {
        let json = serde_json::json!({
            "events": [
                {"author": "author-role", "text": "Text value", "timestamp_ms": 11},
                {"sender": "sender-role", "message": "Message value", "timestamp_ms": 12},
                {"author": "body-role", "body": "Body value", "timestamp_ms": 13},
                {"sender": "outer-sender", "timestamp_ms": 14, "message": {"body": "Nested body value"}}
            ]
        });

        let items =
            extract_conversation_items_from_json("session-matrix", "proj\\matrix.json", &json);

        assert_eq!(items.len(), 4);
        assert_eq!(items[0].role, "author-role");
        assert_eq!(items[0].full_text.as_deref(), Some("Text value"));
        assert_eq!(items[1].role, "sender-role");
        assert_eq!(items[1].full_text.as_deref(), Some("Message value"));
        assert_eq!(items[2].role, "body-role");
        assert_eq!(items[2].full_text.as_deref(), Some("Body value"));
        assert_eq!(items[3].role, "outer-sender");
        assert_eq!(items[3].timestamp_ms, 14);
        assert_eq!(items[3].full_text.as_deref(), Some("Nested body value"));
    }

    #[test]
    fn events_key_matrix_rejects_empty_and_telemetry_without_readable_text() {
        let json = serde_json::json!({
            "events": [
                {"author": "author-role", "text": "   ", "timestamp_ms": 21},
                {"sender": "sender-role", "message": "", "timestamp_ms": 22},
                {"author": "body-role", "body": "\n\t", "timestamp_ms": 23},
                {"type": "tool_result", "payload": {"tool": "bash", "exit_code": 0}},
                {"type": "telemetry", "message": {"latency_ms": 123}}
            ]
        });

        let items = extract_conversation_items_from_json(
            "session-matrix-empty",
            "proj\\matrix-empty.json",
            &json,
        );

        assert!(items.is_empty());
    }

    #[test]
    fn events_array_prefers_nested_message_role_and_timestamp() {
        let json = serde_json::json!({
            "events": [
                {
                    "type": "message",
                    "role": "assistant",
                    "timestamp_ms": 999,
                    "message": {
                        "role": "user",
                        "timestamp_ms": 123,
                        "content": "Nested metadata wins"
                    }
                }
            ]
        });

        let items =
            extract_conversation_items_from_json("session-nested", "proj\\nested.json", &json);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].role, "user");
        assert_eq!(items[0].timestamp_ms, 123);
        assert_eq!(items[0].full_text.as_deref(), Some("Nested metadata wins"));
    }

    #[test]
    fn conversation_item_id_is_deterministic_hashed() {
        let json = serde_json::json!({
            "messages": [
                {"role": "assistant", "content": "Deterministic", "timestamp_ms": "1700000000000"}
            ]
        });

        let items_a =
            extract_conversation_items_from_json("session-hash", "proj\\path\\session.json", &json);
        let items_b =
            extract_conversation_items_from_json("session-hash", "proj/path/session.json", &json);

        assert_eq!(items_a.len(), 1);
        assert_eq!(items_b.len(), 1);
        assert_eq!(items_a[0].id, items_b[0].id);
        assert!(items_a[0].id.starts_with("conv-"));
        assert!(!items_a[0].id.contains("session-hash"));
        assert!(!items_a[0].id.contains("proj/path/session.json"));
        assert_eq!(items_a[0].id.len(), "conv-".len() + 16);
    }

    #[test]
    fn extract_conversation_items_from_source_text_supports_jsonl_lines() {
        let source_path = "projects/proj-jsonl/session.jsonl";
        let text = [
            serde_json::json!({
                "sessionId": "session-jsonl",
                "message": {
                    "role": "user",
                    "timestamp_ms": 1000,
                    "content": "first"
                }
            })
            .to_string(),
            "{\"sessionId\":\"session-jsonl\",\"message\":{\"role\":\"assistant\",\"timestamp_ms\":1001,\"content\":\"second\"}}".to_string(),
            serde_json::json!({
                "sessionId": "other-session",
                "message": {
                    "role": "assistant",
                    "timestamp_ms": 1002,
                    "content": "ignored"
                }
            })
            .to_string(),
            "{not-json".to_string(),
        ]
        .join("\n");

        let items = extract_conversation_items_from_source_text("session-jsonl", source_path, &text);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].role, "user");
        assert_eq!(items[0].timestamp_ms, 1000);
        assert_eq!(items[1].role, "assistant");
        assert_eq!(items[1].timestamp_ms, 1001);
        assert!(items.iter().all(|item| item.source_path.contains("session.jsonl")));
    }

    #[test]
    fn get_session_details_returns_none_presence_for_unknown_session() {
        let store = test_store();
        let details = store.get_session_details("missing-session", None, 100, false);

        assert_eq!(details.session_id, "missing-session");
        assert_eq!(details.presence, SessionPresence::None);
        assert!(details.project_paths.is_empty());
        assert!(details.selected_project_path.is_none());
        assert!(details.summary.last_proxy_activity_ms.is_none());
        assert!(details.summary.last_local_activity_ms.is_none());
        assert!(details.requests.is_empty());
        assert!(details.timeline.is_empty());
        assert!(details.conversation.is_empty());
    }

    #[test]
    fn get_session_details_computes_presence_from_proxy_and_local_union() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-session-details-presence-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let projects_dir = log_dir.join("projects");
        std::fs::create_dir_all(projects_dir.join("proj-local-only")).unwrap();
        std::fs::create_dir_all(projects_dir.join("proj-both")).unwrap();

        std::fs::write(
            projects_dir
                .join("proj-local-only")
                .join("session-local-only.json"),
            serde_json::json!({
                "session_id": "session-local-only",
                "messages": [
                    {"role": "user", "timestamp_ms": 10, "content": "hello"},
                    {"role": "assistant", "timestamp_ms": 20, "content": "world"}
                ]
            })
            .to_string(),
        )
        .unwrap();

        std::fs::write(
            projects_dir.join("proj-both").join("session-both.json"),
            serde_json::json!({
                "session_id": "session-both",
                "messages": [
                    {"role": "user", "timestamp_ms": 3100, "content": "one"},
                    {"role": "assistant", "timestamp_ms": 3200, "content": "two"}
                ]
            })
            .to_string(),
        )
        .unwrap();

        let store = StatsStore::new(100, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let mut proxy_only = sample_entry();
        proxy_only.id = "req-proxy-only-1".into();
        proxy_only.session_id = Some("session-proxy-only".into());
        proxy_only.timestamp =
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(1000).unwrap();
        store.add_entry(proxy_only);

        let mut both_a = sample_entry();
        both_a.id = "req-both-a".into();
        both_a.session_id = Some("session-both".into());
        both_a.timestamp = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(3000).unwrap();
        both_a.status = RequestStatus::ServerError(503);
        store.add_entry(both_a);

        let mut both_b = sample_entry();
        both_b.id = "req-both-b".into();
        both_b.session_id = Some("session-both".into());
        both_b.timestamp = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(2000).unwrap();
        both_b.stalls = vec![StallEvent {
            timestamp: both_b.timestamp,
            duration_s: 25.0,
            bytes_before: 64,
        }];
        store.add_entry(both_b);

        store.upsert_local_event(&LocalEvent {
            id: "evt-both-1".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "projects/proj-both/session-both.json".into(),
            event_time_ms: 3500,
            session_hint: Some("session-both".into()),
            event_kind: "project_metadata".into(),
            model_hint: None,
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({"size": 42}),
        });

        let proxy_only_details = store.get_session_details("session-proxy-only", None, 1, false);
        assert_eq!(proxy_only_details.presence, SessionPresence::ProxyOnly);
        assert_eq!(proxy_only_details.summary.proxy_request_count_total, 1);
        assert_eq!(proxy_only_details.summary.local_event_count_total, 0);
        assert_eq!(
            proxy_only_details.summary.conversation_message_count_total,
            0
        );
        assert_eq!(
            proxy_only_details.summary.last_proxy_activity_ms,
            Some(1000)
        );
        assert_eq!(proxy_only_details.summary.last_local_activity_ms, None);

        let local_only_details = store.get_session_details("session-local-only", None, 1, false);
        assert_eq!(local_only_details.presence, SessionPresence::LocalOnly);
        assert_eq!(local_only_details.summary.proxy_request_count_total, 0);
        assert_eq!(local_only_details.summary.local_event_count_total, 0);
        assert_eq!(
            local_only_details.summary.conversation_message_count_total,
            2
        );
        assert_eq!(local_only_details.summary.last_proxy_activity_ms, None);
        assert_eq!(local_only_details.summary.last_local_activity_ms, Some(20));

        let both_details = store.get_session_details("session-both", None, 1, false);
        assert_eq!(both_details.presence, SessionPresence::Both);
        assert_eq!(both_details.summary.proxy_request_count_total, 2);
        assert_eq!(both_details.summary.proxy_error_count_total, 1);
        assert_eq!(both_details.summary.proxy_stall_count_total, 1);
        assert_eq!(both_details.summary.local_event_count_total, 1);
        assert_eq!(both_details.summary.conversation_message_count_total, 2);
        assert_eq!(both_details.summary.last_proxy_activity_ms, Some(3000));
        assert_eq!(both_details.summary.last_local_activity_ms, Some(3500));

        assert_eq!(both_details.requests.len(), 1);
        assert_eq!(both_details.timeline.len(), 1);
        assert_eq!(both_details.conversation.len(), 1);

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn get_session_details_applies_per_section_order_and_limits() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-session-details-order-limit-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let projects_dir = log_dir.join("projects");
        std::fs::create_dir_all(projects_dir.join("proj-order")).unwrap();

        std::fs::write(
            projects_dir.join("proj-order").join("session-order.json"),
            serde_json::json!({
                "session_id": "session-order",
                "messages": [
                    {"role": "user", "timestamp_ms": 7100, "content": "first"},
                    {"role": "assistant", "timestamp_ms": 7100, "content": "second"},
                    {"role": "assistant", "timestamp_ms": 6000, "content": "third"}
                ]
            })
            .to_string(),
        )
        .unwrap();

        let store = StatsStore::new(100, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        for (id, ts_ms) in [
            ("req-a", 5000_i64),
            ("req-b", 5000_i64),
            ("req-c", 4000_i64),
        ] {
            let mut req = sample_entry();
            req.id = id.into();
            req.session_id = Some("session-order".into());
            req.timestamp = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ts_ms).unwrap();
            store.add_entry(req);
        }

        store.upsert_local_event(&LocalEvent {
            id: "evt-b".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "projects/proj-order/session-order.json".into(),
            event_time_ms: 5000,
            session_hint: Some("session-order".into()),
            event_kind: "alpha".into(),
            model_hint: None,
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({}),
        });
        store.upsert_local_event(&LocalEvent {
            id: "evt-a".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "projects/proj-order/session-order.json".into(),
            event_time_ms: 5000,
            session_hint: Some("session-order".into()),
            event_kind: "alpha".into(),
            model_hint: None,
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({}),
        });
        store.upsert_local_event(&LocalEvent {
            id: "evt-c".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "projects/proj-order/session-order.json".into(),
            event_time_ms: 4500,
            session_hint: Some("session-order".into()),
            event_kind: "beta".into(),
            model_hint: None,
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({}),
        });

        let details = store.get_session_details("session-order", None, 2, false);

        assert_eq!(details.summary.proxy_request_count_total, 3);
        assert_eq!(details.summary.local_event_count_total, 3);
        assert_eq!(details.summary.conversation_message_count_total, 3);

        assert_eq!(details.requests.len(), 2);
        assert!(details.truncated_sections.requests);
        assert_eq!(details.timeline.len(), 2);
        assert!(details.truncated_sections.timeline);
        assert_eq!(details.conversation.len(), 2);
        assert!(details.truncated_sections.conversation);

        for pair in details.requests.windows(2) {
            let left = &pair[0];
            let right = &pair[1];
            if left.timestamp_ms == right.timestamp_ms {
                assert!(left.id >= right.id, "request tie-break should be id DESC");
            } else {
                assert!(
                    left.timestamp_ms >= right.timestamp_ms,
                    "requests should be timestamp DESC"
                );
            }
        }

        for pair in details.timeline.windows(2) {
            let left = &pair[0];
            let right = &pair[1];
            if left.timestamp_ms != right.timestamp_ms {
                assert!(
                    left.timestamp_ms >= right.timestamp_ms,
                    "timeline should be timestamp DESC"
                );
            } else if left.kind != right.kind {
                assert!(
                    left.kind <= right.kind,
                    "timeline tie-break should be kind ASC"
                );
            } else {
                assert!(left.id <= right.id, "timeline tie-break should be id ASC");
            }
        }

        for pair in details.conversation.windows(2) {
            let left = &pair[0];
            let right = &pair[1];
            if left.timestamp_ms != right.timestamp_ms {
                assert!(
                    left.timestamp_ms >= right.timestamp_ms,
                    "conversation should be timestamp DESC"
                );
            } else {
                assert!(
                    left.id <= right.id,
                    "conversation tie-break should be id ASC"
                );
            }
        }

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn get_session_details_trims_payload_in_documented_order() {
        let mut response = SessionDetailsResponse {
            session_id: "session-trim".into(),
            presence: SessionPresence::Both,
            project_paths: vec!["proj-trim".into()],
            selected_project_path: Some("proj-trim".into()),
            summary: SessionDetailsSummary {
                proxy_request_count_total: 2,
                proxy_error_count_total: 0,
                proxy_stall_count_total: 0,
                local_event_count_total: 1,
                conversation_message_count_total: 2,
                last_proxy_activity_ms: Some(3000),
                last_local_activity_ms: Some(4000),
            },
            requests: vec![
                SessionRequestSummary {
                    id: "req-new".into(),
                    timestamp_ms: 3000,
                    status: Some("200".into()),
                    ttft_ms: Some(1.0),
                    duration_ms: Some(2.0),
                    path: Some("/v1/messages".into()),
                    model: Some("model-a".into()),
                },
                SessionRequestSummary {
                    id: "req-old".into(),
                    timestamp_ms: 2000,
                    status: Some("200".into()),
                    ttft_ms: Some(1.0),
                    duration_ms: Some(2.0),
                    path: Some("/v1/messages".into()),
                    model: Some("model-a".into()),
                },
            ],
            timeline: vec![
                SessionTimelineItem {
                    id: "timeline-new".into(),
                    timestamp_ms: 4000,
                    kind: "conversation".into(),
                    label: "new".into(),
                    request_id: None,
                    local_event_id: None,
                    conversation_id: Some("conv-new".into()),
                },
                SessionTimelineItem {
                    id: "timeline-old".into(),
                    timestamp_ms: 1000,
                    kind: "request".into(),
                    label: "old".into(),
                    request_id: Some("req-old".into()),
                    local_event_id: None,
                    conversation_id: None,
                },
            ],
            conversation: vec![
                SessionConversationItem {
                    id: "conv-new".into(),
                    timestamp_ms: 4000,
                    role: "assistant".into(),
                    preview: "preview-new".into(),
                    full_text: Some("N".repeat(MAX_FULL_TEXT_CHARS)),
                    text_truncated: true,
                    source_path: "projects/proj-trim/new.json".into(),
                    source_kind: "claude_project".into(),
                    expandable: true,
                },
                SessionConversationItem {
                    id: "conv-old".into(),
                    timestamp_ms: 1000,
                    role: "assistant".into(),
                    preview: "preview-old".into(),
                    full_text: Some("O".repeat(MAX_FULL_TEXT_CHARS)),
                    text_truncated: true,
                    source_path: "projects/proj-trim/old.json".into(),
                    source_kind: "claude_project".into(),
                    expandable: true,
                },
            ],
            truncated_sections: SessionTruncatedSections {
                requests: false,
                timeline: false,
                conversation: false,
                full_text: false,
            },
        };

        enforce_session_details_payload_cap(&mut response, 1_000, true);

        assert_eq!(response.timeline.len(), 1);
        assert_eq!(response.timeline[0].id, "timeline-new");

        assert_eq!(response.conversation.len(), 1);
        assert_eq!(response.conversation[0].id, "conv-new");

        assert_eq!(response.requests.len(), 1);
        assert_eq!(response.requests[0].id, "req-new");

        assert!(response.conversation[0].full_text.is_none());
        assert!(response.truncated_sections.timeline);
        assert!(response.truncated_sections.conversation);
        assert!(response.truncated_sections.requests);
        assert!(response.truncated_sections.full_text);
    }

    #[test]
    fn conversation_scan_stops_at_configured_bounds() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-session-details-scan-bounds-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let projects_dir = log_dir.join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();

        for i in 0..250_u32 {
            let project_name = format!("proj-{i:03}");
            let project_dir = projects_dir.join(&project_name);
            std::fs::create_dir_all(&project_dir).unwrap();
            std::fs::write(
                project_dir.join("session.json"),
                serde_json::json!({
                    "session_id": "session-scan",
                    "messages": [
                        {
                            "role": "assistant",
                            "timestamp_ms": 1_000_000_i64 + i as i64,
                            "content": format!("message-{i:03}")
                        }
                    ]
                })
                .to_string(),
            )
            .unwrap();
        }

        let store = StatsStore::new(100, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());
        let details = store.get_session_details("session-scan", None, 5_000, false);

        assert!(details.truncated_sections.conversation);
        assert_eq!(details.summary.conversation_message_count_total, 200);
        assert_eq!(details.conversation.len(), 200);
        assert_eq!(details.project_paths.len(), 200);
        assert!(details.project_paths.iter().any(|path| path == "proj-000"));
        assert!(details.project_paths.iter().any(|path| path == "proj-199"));
        assert!(!details.project_paths.iter().any(|path| path == "proj-200"));

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn initialize_database_creates_context_tables() {
        let log_dir =
            std::env::temp_dir().join(format!("claude-proxy-db-schema-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(10, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());
        let conn = store.db.lock();

        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

        for table in [
            "local_events",
            "request_correlations",
            "config_snapshots",
            "settings_history",
            "ingestion_checkpoints",
            "explanations",
        ] {
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(exists, 1, "missing table: {table}");
        }

        let idx_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_request_correlations_request_id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            idx_exists, 1,
            "missing index: idx_request_correlations_request_id"
        );

        let settings_idx_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_settings_history_saved_at_ms'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            settings_idx_exists, 1,
            "missing index: idx_settings_history_saved_at_ms"
        );

        let request_corr_foreign_keys: Vec<(String, String, String)> = conn
            .prepare("PRAGMA foreign_key_list(request_correlations)")
            .unwrap()
            .query_map([], |row| Ok((row.get(2)?, row.get(3)?, row.get(4)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert!(
            request_corr_foreign_keys
                .iter()
                .any(|(table, from, to)| table == "requests" && from == "request_id" && to == "id"),
            "missing FK request_correlations.request_id -> requests.id"
        );
        assert!(
            request_corr_foreign_keys
                .iter()
                .any(|(table, from, to)| table == "local_events"
                    && from == "local_event_id"
                    && to == "id"),
            "missing FK request_correlations.local_event_id -> local_events.id"
        );

        let explanations_foreign_keys: Vec<(String, String, String)> = conn
            .prepare("PRAGMA foreign_key_list(explanations)")
            .unwrap()
            .query_map([], |row| Ok((row.get(2)?, row.get(3)?, row.get(4)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert!(
            explanations_foreign_keys
                .iter()
                .any(|(table, from, to)| table == "requests" && from == "request_id" && to == "id"),
            "missing FK explanations.request_id -> requests.id"
        );

        drop(conn);

        let mut req = sample_entry();
        req.id = "req-schema-check".into();
        store.add_entry(req);

        let conn = store.db.lock();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

        conn.execute(
            "
            INSERT INTO local_events (
                id, source_kind, source_path, event_time_ms, session_hint, event_kind, model_hint, payload_policy, payload_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ",
            rusqlite::params![
                "evt-schema-check",
                "claude_project",
                "project/a/session.json",
                chrono::Utc::now().timestamp_millis(),
                Option::<String>::None,
                "file_updated",
                Option::<String>::None,
                "metadata_only",
                "{}"
            ],
        )
        .unwrap();

        let bad_corr_confidence = conn.execute(
            "
            INSERT INTO request_correlations (
                id, request_id, local_event_id, link_type, confidence, reason, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ",
            rusqlite::params![
                "corr-bad-confidence",
                "req-schema-check",
                "evt-schema-check",
                "session_hint",
                1.5,
                "invalid confidence",
                chrono::Utc::now().timestamp_millis()
            ],
        );
        assert!(
            bad_corr_confidence.is_err(),
            "request_correlations confidence should be constrained to [0,1]"
        );

        let bad_corr_fk = conn.execute(
            "
            INSERT INTO request_correlations (
                id, request_id, local_event_id, link_type, confidence, reason, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ",
            rusqlite::params![
                "corr-bad-fk",
                "missing-request",
                "evt-schema-check",
                "session_hint",
                0.5,
                "missing request fk",
                chrono::Utc::now().timestamp_millis()
            ],
        );
        assert!(
            bad_corr_fk.is_err(),
            "request_correlations.request_id should enforce FK to requests.id"
        );

        let bad_corr_link_type = conn.execute(
            "
            INSERT INTO request_correlations (
                id, request_id, local_event_id, link_type, confidence, reason, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ",
            rusqlite::params![
                "corr-bad-link-type",
                "req-schema-check",
                "evt-schema-check",
                "definitely_invalid",
                0.5,
                "invalid link type",
                chrono::Utc::now().timestamp_millis()
            ],
        );
        assert!(
            bad_corr_link_type.is_err(),
            "request_correlations.link_type should be constrained to known enum values"
        );

        let bad_explanation_confidence = conn.execute(
            "
            INSERT INTO explanations (
                id, request_id, anomaly_kind, rank, confidence, summary, evidence_json, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ",
            rusqlite::params![
                "exp-bad-confidence",
                "req-schema-check",
                "slow_ttft",
                1,
                -0.1,
                "invalid confidence",
                "{}",
                chrono::Utc::now().timestamp_millis()
            ],
        );
        assert!(
            bad_explanation_confidence.is_err(),
            "explanations confidence should be constrained to [0,1]"
        );

        let bad_explanation_rank = conn.execute(
            "
            INSERT INTO explanations (
                id, request_id, anomaly_kind, rank, confidence, summary, evidence_json, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ",
            rusqlite::params![
                "exp-bad-rank",
                "req-schema-check",
                "slow_ttft",
                -1,
                0.4,
                "invalid rank",
                "{}",
                chrono::Utc::now().timestamp_millis()
            ],
        );
        assert!(
            bad_explanation_rank.is_err(),
            "explanations rank should be constrained to be >= 0"
        );

        let bad_explanation_fk = conn.execute(
            "
            INSERT INTO explanations (
                id, request_id, anomaly_kind, rank, confidence, summary, evidence_json, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ",
            rusqlite::params![
                "exp-bad-fk",
                "missing-request",
                "slow_ttft",
                0,
                0.4,
                "missing request fk",
                "{}",
                chrono::Utc::now().timestamp_millis()
            ],
        );
        assert!(
            bad_explanation_fk.is_err(),
            "explanations.request_id should enforce FK to requests.id"
        );

        drop(conn);
        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn replace_explanations_for_request_replaces_existing_rows() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-explanations-replace-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(20, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let mut req = sample_entry();
        req.id = "req-replace-exp-1".into();
        store.add_entry(req);

        store.upsert_explanation(&Explanation {
            id: "exp-old-1".into(),
            request_id: "req-replace-exp-1".into(),
            anomaly_kind: "slow_ttft".into(),
            rank: 1,
            confidence: 0.5,
            summary: "old explanation".into(),
            evidence_json: serde_json::json!({"kind":"old"}),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        });

        store.replace_explanations_for_request(
            "req-replace-exp-1",
            &[Explanation {
                id: "exp-new-1".into(),
                request_id: "req-replace-exp-1".into(),
                anomaly_kind: "slow_ttft".into(),
                rank: 1,
                confidence: 0.9,
                summary: "new explanation".into(),
                evidence_json: serde_json::json!({"kind":"new"}),
                created_at_ms: chrono::Utc::now().timestamp_millis(),
            }],
        );

        let rows = store.get_explanations_for_request("req-replace-exp-1", 10);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "exp-new-1");
        assert_eq!(rows[0].summary, "new explanation");

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn persist_and_load_explanations_for_request() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-explanations-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(20, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let mut req = sample_entry();
        req.id = "req-1".into();
        store.add_entry(req);

        let explanation = Explanation {
            id: "exp-1".into(),
            request_id: "req-1".into(),
            anomaly_kind: "slow_ttft".into(),
            rank: 1,
            confidence: 0.88,
            summary: "Model changed 90s before TTFT spike".into(),
            evidence_json: serde_json::json!({"model_before":"haiku","model_after":"opus"}),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        };

        store.upsert_explanation(&explanation);
        let rows = store.get_explanations_for_request("req-1", 5);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "exp-1");
        assert_eq!(rows[0].request_id, "req-1");
        assert_eq!(rows[0].rank, 1);
        assert_eq!(rows[0].confidence, 0.88);
        assert_eq!(rows[0].summary, "Model changed 90s before TTFT spike");
        assert_eq!(
            rows[0].evidence_json,
            serde_json::json!({"model_before":"haiku","model_after":"opus"})
        );

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn upsert_local_event_preserves_request_correlations_and_updates_fields() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-local-event-upsert-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(50, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let mut request = sample_entry();
        request.id = "req-upsert-1".into();
        store.add_entry(request);

        let initial_event = LocalEvent {
            id: "evt-upsert-1".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "project/a/session.json".into(),
            event_time_ms: chrono::Utc::now().timestamp_millis(),
            session_hint: Some("session-upsert-1".into()),
            event_kind: "file_created".into(),
            model_hint: Some("claude-3-5-sonnet".into()),
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({"size": 10}),
        };
        store.upsert_local_event(&initial_event);

        {
            let conn = store.db.lock();
            conn.execute(
                "
                INSERT INTO request_correlations (
                    id, request_id, local_event_id, link_type, confidence, reason, created_at_ms
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ",
                rusqlite::params![
                    "corr-upsert-1",
                    "req-upsert-1",
                    "evt-upsert-1",
                    "session_hint",
                    0.95,
                    "session id match",
                    chrono::Utc::now().timestamp_millis(),
                ],
            )
            .unwrap();
        }

        let original_rowid: i64 = {
            let conn = store.db.lock();
            conn.query_row(
                "SELECT rowid FROM local_events WHERE id = ?1",
                ["evt-upsert-1"],
                |row| row.get(0),
            )
            .unwrap()
        };

        let updated_event = LocalEvent {
            id: "evt-upsert-1".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "project/a/session-updated.json".into(),
            event_time_ms: chrono::Utc::now().timestamp_millis() + 1,
            session_hint: Some("session-upsert-1".into()),
            event_kind: "file_updated".into(),
            model_hint: Some("claude-opus-4.6".into()),
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({"size": 42}),
        };
        store.upsert_local_event(&updated_event);

        let loaded = store.get_local_events(Some("session-upsert-1"), 10);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "evt-upsert-1");
        assert_eq!(loaded[0].source_path, "project/a/session-updated.json");
        assert_eq!(loaded[0].event_kind, "file_updated");
        assert_eq!(loaded[0].payload_json, serde_json::json!({"size": 42}));

        let updated_rowid: i64 = {
            let conn = store.db.lock();
            conn.query_row(
                "SELECT rowid FROM local_events WHERE id = ?1",
                ["evt-upsert-1"],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            updated_rowid, original_rowid,
            "upsert should update existing row without delete/reinsert"
        );

        let corr_count: i64 = {
            let conn = store.db.lock();
            conn.query_row(
                "SELECT COUNT(*) FROM request_correlations WHERE local_event_id = ?1",
                ["evt-upsert-1"],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            corr_count, 1,
            "request_correlations link should remain intact after local event upsert"
        );

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn replace_correlations_for_request_replaces_and_caps_links() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-correlation-replace-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(50, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let mut entry = sample_entry();
        entry.id = "req-replace-1".into();
        entry.session_id = Some("session-replace-1".into());
        store.add_entry(entry);

        let event_one = LocalEvent {
            id: "evt-replace-1".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "project/a/session.json".into(),
            event_time_ms: chrono::Utc::now().timestamp_millis(),
            session_hint: Some("session-replace-1".into()),
            event_kind: "session_touch".into(),
            model_hint: None,
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({}),
        };
        let event_two = LocalEvent {
            id: "evt-replace-2".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "project/a/session2.json".into(),
            event_time_ms: chrono::Utc::now().timestamp_millis(),
            session_hint: Some("session-replace-1".into()),
            event_kind: "session_touch".into(),
            model_hint: None,
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({}),
        };
        store.upsert_local_event(&event_one);
        store.upsert_local_event(&event_two);

        store.upsert_request_correlation(&RequestCorrelation {
            id: "corr-old-1".into(),
            request_id: "req-replace-1".into(),
            local_event_id: "evt-replace-1".into(),
            link_type: CorrelationLinkType::SessionHint,
            confidence: 0.9,
            reason: "old 1".into(),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        });
        store.upsert_request_correlation(&RequestCorrelation {
            id: "corr-old-2".into(),
            request_id: "req-replace-1".into(),
            local_event_id: "evt-replace-2".into(),
            link_type: CorrelationLinkType::Temporal,
            confidence: 0.7,
            reason: "old 2".into(),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        });

        store.replace_correlations_for_request(
            "req-replace-1",
            &[RequestCorrelation {
                id: "corr-new-1".into(),
                request_id: "req-replace-1".into(),
                local_event_id: "evt-replace-2".into(),
                link_type: CorrelationLinkType::SessionHint,
                confidence: 0.95,
                reason: "new best link".into(),
                created_at_ms: chrono::Utc::now().timestamp_millis(),
            }],
        );

        let links = store.get_correlations_for_request("req-replace-1", 20);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].id, "corr-new-1");
        assert_eq!(links[0].local_event_id, "evt-replace-2");

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn insert_correlation_and_query_by_request_id() {
        let log_dir =
            std::env::temp_dir().join(format!("claude-proxy-correlation-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(50, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let mut entry = sample_entry();
        entry.id = "req-1".into();
        entry.session_id = Some("session-1".into());
        store.add_entry(entry);

        let event = LocalEvent {
            id: "evt-1".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "project/a/session.json".into(),
            event_time_ms: chrono::Utc::now().timestamp_millis(),
            session_hint: Some("session-1".into()),
            event_kind: "session_touch".into(),
            model_hint: None,
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({}),
        };
        store.upsert_local_event(&event);

        store.upsert_request_correlation(&RequestCorrelation {
            id: "corr-1".into(),
            request_id: "req-1".into(),
            local_event_id: "evt-1".into(),
            link_type: CorrelationLinkType::SessionHint,
            confidence: 0.92,
            reason: "session id match".into(),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        });

        let links = store.get_correlations_for_request("req-1", 20);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].link_type, CorrelationLinkType::SessionHint);

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn build_debug_session_graph_links_requests_events_explanations() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-session-graph-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(50, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let mut request = sample_entry();
        request.id = "req-graph-1".into();
        request.session_id = Some("s-1".into());
        store.add_entry(request);

        store.upsert_local_event(&LocalEvent {
            id: "evt-graph-1".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "projects/demo/session.json".into(),
            event_time_ms: chrono::Utc::now().timestamp_millis(),
            session_hint: Some("s-1".into()),
            event_kind: "session_touch".into(),
            model_hint: None,
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({}),
        });

        store.upsert_request_correlation(&RequestCorrelation {
            id: "corr-graph-1".into(),
            request_id: "req-graph-1".into(),
            local_event_id: "evt-graph-1".into(),
            link_type: CorrelationLinkType::SessionHint,
            confidence: 0.93,
            reason: "session matched".into(),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        });

        store.upsert_explanation(&Explanation {
            id: "exp-graph-1".into(),
            request_id: "req-graph-1".into(),
            anomaly_kind: "slow_ttft".into(),
            rank: 1,
            confidence: 0.88,
            summary: "Likely related to recent config drift".into(),
            evidence_json: serde_json::json!({"hint":"config"}),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        });

        let graph = store.get_session_graph("s-1", 200).unwrap();

        assert!(graph.nodes.iter().any(|n| n.kind == "request"));
        assert!(graph.nodes.iter().any(|n| n.kind == "local_event"));
        assert!(graph.nodes.iter().any(|n| n.kind == "explanation"));
        assert!(!graph.edges.is_empty());

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn upsert_local_event_reports_failure_when_sqlite_write_fails() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-local-event-fail-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(50, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());
        {
            let conn = store.db.lock();
            conn.execute("DROP TABLE local_events", []).unwrap();
        }

        let event = LocalEvent {
            id: "evt-fail-1".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "project/a/session.json".into(),
            event_time_ms: chrono::Utc::now().timestamp_millis(),
            session_hint: Some("session-fail-1".into()),
            event_kind: "file_updated".into(),
            model_hint: Some("claude-opus-4.6".into()),
            payload_policy: crate::correlation::PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({"size": 120}),
        };

        let persisted = store.upsert_local_event(&event);
        assert!(
            !persisted,
            "upsert should report false when SQLite write fails"
        );

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn upsert_local_event_and_checkpoint_round_trip() {
        let log_dir =
            std::env::temp_dir().join(format!("claude-proxy-local-event-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(50, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());
        let event = LocalEvent {
            id: "evt-1".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "project/a/session.json".into(),
            event_time_ms: chrono::Utc::now().timestamp_millis(),
            session_hint: Some("session-1".into()),
            event_kind: "file_updated".into(),
            model_hint: Some("claude-opus-4.6".into()),
            payload_policy: crate::correlation::PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({"size": 120}),
        };

        store.upsert_local_event(&event);
        store.set_ingestion_checkpoint(SourceKind::ClaudeProject, "checkpoint-42");

        let loaded = store.get_local_events(Some("session-1"), 10);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "evt-1");
        assert_eq!(
            store
                .get_ingestion_checkpoint(SourceKind::ClaudeProject)
                .as_deref(),
            Some("checkpoint-42")
        );

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn add_entry_broadcasts_entry_and_stats_messages() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-test-broadcast-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(100, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());
        let mut rx = store.broadcast_tx.subscribe();

        store.add_entry(sample_entry());

        let mut messages = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(msg) => messages.push(msg),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
            }
        }

        let entry_index = messages
            .iter()
            .position(|msg| msg.contains("\"type\":\"entry\""))
            .expect("missing entry broadcast message");
        let stats_index = messages
            .iter()
            .position(|msg| msg.contains("\"type\":\"stats\""))
            .expect("missing stats broadcast message");

        assert!(
            entry_index < stats_index,
            "entry message should be broadcast before stats message"
        );

        let entry_msg: serde_json::Value = serde_json::from_str(&messages[entry_index])
            .expect("entry broadcast must be valid JSON");
        let stats_msg: serde_json::Value = serde_json::from_str(&messages[stats_index])
            .expect("stats broadcast must be valid JSON");

        assert_eq!(
            entry_msg.get("type").and_then(|v| v.as_str()),
            Some("entry")
        );
        assert_eq!(
            stats_msg.get("type").and_then(|v| v.as_str()),
            Some("stats")
        );
        assert_eq!(
            entry_msg
                .get("data")
                .and_then(|v| v.get("id"))
                .and_then(|v| v.as_str()),
            Some("test-entry")
        );

        let total_requests = stats_msg
            .get("data")
            .and_then(|v| v.get("stats"))
            .and_then(|v| v.get("total_requests"))
            .and_then(|v| v.as_u64())
            .expect("stats broadcast should include total_requests");
        assert!(
            total_requests >= 1,
            "stats broadcast should include newly added request"
        );

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn search_unknown_matches_entries_without_session_id() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-test-unknown-search-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(100, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let mut entry = sample_entry();
        entry.id = "unknown-session-entry".into();
        entry.session_id = None;
        store.add_entry(entry);

        let sessions = store.get_sessions();
        assert!(sessions
            .iter()
            .any(|session| session.session_id == "unknown"));

        let filter = EntryFilter {
            search: Some("unknown".into()),
            ..EntryFilter::default()
        };
        let matches = store.get_entries(10, 0, &filter, None, None);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "unknown-session-entry");

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn clear_stats_resets_live_and_volatile_tables() {
        let log_dir =
            std::env::temp_dir().join(format!("claude-proxy-test-memory-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(100, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let mut entry = sample_entry();
        entry.id = "req-clear-stats-1".into();
        entry.session_id = Some("session-clear-stats-1".into());
        store.add_entry(entry);
        store.write_body("req-clear-stats-1", "request-body", "response-body");

        let event = LocalEvent {
            id: "evt-clear-stats-1".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "projects/demo/session.json".into(),
            event_time_ms: chrono::Utc::now().timestamp_millis(),
            session_hint: Some("session-clear-stats-1".into()),
            event_kind: "session_touch".into(),
            model_hint: Some("claude-opus-4.6".into()),
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({"k":"v"}),
        };
        assert!(store.upsert_local_event(&event));

        store.upsert_request_correlation(&RequestCorrelation {
            id: "corr-clear-stats-1".into(),
            request_id: "req-clear-stats-1".into(),
            local_event_id: "evt-clear-stats-1".into(),
            link_type: CorrelationLinkType::SessionHint,
            confidence: 0.9,
            reason: "session matched".into(),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        });

        store.upsert_explanation(&Explanation {
            id: "exp-clear-stats-1".into(),
            request_id: "req-clear-stats-1".into(),
            anomaly_kind: "slow_ttft".into(),
            rank: 0,
            confidence: 0.8,
            summary: "explanation".into(),
            evidence_json: serde_json::json!({"hint":"x"}),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        });

        store.set_ingestion_checkpoint(SourceKind::ClaudeProject, "checkpoint-clear-stats");

        {
            let conn = store.db.lock();
            conn.execute(
                "INSERT INTO config_snapshots (id, source_path, content_hash, captured_at_ms, payload_policy, payload_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    "cfg-clear-stats-1",
                    "settings.json",
                    "hash-clear-stats",
                    chrono::Utc::now().timestamp_millis(),
                    "metadata_only",
                    "{}"
                ],
            )
            .unwrap();
        }

        assert!(
            store.insert_settings_history_snapshot(&SettingsHistoryItem {
                id: "settings-clear-stats".into(),
                saved_at_ms: 1_700_000_000_002,
                content_hash: "hash-clear-stats".into(),
                settings_json: "{\"clear\":\"stats\"}".into(),
                source: "admin".into(),
            })
        );

        let table_count = |table: &str| -> i64 {
            let conn = store.db.lock();
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
        };

        assert_eq!(store.persisted_entry_count(), 1);
        assert!(store.get_body("req-clear-stats-1").is_some());
        assert_eq!(table_count("local_events"), 1);
        assert_eq!(table_count("request_correlations"), 1);
        assert_eq!(table_count("explanations"), 1);
        assert_eq!(table_count("ingestion_checkpoints"), 1);
        assert_eq!(table_count("config_snapshots"), 1);
        assert_eq!(table_count("settings_history"), 1);

        let summary = store.clear_stats();

        assert!(matches!(summary.mode, ClearMode::MemoryOnly));
        assert_eq!(summary.cleared_entries, 1);
        assert_eq!(summary.deleted_persisted_entries, 0);
        assert_eq!(store.get_live_stats().total_requests, 0);
        assert_eq!(
            store
                .get_entries(10, 0, &EntryFilter::default(), None, None)
                .len(),
            1
        );
        assert_eq!(store.persisted_entry_count(), 1);
        assert!(store.get_body("req-clear-stats-1").is_some());
        assert_eq!(table_count("local_events"), 0);
        assert_eq!(table_count("request_correlations"), 0);
        assert_eq!(table_count("explanations"), 0);
        assert_eq!(table_count("ingestion_checkpoints"), 0);
        assert_eq!(table_count("config_snapshots"), 0);
        assert_eq!(table_count("settings_history"), 1);
        assert!(has_database_file(&log_dir));

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn clear_all_clears_all_db_tables_without_fs_delete() {
        let log_dir =
            std::env::temp_dir().join(format!("claude-proxy-test-{}", uuid::Uuid::new_v4()));
        let claude_dir = log_dir.join(".claude");
        let projects_dir = claude_dir.join("projects");
        let project_dir = projects_dir.join("proj-reset-keep");
        std::fs::create_dir_all(&project_dir).unwrap();
        let marker_file = project_dir.join("keep.txt");
        std::fs::write(&marker_file, "keep").unwrap();
        let legacy_proxy_log = log_dir.join("proxy-legacy.jsonl");
        std::fs::write(&legacy_proxy_log, "{}\n").unwrap();

        let store = StatsStore::new(
            100,
            log_dir.clone(),
            20.0,
            8.0,
            2_097_152,
            claude_dir.clone(),
        );

        let mut entry = sample_entry();
        entry.id = "req-clear-all-1".into();
        entry.session_id = Some("session-clear-all-1".into());
        store.add_entry(entry);
        store.write_body("req-clear-all-1", "request-body", "response-body");

        let event = LocalEvent {
            id: "evt-clear-all-1".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "projects/demo/session.json".into(),
            event_time_ms: chrono::Utc::now().timestamp_millis(),
            session_hint: Some("session-clear-all-1".into()),
            event_kind: "session_touch".into(),
            model_hint: Some("claude-opus-4.6".into()),
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({"k":"v"}),
        };
        assert!(store.upsert_local_event(&event));

        store.upsert_request_correlation(&RequestCorrelation {
            id: "corr-clear-all-1".into(),
            request_id: "req-clear-all-1".into(),
            local_event_id: "evt-clear-all-1".into(),
            link_type: CorrelationLinkType::SessionHint,
            confidence: 0.9,
            reason: "session matched".into(),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        });

        store.upsert_explanation(&Explanation {
            id: "exp-clear-all-1".into(),
            request_id: "req-clear-all-1".into(),
            anomaly_kind: "slow_ttft".into(),
            rank: 0,
            confidence: 0.8,
            summary: "explanation".into(),
            evidence_json: serde_json::json!({"hint":"x"}),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        });

        store.set_ingestion_checkpoint(SourceKind::ClaudeProject, "checkpoint-clear-all");

        {
            let conn = store.db.lock();
            conn.execute(
                "INSERT INTO config_snapshots (id, source_path, content_hash, captured_at_ms, payload_policy, payload_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    "cfg-clear-all-1",
                    "settings.json",
                    "hash-clear-all",
                    chrono::Utc::now().timestamp_millis(),
                    "metadata_only",
                    "{}"
                ],
            )
            .unwrap();
        }

        assert!(
            store.insert_settings_history_snapshot(&SettingsHistoryItem {
                id: "settings-clear-all".into(),
                saved_at_ms: 1_700_000_000_001,
                content_hash: "hash-clear-all".into(),
                settings_json: "{\"clear\":\"all\"}".into(),
                source: "admin".into(),
            })
        );

        let table_count = |table: &str| -> i64 {
            let conn = store.db.lock();
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
        };

        assert_eq!(table_count("requests"), 1);
        assert_eq!(table_count("request_bodies"), 1);
        assert_eq!(table_count("local_events"), 1);
        assert_eq!(table_count("request_correlations"), 1);
        assert_eq!(table_count("explanations"), 1);
        assert_eq!(table_count("ingestion_checkpoints"), 1);
        assert_eq!(table_count("config_snapshots"), 1);
        assert_eq!(table_count("settings_history"), 1);

        let summary = store.clear_all();

        assert!(matches!(summary.mode, ClearMode::StatsAndLogs));
        assert_eq!(summary.cleared_entries, 1);
        assert!(summary.deleted_persisted_entries >= 1);
        assert_eq!(store.get_live_stats().total_requests, 0);
        assert!(store
            .get_entries(10, 0, &EntryFilter::default(), None, None)
            .is_empty());
        assert_eq!(store.persisted_entry_count(), 0);
        assert_eq!(table_count("requests"), 0);
        assert_eq!(table_count("request_bodies"), 0);
        assert_eq!(table_count("local_events"), 0);
        assert_eq!(table_count("request_correlations"), 0);
        assert_eq!(table_count("explanations"), 0);
        assert_eq!(table_count("ingestion_checkpoints"), 0);
        assert_eq!(table_count("config_snapshots"), 0);
        assert_eq!(table_count("settings_history"), 0);
        assert!(projects_dir.exists());
        assert!(project_dir.exists());
        assert!(marker_file.exists());
        assert!(legacy_proxy_log.exists());
        assert!(has_database_file(&log_dir));

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn load_from_db_restores_recent_entries_up_to_max_entries() {
        let log_dir =
            std::env::temp_dir().join(format!("claude-proxy-test-reload-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();

        {
            let store = StatsStore::new(10, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

            let mut first = sample_entry();
            first.id = "entry-1".into();
            first.timestamp = Utc::now() - chrono::Duration::seconds(10);
            first.model = "claude-older".into();
            store.add_entry(first);

            let mut second = sample_entry();
            second.id = "entry-2".into();
            second.timestamp = Utc::now();
            second.model = "claude-newer".into();
            store.add_entry(second);

            assert_eq!(store.persisted_entry_count(), 2);
        }

        let store = StatsStore::new(1, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());
        store.load_from_db();

        let entries = store.get_entries(10, 0, &EntryFilter::default(), None, None);
        assert_eq!(store.persisted_entry_count(), 2);
        assert_eq!(store.get_live_stats().total_requests, 1);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "entry-2");
        assert_eq!(entries[0].model, "claude-newer");

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn get_entries_reads_full_history_from_sqlite_beyond_memory_window() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-test-history-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(1, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        for (idx, model) in ["claude-a", "claude-b", "claude-c"].into_iter().enumerate() {
            let mut entry = sample_entry();
            entry.id = format!("history-entry-{idx}");
            entry.timestamp = Utc::now() + chrono::Duration::seconds(idx as i64);
            entry.model = model.into();
            store.add_entry(entry);
        }

        assert_eq!(store.get_live_stats().total_requests, 1);

        let entries = store.get_entries(10, 0, &EntryFilter::default(), None, None);
        let models: Vec<_> = entries.iter().map(|entry| entry.model.as_str()).collect();

        assert_eq!(entries.len(), 3);
        assert_eq!(models, vec!["claude-c", "claude-b", "claude-a"]);

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn get_entries_filters_and_paginates_full_sqlite_history() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-test-filtered-history-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(1, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let mut older = sample_entry();
        older.id = "older-stall".into();
        older.timestamp = Utc::now() - chrono::Duration::seconds(2);
        older.model = "claude-filter".into();
        older.error = Some("Forbidden access".into());
        older.status = RequestStatus::ClientError(403);
        older.stalls = vec![StallEvent {
            timestamp: older.timestamp,
            duration_s: 21.0,
            bytes_before: 100,
        }];
        store.add_entry(older);

        let mut newer = sample_entry();
        newer.id = "newer-stall".into();
        newer.timestamp = Utc::now() - chrono::Duration::seconds(1);
        newer.model = "claude-filter".into();
        newer.error = Some("Forbidden again".into());
        newer.status = RequestStatus::ClientError(403);
        newer.stalls = vec![StallEvent {
            timestamp: newer.timestamp,
            duration_s: 22.0,
            bytes_before: 200,
        }];
        store.add_entry(newer);

        let mut success = sample_entry();
        success.id = "plain-success".into();
        success.timestamp = Utc::now();
        success.model = "claude-other".into();
        store.add_entry(success);

        let filter = EntryFilter {
            search: Some("forbidden".into()),
            status: Some("stall".into()),
            model: Some("claude-filter".into()),
            ..EntryFilter::default()
        };

        let first_page = store.get_entries(1, 0, &filter, None, None);
        let second_page = store.get_entries(1, 1, &filter, None, None);

        assert_eq!(first_page.len(), 1);
        assert_eq!(second_page.len(), 1);
        assert_eq!(first_page[0].id, "newer-stall");
        assert_eq!(second_page[0].id, "older-stall");

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn get_entries_with_anomaly_focus_filters_to_window() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-test-anomaly-focus-window-filter-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(1, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());
        let anomaly_ts = Utc::now();

        let mut nearest = sample_entry();
        nearest.id = "within-nearest".into();
        nearest.timestamp = anomaly_ts + chrono::Duration::milliseconds(150);
        store.add_entry(nearest);

        let mut within = sample_entry();
        within.id = "within-second".into();
        within.timestamp = anomaly_ts - chrono::Duration::milliseconds(900);
        store.add_entry(within);

        let mut outside = sample_entry();
        outside.id = "outside-window".into();
        outside.timestamp = anomaly_ts + chrono::Duration::milliseconds(1_500);
        store.add_entry(outside);

        let focused = store.get_entries_with_anomaly_focus(
            10,
            0,
            &EntryFilter::default(),
            anomaly_ts.timestamp_millis(),
            1_000,
        );

        let ids: Vec<_> = focused
            .entries
            .iter()
            .map(|entry| entry.entry.id.as_str())
            .collect();

        assert_eq!(ids, vec!["within-nearest", "within-second"]);
        assert!(focused
            .entries
            .iter()
            .all(|entry| entry.within_window == Some(true)));
        assert!(focused
            .entries
            .iter()
            .all(|entry| entry.distance_ms.is_some_and(|distance| distance <= 1_000)));
        assert_eq!(
            focused.preselected_request_id.as_deref(),
            Some("within-nearest")
        );

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn get_entries_with_anomaly_focus_orders_by_distance_then_timestamp_then_id() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-test-anomaly-focus-order-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(1, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let anomaly_ts = Utc::now();

        let mut closest = sample_entry();
        closest.id = "closest".into();
        closest.timestamp = anomaly_ts + chrono::Duration::milliseconds(500);
        store.add_entry(closest);

        let mut tie_a = sample_entry();
        tie_a.id = "tie-a".into();
        tie_a.timestamp = anomaly_ts + chrono::Duration::milliseconds(1_000);
        store.add_entry(tie_a);

        let mut tie_b = sample_entry();
        tie_b.id = "tie-b".into();
        tie_b.timestamp = anomaly_ts + chrono::Duration::milliseconds(1_000);
        store.add_entry(tie_b);

        let mut older = sample_entry();
        older.id = "older".into();
        older.timestamp = anomaly_ts - chrono::Duration::milliseconds(1_000);
        store.add_entry(older);

        let focused = store.get_entries_with_anomaly_focus(
            10,
            0,
            &EntryFilter::default(),
            anomaly_ts.timestamp_millis(),
            120_000,
        );

        let ordered_ids: Vec<_> = focused
            .entries
            .iter()
            .map(|entry| entry.entry.id.as_str())
            .collect();

        assert_eq!(ordered_ids, vec!["closest", "tie-b", "tie-a", "older"]);
        assert_eq!(
            focused.preselected_request_id.as_deref(),
            ordered_ids.first().copied()
        );

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn get_entries_with_anomaly_focus_paginates_ranked_results() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-test-anomaly-focus-pagination-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(1, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let anomaly_ts = Utc::now();

        let mut nearest = sample_entry();
        nearest.id = "rank-1".into();
        nearest.timestamp = anomaly_ts + chrono::Duration::milliseconds(100);
        store.add_entry(nearest);

        let mut second = sample_entry();
        second.id = "rank-2".into();
        second.timestamp = anomaly_ts - chrono::Duration::milliseconds(300);
        store.add_entry(second);

        let mut third = sample_entry();
        third.id = "rank-3".into();
        third.timestamp = anomaly_ts + chrono::Duration::milliseconds(700);
        store.add_entry(third);

        let page = store.get_entries_with_anomaly_focus(
            1,
            1,
            &EntryFilter::default(),
            anomaly_ts.timestamp_millis(),
            120_000,
        );

        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].entry.id, "rank-2");
        assert_eq!(page.preselected_request_id.as_deref(), Some("rank-1"));

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn get_entries_with_anomaly_focus_applies_filter_before_pagination_and_keeps_preselection_stable(
    ) {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-test-anomaly-focus-filter-pagination-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(1, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());
        let anomaly_ts = Utc::now();

        let mut err_nearest = sample_entry();
        err_nearest.id = "err-nearest".into();
        err_nearest.timestamp = anomaly_ts + chrono::Duration::milliseconds(100);
        err_nearest.status = RequestStatus::ServerError(500);
        store.add_entry(err_nearest);

        let mut err_second = sample_entry();
        err_second.id = "err-second".into();
        err_second.timestamp = anomaly_ts - chrono::Duration::milliseconds(200);
        err_second.status = RequestStatus::ClientError(429);
        store.add_entry(err_second);

        let mut err_third = sample_entry();
        err_third.id = "err-third".into();
        err_third.timestamp = anomaly_ts + chrono::Duration::milliseconds(300);
        err_third.status = RequestStatus::Timeout;
        store.add_entry(err_third);

        let mut success_closer = sample_entry();
        success_closer.id = "success-closer".into();
        success_closer.timestamp = anomaly_ts + chrono::Duration::milliseconds(50);
        success_closer.status = RequestStatus::Success(200);
        store.add_entry(success_closer);

        let mut err_outside_window = sample_entry();
        err_outside_window.id = "err-outside-window".into();
        err_outside_window.timestamp = anomaly_ts + chrono::Duration::milliseconds(10_000);
        err_outside_window.status = RequestStatus::ServerError(503);
        store.add_entry(err_outside_window);

        let filter = EntryFilter {
            status: Some("error".into()),
            ..EntryFilter::default()
        };

        let focused = store.get_entries_with_anomaly_focus(
            1,
            1,
            &filter,
            anomaly_ts.timestamp_millis(),
            1_000,
        );

        assert_eq!(focused.entries.len(), 1);
        assert_eq!(focused.entries[0].entry.id, "err-second");
        assert_eq!(focused.entries[0].distance_ms, Some(200));
        assert_eq!(focused.entries[0].within_window, Some(true));
        assert_eq!(
            focused.preselected_request_id.as_deref(),
            Some("err-nearest")
        );

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn get_claude_sessions_aggregates_project_paths_for_same_session_id() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-claude-sessions-aggregate-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let projects_dir = log_dir.join("projects");
        let alpha_dir = projects_dir.join("proj-alpha");
        let zeta_dir = projects_dir.join("proj-zeta");
        std::fs::create_dir_all(&alpha_dir).unwrap();
        std::fs::create_dir_all(&zeta_dir).unwrap();

        std::fs::write(
            alpha_dir.join("session.json"),
            serde_json::json!({
                "session_id": "session-shared",
                "messages": [
                    {"role": "user", "timestamp_ms": 1000, "content": "alpha"}
                ]
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            zeta_dir.join("session.json"),
            serde_json::json!({
                "session_id": "session-shared",
                "messages": [
                    {"role": "assistant", "timestamp_ms": 2000, "content": "zeta"}
                ]
            })
            .to_string(),
        )
        .unwrap();

        let store = StatsStore::new(100, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());
        let sessions = store.get_claude_sessions();

        let shared = sessions
            .iter()
            .find(|session| session.session_id == "session-shared")
            .expect("expected aggregated shared session");

        assert_eq!(shared.project_paths, vec!["proj-alpha", "proj-zeta"]);
        assert_eq!(shared.project_path, "proj-alpha");

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn session_discovery_includes_nested_json() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-session-discovery-nested-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let nested_dir = log_dir
            .join("projects")
            .join("proj-nested")
            .join("artifacts")
            .join("sessions");
        std::fs::create_dir_all(&nested_dir).unwrap();
        std::fs::write(
            nested_dir.join("nested-session.json"),
            serde_json::json!({
                "session_id": "session-nested-json",
                "messages": [
                    {"role": "assistant", "timestamp_ms": 1000, "content": "nested"}
                ]
            })
            .to_string(),
        )
        .unwrap();

        let store = StatsStore::new(100, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());
        let sessions = store.get_claude_sessions();

        let nested = sessions
            .iter()
            .find(|session| session.session_id == "session-nested-json")
            .expect("expected nested artifact session to be discovered");
        assert!(nested
            .project_paths
            .iter()
            .any(|path| path == "proj-nested"));

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn session_discovery_accepts_session_key_variants() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-session-discovery-key-variants-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let projects_dir = log_dir.join("projects");
        let camel_dir = projects_dir.join("proj-camel");
        let snake_dir = projects_dir.join("proj-snake");
        let plain_dir = projects_dir.join("proj-plain");
        std::fs::create_dir_all(&camel_dir).unwrap();
        std::fs::create_dir_all(&snake_dir).unwrap();
        std::fs::create_dir_all(&plain_dir).unwrap();

        std::fs::write(
            camel_dir.join("session-camel.json"),
            serde_json::json!({ "sessionId": "session-camel" }).to_string(),
        )
        .unwrap();
        std::fs::write(
            snake_dir.join("session-snake.json"),
            serde_json::json!({ "session_id": "session-snake" }).to_string(),
        )
        .unwrap();
        std::fs::write(
            plain_dir.join("session-plain.json"),
            serde_json::json!({ "session": "session-plain" }).to_string(),
        )
        .unwrap();

        let store = StatsStore::new(100, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());
        let sessions = store.get_claude_sessions();

        assert!(sessions
            .iter()
            .any(|session| session.session_id == "session-camel"));
        assert!(sessions
            .iter()
            .any(|session| session.session_id == "session-snake"));
        assert!(sessions
            .iter()
            .any(|session| session.session_id == "session-plain"));

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn session_discovery_root_jsonl_discovers_sessions_with_timestamp_variants_and_malformed_lines()
    {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-session-discovery-root-jsonl-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let projects_dir = log_dir.join("projects");
        let camel_dir = projects_dir.join("proj-root-jsonl-camel");
        let snake_dir = projects_dir.join("proj-root-jsonl-snake");
        std::fs::create_dir_all(&camel_dir).unwrap();
        std::fs::create_dir_all(&snake_dir).unwrap();

        let iso_older = "2026-03-15T10:00:00Z";
        let iso_newer = "2026-03-15T10:05:00Z";
        let epoch_older = 1_773_571_200_000i64;
        let epoch_newer = 1_773_571_560_000i64;

        let camel_jsonl = [
            serde_json::json!({
                "sessionId": "session-root-jsonl-camel",
                "timestamp": iso_older,
                "role": "user",
                "content": "hello"
            })
            .to_string(),
            "{\"sessionId\":\"session-root-jsonl-camel\",\"timestamp\":\"bad-ts\"".to_string(),
            serde_json::json!({
                "sessionId": "session-root-jsonl-camel",
                "timestamp": iso_newer,
                "role": "assistant",
                "content": "world"
            })
            .to_string(),
        ]
        .join("\n");

        let snake_jsonl = [
            serde_json::json!({
                "session_id": "session-root-jsonl-snake",
                "timestamp": epoch_older,
                "role": "user",
                "content": "alpha"
            })
            .to_string(),
            "not-json".to_string(),
            serde_json::json!({
                "session_id": "session-root-jsonl-snake",
                "timestamp": epoch_newer,
                "role": "assistant",
                "content": "beta"
            })
            .to_string(),
        ]
        .join("\n");

        std::fs::write(
            camel_dir.join("session-root-jsonl-camel.jsonl"),
            camel_jsonl,
        )
        .unwrap();
        std::fs::write(
            snake_dir.join("session-root-jsonl-snake.jsonl"),
            snake_jsonl,
        )
        .unwrap();

        let store = StatsStore::new(100, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let discovered_once = store.get_claude_sessions();
        let discovered_twice = store.get_claude_sessions();

        let camel_once = discovered_once
            .iter()
            .find(|session| session.session_id == "session-root-jsonl-camel")
            .expect("expected root jsonl session with sessionId key");
        let camel_twice = discovered_twice
            .iter()
            .find(|session| session.session_id == "session-root-jsonl-camel")
            .expect("expected root jsonl session with sessionId key on repeated read");
        assert!(camel_once.has_local_evidence);
        assert_eq!(camel_once.last_local_activity_ms, camel_twice.last_local_activity_ms);

        let snake_once = discovered_once
            .iter()
            .find(|session| session.session_id == "session-root-jsonl-snake")
            .expect("expected root jsonl session with session_id key");
        let snake_twice = discovered_twice
            .iter()
            .find(|session| session.session_id == "session-root-jsonl-snake")
            .expect("expected root jsonl session with session_id key on repeated read");
        assert!(snake_once.has_local_evidence);
        assert_eq!(snake_once.last_local_activity_ms, snake_twice.last_local_activity_ms);

        let iso_newer_ms = chrono::DateTime::parse_from_rfc3339(iso_newer)
            .unwrap()
            .with_timezone(&chrono::Utc)
            .timestamp_millis();
        assert!(camel_once.last_local_activity_ms >= iso_newer_ms);
        assert!(snake_once.last_local_activity_ms >= epoch_newer);

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn session_discovery_mixed_json_and_root_jsonl_keeps_both_formats() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-session-discovery-mixed-formats-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let projects_dir = log_dir.join("projects");
        let mixed_dir = projects_dir.join("proj-mixed-formats");
        std::fs::create_dir_all(&mixed_dir).unwrap();

        std::fs::write(
            mixed_dir.join("session-artifact.json"),
            serde_json::json!({
                "session_id": "session-json-artifact",
                "messages": [
                    {"role": "assistant", "timestamp_ms": 1_773_571_000_000i64, "content": "artifact"}
                ]
            })
            .to_string(),
        )
        .unwrap();

        let root_jsonl = [
            serde_json::json!({
                "sessionId": "session-root-jsonl-mixed",
                "timestamp": "2026-03-15T11:00:00Z",
                "role": "user",
                "content": "mixed"
            })
            .to_string(),
            "{\"sessionId\":\"session-root-jsonl-mixed\"".to_string(),
            serde_json::json!({
                "sessionId": "session-root-jsonl-mixed",
                "timestamp": "2026-03-15T11:10:00Z",
                "role": "assistant",
                "content": "formats"
            })
            .to_string(),
        ]
        .join("\n");
        std::fs::write(mixed_dir.join("session-root-jsonl-mixed.jsonl"), root_jsonl).unwrap();

        let store = StatsStore::new(100, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());
        let sessions = store.get_claude_sessions();

        let artifact = sessions
            .iter()
            .find(|session| session.session_id == "session-json-artifact")
            .expect("expected json artifact session to be discovered");
        assert!(artifact.has_local_evidence);

        let root_jsonl_session = sessions
            .iter()
            .find(|session| session.session_id == "session-root-jsonl-mixed")
            .expect("expected root jsonl session to be discovered");
        assert!(root_jsonl_session.has_local_evidence);

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn get_claude_sessions_marks_local_when_any_local_evidence_exists() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-claude-sessions-local-union-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let projects_dir = log_dir.join("projects");
        let artifact_dir = projects_dir.join("proj-artifact");
        std::fs::create_dir_all(&artifact_dir).unwrap();

        std::fs::write(
            artifact_dir.join("session-artifact.json"),
            serde_json::json!({
                "session_id": "session-artifact",
                "messages": [
                    {"role": "assistant", "timestamp_ms": 1234, "content": "artifact"}
                ]
            })
            .to_string(),
        )
        .unwrap();

        let store = StatsStore::new(100, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        store.upsert_local_event(&LocalEvent {
            id: "evt-local-only".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "projects/proj-event/session-local-only.json".into(),
            event_time_ms: 7777,
            session_hint: Some("session-local-only".into()),
            event_kind: "session_touch".into(),
            model_hint: None,
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({}),
        });

        let sessions = store.get_claude_sessions();

        let artifact = sessions
            .iter()
            .find(|session| session.session_id == "session-artifact")
            .expect("missing artifact-backed session");
        assert!(artifact.has_local_evidence);

        let local_only = sessions
            .iter()
            .find(|session| session.session_id == "session-local-only")
            .expect("missing local-event-backed session");
        assert!(local_only.has_local_evidence);

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn session_live_state_is_live_when_in_flight_requests_present() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-session-live-pending-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(100, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let mut pending = sample_entry();
        pending.id = "req-pending-live".into();
        pending.session_id = Some("session-pending-live".into());
        pending.status = RequestStatus::Pending;
        pending.timestamp = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
            Utc::now().timestamp_millis() - 30_000,
        )
        .unwrap();
        store.add_entry(pending);

        let sessions = store.get_claude_sessions();
        let session = sessions
            .iter()
            .find(|session| session.session_id == "session-pending-live")
            .expect("missing pending-backed session");

        assert_eq!(session.in_flight_requests, 1);
        assert!(session.is_live);

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn session_live_state_treats_stale_pending_as_live_signal() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-session-live-stale-pending-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(100, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let mut pending = sample_entry();
        pending.id = "req-stale-pending".into();
        pending.session_id = Some("session-stale-pending".into());
        pending.status = RequestStatus::Pending;
        pending.timestamp = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
            Utc::now().timestamp_millis() - (SESSION_LIVE_ACTIVITY_WINDOW_MS + 60_000),
        )
        .unwrap();
        store.add_entry(pending);

        let sessions = store.get_claude_sessions();
        let session = sessions
            .iter()
            .find(|session| session.session_id == "session-stale-pending")
            .expect("missing stale-pending session");

        assert_eq!(session.in_flight_requests, 1);
        assert!(session.is_live);

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn session_live_state_is_live_when_recent_activity_without_in_flight() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-session-live-recent-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(100, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        store.upsert_local_event(&LocalEvent {
            id: "evt-live-recent".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "projects/proj-recent/session.json".into(),
            event_time_ms: Utc::now().timestamp_millis() - 30_000,
            session_hint: Some("session-recent-live".into()),
            event_kind: "session_touch".into(),
            model_hint: None,
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({}),
        });

        let sessions = store.get_claude_sessions();
        let session = sessions
            .iter()
            .find(|session| session.session_id == "session-recent-live")
            .expect("missing recent-activity session");

        assert_eq!(session.in_flight_requests, 0);
        assert!(session.is_live);

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn session_live_state_is_false_when_no_in_flight_and_activity_stale() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-session-live-stale-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(100, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let stale_ts = Utc::now().timestamp_millis() - 300_000;

        let mut success = sample_entry();
        success.id = "req-stale-success".into();
        success.session_id = Some("session-stale".into());
        success.status = RequestStatus::Success(200);
        success.timestamp =
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(stale_ts).unwrap();
        store.add_entry(success);

        store.upsert_local_event(&LocalEvent {
            id: "evt-stale".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "projects/proj-stale/session.json".into(),
            event_time_ms: stale_ts,
            session_hint: Some("session-stale".into()),
            event_kind: "session_touch".into(),
            model_hint: None,
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({}),
        });

        let sessions = store.get_claude_sessions();
        let session = sessions
            .iter()
            .find(|session| session.session_id == "session-stale")
            .expect("missing stale session");

        assert_eq!(session.in_flight_requests, 0);
        assert!(!session.is_live);

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn get_claude_sessions_sorts_by_activity_then_session_id() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-claude-sessions-sort-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(100, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        store.upsert_local_event(&LocalEvent {
            id: "evt-sid-b".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "projects/proj-b/session.json".into(),
            event_time_ms: 10_000,
            session_hint: Some("sid-b".into()),
            event_kind: "session_touch".into(),
            model_hint: None,
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({}),
        });

        let mut sid_a = sample_entry();
        sid_a.id = "req-sid-a".into();
        sid_a.session_id = Some("sid-a".into());
        sid_a.timestamp = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(10_000).unwrap();
        store.add_entry(sid_a);

        let mut sid_c = sample_entry();
        sid_c.id = "req-sid-c".into();
        sid_c.session_id = Some("sid-c".into());
        sid_c.timestamp = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(5_000).unwrap();
        store.add_entry(sid_c);

        let sessions = store.get_claude_sessions();
        let order: Vec<_> = sessions
            .iter()
            .map(|session| session.session_id.as_str())
            .collect();

        assert_eq!(order, vec!["sid-a", "sid-b", "sid-c"]);

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn get_sessions_reads_full_history_from_sqlite() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-test-sessions-history-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(1, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let mut first = sample_entry();
        first.id = "session-a-1".into();
        first.timestamp = Utc::now() - chrono::Duration::seconds(3);
        first.session_id = Some("session-a".into());
        first.model = "claude-a".into();
        store.add_entry(first);

        let mut second = sample_entry();
        second.id = "session-a-2".into();
        second.timestamp = Utc::now() - chrono::Duration::seconds(2);
        second.session_id = Some("session-a".into());
        second.model = "claude-b".into();
        second.status = RequestStatus::ServerError(500);
        second.error = Some("boom".into());
        store.add_entry(second);

        let mut third = sample_entry();
        third.id = "session-b-1".into();
        third.timestamp = Utc::now() - chrono::Duration::seconds(1);
        third.session_id = Some("session-b".into());
        third.model = "claude-c".into();
        store.add_entry(third);

        let sessions = store.get_sessions();
        assert_eq!(sessions.len(), 2);

        let session_a = sessions
            .iter()
            .find(|session| session.session_id == "session-a")
            .unwrap();
        assert_eq!(session_a.request_count, 2);
        assert_eq!(session_a.error_count, 1);
        assert_eq!(session_a.model, "claude-b");

        let session_b = sessions
            .iter()
            .find(|session| session.session_id == "session-b")
            .unwrap();
        assert_eq!(session_b.request_count, 1);

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn session_delete_deletes_child_and_parent_rows_and_keeps_non_session_tables() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-session-delete-ordering-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(50, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let mut target = sample_entry();
        target.id = "req-delete-target".into();
        target.session_id = Some("session-delete-target".into());
        let stale_time = chrono::Utc::now() - chrono::Duration::minutes(10);
        target.timestamp = stale_time;
        store.add_entry(target);
        store.write_body("req-delete-target", "request", "response");

        let mut other = sample_entry();
        other.id = "req-delete-other".into();
        other.session_id = Some("session-delete-other".into());
        store.add_entry(other);
        store.write_body("req-delete-other", "request", "response");

        store.upsert_local_event(&LocalEvent {
            id: "evt-delete-target".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "projects/demo/target.json".into(),
            event_time_ms: stale_time.timestamp_millis(),
            session_hint: Some("session-delete-target".into()),
            event_kind: "session_touch".into(),
            model_hint: None,
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({}),
        });

        store.upsert_local_event(&LocalEvent {
            id: "evt-delete-other".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "projects/demo/other.json".into(),
            event_time_ms: chrono::Utc::now().timestamp_millis(),
            session_hint: Some("session-delete-other".into()),
            event_kind: "session_touch".into(),
            model_hint: None,
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({}),
        });

        store.upsert_request_correlation(&RequestCorrelation {
            id: "corr-delete-target".into(),
            request_id: "req-delete-target".into(),
            local_event_id: "evt-delete-target".into(),
            link_type: CorrelationLinkType::SessionHint,
            confidence: 0.9,
            reason: "target".into(),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        });

        store.upsert_request_correlation(&RequestCorrelation {
            id: "corr-delete-other".into(),
            request_id: "req-delete-other".into(),
            local_event_id: "evt-delete-other".into(),
            link_type: CorrelationLinkType::SessionHint,
            confidence: 0.9,
            reason: "other".into(),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        });

        store.upsert_explanation(&Explanation {
            id: "exp-delete-target".into(),
            request_id: "req-delete-target".into(),
            anomaly_kind: "slow_ttft".into(),
            rank: 0,
            confidence: 0.8,
            summary: "target".into(),
            evidence_json: serde_json::json!({}),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        });

        store.upsert_explanation(&Explanation {
            id: "exp-delete-other".into(),
            request_id: "req-delete-other".into(),
            anomaly_kind: "slow_ttft".into(),
            rank: 0,
            confidence: 0.8,
            summary: "other".into(),
            evidence_json: serde_json::json!({}),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        });

        store.set_ingestion_checkpoint(SourceKind::ClaudeProject, "checkpoint-keep");
        {
            let conn = store.db.lock();
            conn.execute(
                "INSERT INTO config_snapshots (id, source_path, content_hash, captured_at_ms, payload_policy, payload_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params!["cfg-keep", "projects/demo/.claude/settings.json", "hash-keep", chrono::Utc::now().timestamp_millis(), "metadata_only", "{}"],
            )
            .unwrap();
        }

        let outcome = store
            .delete_session_db_rows_with_live_guard("session-delete-target")
            .expect("delete should succeed");
        assert!(!outcome.blocked_live);
        assert_eq!(outcome.deleted_db_rows_by_table.request_bodies, 1);
        assert_eq!(outcome.deleted_db_rows_by_table.request_correlations, 1);
        assert_eq!(outcome.deleted_db_rows_by_table.explanations, 1);
        assert_eq!(outcome.deleted_db_rows_by_table.requests, 1);
        assert_eq!(outcome.deleted_db_rows_by_table.local_events, 1);

        let conn = store.db.lock();
        let table_count = |table: &str| -> i64 {
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
        };

        assert_eq!(table_count("request_bodies"), 1);
        assert_eq!(table_count("request_correlations"), 1);
        assert_eq!(table_count("explanations"), 1);
        assert_eq!(table_count("requests"), 1);
        assert_eq!(table_count("local_events"), 1);
        assert_eq!(table_count("ingestion_checkpoints"), 1);
        assert_eq!(table_count("config_snapshots"), 1);

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn session_delete_scales_for_large_session_without_bind_placeholder_expansion() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-session-delete-large-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(2_500, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());
        let stale_time = chrono::Utc::now() - chrono::Duration::minutes(10);

        let request_count = 1_100u64;
        for idx in 0..request_count {
            let request_id = format!("req-delete-large-{idx}");

            let mut entry = sample_entry();
            entry.id = request_id.clone();
            entry.session_id = Some("session-delete-large".into());
            entry.timestamp = stale_time;
            store.add_entry(entry);

            store.write_body(&request_id, "request", "response");

            let local_event_id = format!("evt-delete-large-{idx}");
            store.upsert_local_event(&LocalEvent {
                id: local_event_id.clone(),
                source_kind: SourceKind::ClaudeProject,
                source_path: format!("projects/demo/large-{idx}.json"),
                event_time_ms: stale_time.timestamp_millis(),
                session_hint: Some("session-delete-large".into()),
                event_kind: "session_touch".into(),
                model_hint: None,
                payload_policy: PayloadPolicy::MetadataOnly,
                payload_json: serde_json::json!({}),
            });

            store.upsert_request_correlation(&RequestCorrelation {
                id: format!("corr-delete-large-{idx}"),
                request_id: request_id.clone(),
                local_event_id,
                link_type: CorrelationLinkType::SessionHint,
                confidence: 0.9,
                reason: "large".into(),
                created_at_ms: stale_time.timestamp_millis(),
            });

            store.upsert_explanation(&Explanation {
                id: format!("exp-delete-large-{idx}"),
                request_id,
                anomaly_kind: "slow_ttft".into(),
                rank: 0,
                confidence: 0.8,
                summary: "large".into(),
                evidence_json: serde_json::json!({}),
                created_at_ms: stale_time.timestamp_millis(),
            });
        }

        let outcome = store
            .delete_session_db_rows_with_live_guard("session-delete-large")
            .expect("large session delete should succeed");
        assert!(!outcome.blocked_live);
        assert_eq!(outcome.deleted_db_rows_by_table.request_bodies, request_count);
        assert_eq!(
            outcome.deleted_db_rows_by_table.request_correlations,
            request_count
        );
        assert_eq!(outcome.deleted_db_rows_by_table.explanations, request_count);
        assert_eq!(outcome.deleted_db_rows_by_table.requests, request_count);
        assert_eq!(outcome.deleted_db_rows_by_table.local_events, request_count);

        let conn = store.db.lock();
        let remaining_requests: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM requests WHERE session_id = ?1",
                params!["session-delete-large"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining_requests, 0);

        let remaining_request_bodies: i64 = conn
            .query_row("SELECT COUNT(*) FROM request_bodies", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining_request_bodies, 0);

        let remaining_request_correlations: i64 = conn
            .query_row("SELECT COUNT(*) FROM request_correlations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining_request_correlations, 0);

        let remaining_explanations: i64 = conn
            .query_row("SELECT COUNT(*) FROM explanations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining_explanations, 0);

        let remaining_local_events: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM local_events WHERE session_hint = ?1",
                params!["session-delete-large"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining_local_events, 0);

        let _ = std::fs::remove_dir_all(&log_dir);
    }


    #[test]
    fn session_delete_blocks_when_liveness_flips_before_commit() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-session-delete-live-flip-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(20, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let mut stale = sample_entry();
        stale.id = "req-delete-live-flip-stale".into();
        stale.session_id = Some("session-delete-live-flip".into());
        stale.timestamp = chrono::Utc::now() - chrono::Duration::minutes(10);
        store.add_entry(stale);

        store.trigger_pre_commit_live_guard_flip_for_test();

        let outcome = store
            .delete_session_db_rows_with_live_guard("session-delete-live-flip")
            .expect("delete should return outcome");

        assert!(outcome.blocked_live);
        assert_eq!(
            outcome.deleted_db_rows_by_table,
            SessionDeleteDbRowsByTable::default()
        );

        let conn = store.db.lock();
        let request_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM requests WHERE session_id = ?1",
                params!["session-delete-live-flip"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(request_count, 1);
        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn session_delete_blocks_live_session() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-session-delete-live-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(20, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let mut pending = sample_entry();
        pending.id = "req-delete-live".into();
        pending.session_id = Some("session-delete-live".into());
        pending.status = RequestStatus::Pending;
        pending.timestamp = chrono::Utc::now();
        store.add_entry(pending);

        let outcome = store
            .delete_session_db_rows_with_live_guard("session-delete-live")
            .expect("delete should return outcome");
        assert!(outcome.blocked_live);
        assert_eq!(
            outcome.deleted_db_rows_by_table,
            SessionDeleteDbRowsByTable::default()
        );

        let conn = store.db.lock();
        let request_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM requests WHERE session_id = ?1",
                params!["session-delete-live"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(request_count, 1);

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn settings_history_roundtrip() {
        let store = test_store();

        let snapshot = SettingsHistoryItem {
            id: "settings-1".into(),
            saved_at_ms: 1_700_000_000_000,
            content_hash: "hash-abc".into(),
            settings_json: "{\"feature\":true}".into(),
            source: "admin".into(),
        };

        assert!(store.insert_settings_history_snapshot(&snapshot));

        let history = store.list_settings_history_desc(10);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, "settings-1");

        let loaded = store
            .get_settings_history_item("settings-1")
            .expect("settings snapshot should exist");
        assert_eq!(loaded.content_hash, "hash-abc");
        assert_eq!(loaded.settings_json, "{\"feature\":true}");

        let _ = std::fs::remove_dir_all(store.storage_dir.clone());
    }

    #[test]
    fn settings_history_supports_delete_clear_ordering_limit_and_duplicate_id_rejection() {
        let store = test_store();

        let oldest = SettingsHistoryItem {
            id: "settings-oldest".into(),
            saved_at_ms: 1_700_000_000_010,
            content_hash: "hash-oldest".into(),
            settings_json: "{\"value\":1}".into(),
            source: "admin".into(),
        };
        let middle = SettingsHistoryItem {
            id: "settings-middle".into(),
            saved_at_ms: 1_700_000_000_020,
            content_hash: "hash-middle".into(),
            settings_json: "{\"value\":2}".into(),
            source: "admin".into(),
        };
        let newest = SettingsHistoryItem {
            id: "settings-newest".into(),
            saved_at_ms: 1_700_000_000_030,
            content_hash: "hash-newest".into(),
            settings_json: "{\"value\":3}".into(),
            source: "admin".into(),
        };

        assert!(store.insert_settings_history_snapshot(&oldest));
        assert!(store.insert_settings_history_snapshot(&middle));
        assert!(store.insert_settings_history_snapshot(&newest));

        let duplicate_id = SettingsHistoryItem {
            id: "settings-middle".into(),
            saved_at_ms: 1_700_000_000_040,
            content_hash: "hash-duplicate".into(),
            settings_json: "{\"value\":999}".into(),
            source: "admin".into(),
        };
        assert!(
            !store.insert_settings_history_snapshot(&duplicate_id),
            "duplicate id should fail due to PRIMARY KEY"
        );

        let ordered_limited = store.list_settings_history_desc(2);
        assert_eq!(ordered_limited.len(), 2);
        assert_eq!(ordered_limited[0].id, "settings-newest");
        assert_eq!(ordered_limited[1].id, "settings-middle");

        assert!(store.delete_settings_history_item("settings-middle"));
        assert!(
            !store.delete_settings_history_item("settings-missing"),
            "deleting missing id should report false"
        );
        assert!(store.get_settings_history_item("settings-middle").is_none());

        assert_eq!(store.clear_settings_history(), 2);
        assert!(store.list_settings_history_desc(10).is_empty());

        let _ = std::fs::remove_dir_all(store.storage_dir.clone());
    }

    #[test]
    fn historical_stats_snapshot_reads_full_sqlite_history() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-test-historical-stats-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(1, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let mut first = sample_entry();
        first.id = "historical-1".into();
        first.timestamp = Utc::now() - chrono::Duration::seconds(2);
        store.add_entry(first);

        let mut second = sample_entry();
        second.id = "historical-2".into();
        second.timestamp = Utc::now() - chrono::Duration::seconds(1);
        second.status = RequestStatus::ClientError(403);
        second.error = Some("forbidden".into());
        store.add_entry(second);

        let snapshot = store.get_historical_stats_snapshot();

        assert_eq!(snapshot.mode, StatsMode::Historical);
        assert_eq!(snapshot.stats.total_requests, 2);
        assert_eq!(snapshot.stats.total_errors, 1);
        assert!(snapshot.coverage_start.is_some());
        assert!(snapshot.coverage_end.is_some());

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn get_merged_sessions_includes_proxy_sessions_with_correct_presence() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-merged-sessions-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = StatsStore::new(50, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone());

        let mut req = sample_entry();
        req.id = "req-merged-1".into();
        req.session_id = Some("s-merged".into());
        store.add_entry(req);

        let rows = store.get_merged_sessions();
        let row = rows.iter().find(|r| r.session_id == "s-merged").unwrap();

        assert_eq!(row.presence, "proxy");
        assert_eq!(row.proxy_request_count, 1);
        assert!(row.last_proxy_activity_ms.is_some());
        assert!(row.last_activity_ms > 0);

        let _ = std::fs::remove_dir_all(&log_dir);
    }
}
