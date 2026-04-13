use crate::analyzer::DetectedAnomaly;
use crate::types::{RequestRecord, RequestStatusKind};
use chrono::{TimeZone, Utc};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use std::convert::TryFrom;
use std::path::Path;
use std::sync::Arc;

pub struct Store {
    db: Arc<Mutex<Connection>>,
}

impl Store {
    fn normalize_fts_query(query: &str) -> Option<String> {
        let terms: Vec<String> = query
            .split_whitespace()
            .map(str::trim)
            .filter(|term| !term.is_empty())
            .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
            .collect();

        if terms.is_empty() {
            None
        } else {
            Some(terms.join(" AND "))
        }
    }

    pub fn new(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let store = Self {
            db: Arc::new(Mutex::new(conn)),
        };
        store.initialize_schema()?;
        Ok(store)
    }

    fn initialize_schema(&self) -> Result<(), rusqlite::Error> {
        let db = self.db.lock();
        db.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS requests (
                id TEXT PRIMARY KEY,
                session_id TEXT,
                timestamp_ms INTEGER NOT NULL,
                method TEXT NOT NULL,
                path TEXT NOT NULL,
                model TEXT NOT NULL DEFAULT '',
                stream INTEGER NOT NULL DEFAULT 0,
                status_code INTEGER,
                status_kind TEXT NOT NULL DEFAULT 'pending',
                ttft_ms REAL,
                duration_ms REAL,
                input_tokens INTEGER,
                output_tokens INTEGER,
                cache_read_tokens INTEGER,
                cache_creation_tokens INTEGER,
                thinking_tokens INTEGER,
                request_size_bytes INTEGER,
                response_size_bytes INTEGER,
                stall_count INTEGER NOT NULL DEFAULT 0,
                stall_details_json TEXT NOT NULL DEFAULT '[]',
                error_summary TEXT,
                stop_reason TEXT,
                content_block_types_json TEXT NOT NULL DEFAULT '[]',
                anomalies_json TEXT NOT NULL DEFAULT '[]',
                analyzed INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_requests_timestamp ON requests(timestamp_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_requests_session ON requests(session_id);
            CREATE INDEX IF NOT EXISTS idx_requests_model ON requests(model);
            CREATE INDEX IF NOT EXISTS idx_requests_analyzed ON requests(analyzed) WHERE analyzed = 0;

            CREATE TABLE IF NOT EXISTS request_bodies (
                request_id TEXT PRIMARY KEY REFERENCES requests(id),
                request_body TEXT,
                response_body TEXT,
                truncated INTEGER NOT NULL DEFAULT 0
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS request_bodies_fts USING fts5(
                request_id,
                request_body,
                response_body,
                content=request_bodies,
                content_rowid=rowid
            );

            CREATE TRIGGER IF NOT EXISTS request_bodies_ai AFTER INSERT ON request_bodies BEGIN
                INSERT INTO request_bodies_fts(rowid, request_id, request_body, response_body)
                VALUES (new.rowid, new.request_id, new.request_body, new.response_body);
            END;

            CREATE TRIGGER IF NOT EXISTS request_bodies_ad AFTER DELETE ON request_bodies BEGIN
                INSERT INTO request_bodies_fts(request_bodies_fts, rowid, request_id, request_body, response_body)
                VALUES ('delete', old.rowid, old.request_id, old.request_body, old.response_body);
            END;

            CREATE TRIGGER IF NOT EXISTS request_bodies_au AFTER UPDATE ON request_bodies BEGIN
                INSERT INTO request_bodies_fts(request_bodies_fts, rowid, request_id, request_body, response_body)
                VALUES ('delete', old.rowid, old.request_id, old.request_body, old.response_body);
                INSERT INTO request_bodies_fts(rowid, request_id, request_body, response_body)
                VALUES (new.rowid, new.request_id, new.request_body, new.response_body);
            END;

            CREATE TABLE IF NOT EXISTS tool_usage (
                id TEXT PRIMARY KEY,
                request_id TEXT NOT NULL REFERENCES requests(id),
                tool_name TEXT NOT NULL,
                tool_input_json TEXT,
                success INTEGER,
                is_error INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_tool_usage_request ON tool_usage(request_id);
            CREATE INDEX IF NOT EXISTS idx_tool_usage_name ON tool_usage(tool_name);

            CREATE TABLE IF NOT EXISTS anomalies (
                id TEXT PRIMARY KEY,
                request_id TEXT NOT NULL REFERENCES requests(id),
                kind TEXT NOT NULL,
                severity TEXT NOT NULL,
                summary TEXT NOT NULL,
                hypothesis TEXT,
                evidence_json TEXT NOT NULL DEFAULT '{}',
                created_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_anomalies_request ON anomalies(request_id);
            CREATE INDEX IF NOT EXISTS idx_anomalies_kind ON anomalies(kind);
            CREATE INDEX IF NOT EXISTS idx_anomalies_severity ON anomalies(severity);

            CREATE TABLE IF NOT EXISTS model_profiles (
                model_name TEXT PRIMARY KEY,
                behavior_class TEXT,
                config_json TEXT NOT NULL DEFAULT '{}',
                observed_json TEXT NOT NULL DEFAULT '{}',
                sample_count INTEGER NOT NULL DEFAULT 0,
                last_updated_ms INTEGER
            );
            "#,
        )?;
        Ok(())
    }

    pub fn list_table_names(&self) -> Result<Vec<String>, rusqlite::Error> {
        let db = self.db.lock();
        let mut stmt =
            db.prepare("SELECT name FROM sqlite_master WHERE type IN ('table','view')")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn add_request(&self, req: &RequestRecord) -> Result<(), rusqlite::Error> {
        let request_size_bytes = i64::try_from(req.request_size_bytes)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
        let response_size_bytes = i64::try_from(req.response_size_bytes)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;

        let db = self.db.lock();
        db.execute(
            r#"INSERT INTO requests (
                id, session_id, timestamp_ms, method, path, model, stream, status_code, status_kind,
                ttft_ms, duration_ms, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
                thinking_tokens, request_size_bytes, response_size_bytes, stall_count, stall_details_json,
                error_summary, stop_reason, content_block_types_json, anomalies_json, analyzed
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)"#,
            params![
                req.id,
                req.session_id,
                req.timestamp.timestamp_millis(),
                req.method,
                req.path,
                req.model,
                req.stream as i64,
                req.status_code,
                req.status_kind.as_str(),
                req.ttft_ms,
                req.duration_ms,
                req.input_tokens,
                req.output_tokens,
                req.cache_read_tokens,
                req.cache_creation_tokens,
                req.thinking_tokens,
                request_size_bytes,
                response_size_bytes,
                req.stall_count as i64,
                req.stall_details_json,
                req.error_summary,
                req.stop_reason,
                req.content_block_types_json,
                req.anomalies_json,
                req.analyzed as i64,
            ],
        )?;
        Ok(())
    }

    pub fn write_bodies(
        &self,
        request_id: &str,
        request_body: &str,
        response_body: &str,
        truncated: bool,
    ) -> Result<(), rusqlite::Error> {
        let db = self.db.lock();
        db.execute(
            "INSERT INTO request_bodies (request_id, request_body, response_body, truncated)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(request_id) DO UPDATE SET
                request_body = excluded.request_body,
                response_body = excluded.response_body,
                truncated = excluded.truncated",
            params![request_id, request_body, response_body, truncated as i64],
        )?;
        Ok(())
    }

    pub fn search_request_ids(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<String>, rusqlite::Error> {
        let Some(query) = Self::normalize_fts_query(query) else {
            return Ok(Vec::new());
        };

        let db = self.db.lock();
        let mut stmt = db.prepare(
            "SELECT request_id FROM request_bodies_fts WHERE request_bodies_fts MATCH ?1 ORDER BY rank LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(params![query, limit as i64, offset as i64], |row| {
            row.get::<_, String>(0)
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn status_kind_from_db(value: &str) -> RequestStatusKind {
        match value {
            "success" => RequestStatusKind::Success,
            "client_error" => RequestStatusKind::ClientError,
            "server_error" => RequestStatusKind::ServerError,
            "timeout" => RequestStatusKind::Timeout,
            "connection_error" => RequestStatusKind::ConnectionError,
            "proxy_error" => RequestStatusKind::ProxyError,
            _ => RequestStatusKind::Pending,
        }
    }

    fn row_to_request(row: &rusqlite::Row<'_>) -> Result<RequestRecord, rusqlite::Error> {
        let ts_ms: i64 = row.get("timestamp_ms")?;
        let timestamp = Utc
            .timestamp_millis_opt(ts_ms)
            .single()
            .unwrap_or_else(Utc::now);

        Ok(RequestRecord {
            id: row.get("id")?,
            session_id: row.get("session_id")?,
            timestamp,
            method: row.get("method")?,
            path: row.get("path")?,
            model: row.get("model")?,
            stream: row.get::<_, i64>("stream")? != 0,
            status_code: row.get("status_code")?,
            status_kind: Self::status_kind_from_db(&row.get::<_, String>("status_kind")?),
            ttft_ms: row.get("ttft_ms")?,
            duration_ms: row.get("duration_ms")?,
            input_tokens: row.get("input_tokens")?,
            output_tokens: row.get("output_tokens")?,
            cache_read_tokens: row.get("cache_read_tokens")?,
            cache_creation_tokens: row.get("cache_creation_tokens")?,
            thinking_tokens: row.get("thinking_tokens")?,
            request_size_bytes: row.get::<_, i64>("request_size_bytes")? as u64,
            response_size_bytes: row.get::<_, i64>("response_size_bytes")? as u64,
            stall_count: row.get::<_, i64>("stall_count")? as u32,
            stall_details_json: row.get("stall_details_json")?,
            error_summary: row.get("error_summary")?,
            stop_reason: row.get("stop_reason")?,
            content_block_types_json: row.get("content_block_types_json")?,
            anomalies_json: row.get("anomalies_json")?,
            analyzed: row.get::<_, i64>("analyzed")? != 0,
        })
    }

    pub fn get_request(&self, request_id: &str) -> Result<Option<RequestRecord>, rusqlite::Error> {
        let db = self.db.lock();
        let mut stmt = db.prepare(
            r#"SELECT id, session_id, timestamp_ms, method, path, model, stream, status_code, status_kind,
                      ttft_ms, duration_ms, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
                      thinking_tokens, request_size_bytes, response_size_bytes, stall_count, stall_details_json,
                      error_summary, stop_reason, content_block_types_json, anomalies_json, analyzed
               FROM requests WHERE id = ?1"#,
        )?;

        let mut rows = stmt.query(params![request_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(Self::row_to_request(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn list_unanalyzed_requests(
        &self,
        limit: usize,
    ) -> Result<Vec<RequestRecord>, rusqlite::Error> {
        let db = self.db.lock();
        let mut stmt = db.prepare(
            r#"SELECT id, session_id, timestamp_ms, method, path, model, stream, status_code, status_kind,
                      ttft_ms, duration_ms, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
                      thinking_tokens, request_size_bytes, response_size_bytes, stall_count, stall_details_json,
                      error_summary, stop_reason, content_block_types_json, anomalies_json, analyzed
               FROM requests
               WHERE analyzed = 0
               ORDER BY timestamp_ms ASC
               LIMIT ?1"#,
        )?;

        let rows = stmt.query_map(params![limit as i64], Self::row_to_request)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn list_recent_requests_for_model(
        &self,
        model: &str,
        limit: usize,
    ) -> Result<Vec<RequestRecord>, rusqlite::Error> {
        let db = self.db.lock();
        let mut stmt = db.prepare(
            r#"SELECT id, session_id, timestamp_ms, method, path, model, stream, status_code, status_kind,
                      ttft_ms, duration_ms, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
                      thinking_tokens, request_size_bytes, response_size_bytes, stall_count, stall_details_json,
                      error_summary, stop_reason, content_block_types_json, anomalies_json, analyzed
               FROM requests
               WHERE model = ?1
               ORDER BY timestamp_ms DESC
               LIMIT ?2"#,
        )?;

        let rows = stmt.query_map(params![model, limit as i64], Self::row_to_request)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn serialize_anomaly_kind(anomaly: &DetectedAnomaly) -> String {
        serde_json::to_string(&anomaly.kind)
            .unwrap_or_else(|_| "\"slow_ttft\"".to_string())
            .replace('"', "")
    }

    fn serialize_anomaly_severity(anomaly: &DetectedAnomaly) -> String {
        serde_json::to_string(&anomaly.severity)
            .unwrap_or_else(|_| "\"warning\"".to_string())
            .replace('"', "")
    }

    pub fn insert_anomalies(
        &self,
        request_id: &str,
        anomalies: &[DetectedAnomaly],
    ) -> Result<(), rusqlite::Error> {
        let now_ms = Utc::now().timestamp_millis();
        let db = self.db.lock();

        for anomaly in anomalies {
            db.execute(
                "INSERT INTO anomalies (id, request_id, kind, severity, summary, hypothesis, evidence_json, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    request_id,
                    Self::serialize_anomaly_kind(anomaly),
                    Self::serialize_anomaly_severity(anomaly),
                    anomaly.summary,
                    anomaly.hypothesis,
                    "{}",
                    now_ms,
                ],
            )?;
        }

        Ok(())
    }

    pub fn persist_analyzed_request(
        &self,
        request_id: &str,
        model: &str,
        anomalies: &[DetectedAnomaly],
    ) -> Result<u64, rusqlite::Error> {
        let now_ms = Utc::now().timestamp_millis();
        let mut db = self.db.lock();
        let tx = db.transaction()?;

        tx.execute(
            "DELETE FROM anomalies WHERE request_id = ?1",
            params![request_id],
        )?;

        for anomaly in anomalies {
            tx.execute(
                "INSERT INTO anomalies (id, request_id, kind, severity, summary, hypothesis, evidence_json, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    request_id,
                    Self::serialize_anomaly_kind(anomaly),
                    Self::serialize_anomaly_severity(anomaly),
                    anomaly.summary,
                    anomaly.hypothesis,
                    "{}",
                    now_ms,
                ],
            )?;
        }

        tx.execute(
            "UPDATE requests SET analyzed = 1 WHERE id = ?1",
            params![request_id],
        )?;

        tx.execute(
            "INSERT INTO model_profiles (model_name, sample_count, last_updated_ms) VALUES (?1, 0, ?2)
             ON CONFLICT(model_name) DO NOTHING",
            params![model, now_ms],
        )?;

        tx.execute(
            "UPDATE model_profiles SET sample_count = sample_count + 1, last_updated_ms = ?2 WHERE model_name = ?1",
            params![model, now_ms],
        )?;

        let sample_count: i64 = tx.query_row(
            "SELECT sample_count FROM model_profiles WHERE model_name = ?1",
            params![model],
            |row| row.get(0),
        )?;

        tx.commit()?;

        Ok(sample_count.max(0) as u64)
    }

    pub fn mark_analyzed(&self, request_id: &str) -> Result<(), rusqlite::Error> {
        let db = self.db.lock();
        db.execute(
            "UPDATE requests SET analyzed = 1 WHERE id = ?1",
            params![request_id],
        )?;
        Ok(())
    }

    pub fn compute_model_observed_stats(
        &self,
        model: &str,
    ) -> Result<serde_json::Value, rusqlite::Error> {
        let db = self.db.lock();

        // Aggregate query for counts, averages, and rates
        let row_data: (
            i64,            // sample_count
            Option<f64>,    // avg_ttft_ms
            Option<f64>,    // min_ttft_ms
            Option<f64>,    // max_ttft_ms
            Option<f64>,    // avg_duration_ms
            Option<f64>,    // avg_input_tokens
            Option<f64>,    // avg_output_tokens
            Option<f64>,    // avg_thinking_tokens
            i64,            // thinking_count (non-null thinking_tokens > 0)
            Option<f64>,    // avg_cache_read_tokens
            i64,            // cache_read_count (non-null cache_read > 0)
            i64,            // cache_creation_count (non-null cache_creation > 0)
            i64,            // error_count (status >= 400)
            i64,            // rate_limit_count (status = 429)
            i64,            // overload_count (status = 529)
            i64,            // server_error_count (status 500-599)
            i64,            // stall_count (stall_count > 0)
            i64,            // end_turn_count
            i64,            // max_tokens_count
            i64,            // stream_count
            i64,            // interrupted_stream_count
        ) = db.query_row(
            "SELECT
                COUNT(*),
                AVG(ttft_ms),
                MIN(ttft_ms),
                MAX(ttft_ms),
                AVG(duration_ms),
                AVG(CAST(input_tokens AS REAL)),
                AVG(CAST(output_tokens AS REAL)),
                AVG(CASE WHEN thinking_tokens > 0 THEN CAST(thinking_tokens AS REAL) END),
                SUM(CASE WHEN thinking_tokens > 0 THEN 1 ELSE 0 END),
                AVG(CASE WHEN cache_read_tokens > 0 THEN CAST(cache_read_tokens AS REAL) END),
                SUM(CASE WHEN cache_read_tokens > 0 THEN 1 ELSE 0 END),
                SUM(CASE WHEN cache_creation_tokens > 0 THEN 1 ELSE 0 END),
                SUM(CASE WHEN status_code >= 400 THEN 1 ELSE 0 END),
                SUM(CASE WHEN status_code = 429 THEN 1 ELSE 0 END),
                SUM(CASE WHEN status_code = 529 THEN 1 ELSE 0 END),
                SUM(CASE WHEN status_code >= 500 AND status_code < 600 THEN 1 ELSE 0 END),
                SUM(CASE WHEN stall_count > 0 THEN 1 ELSE 0 END),
                SUM(CASE WHEN stop_reason = 'end_turn' THEN 1 ELSE 0 END),
                SUM(CASE WHEN stop_reason = 'max_tokens' THEN 1 ELSE 0 END),
                SUM(CASE WHEN stream = 1 THEN 1 ELSE 0 END),
                SUM(CASE WHEN stream = 1 AND stop_reason IS NULL AND error_summary IS NOT NULL THEN 1 ELSE 0 END)
            FROM requests WHERE model = ?1",
            params![model],
            |row| Ok((
                row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?,
                row.get(8)?, row.get(9)?, row.get(10)?, row.get(11)?,
                row.get(12)?, row.get(13)?, row.get(14)?, row.get(15)?,
                row.get(16)?, row.get(17)?, row.get(18)?, row.get(19)?,
                row.get(20)?,
            )),
        )?;

        let (
            sample_count, avg_ttft_ms, min_ttft_ms, max_ttft_ms,
            avg_duration_ms, avg_input_tokens, avg_output_tokens, avg_thinking_tokens,
            thinking_count, avg_cache_read_tokens, cache_read_count, cache_creation_count,
            error_count, rate_limit_count, overload_count, server_error_count,
            stall_req_count, end_turn_count, max_tokens_count, stream_count,
            interrupted_stream_count,
        ) = row_data;

        if sample_count == 0 {
            return Ok(serde_json::json!({ "sample_count": 0 }));
        }

        let n = sample_count as f64;

        // Compute TTFT percentiles from sorted values
        let ttft_values: Vec<f64> = {
            let mut stmt = db.prepare(
                "SELECT ttft_ms FROM requests WHERE model = ?1 AND ttft_ms IS NOT NULL ORDER BY ttft_ms"
            )?;
            let rows = stmt.query_map(params![model], |row| row.get::<_, f64>(0))?;
            rows.filter_map(|r| r.ok()).collect()
        };

        let (median_ttft_ms, p95_ttft_ms, p99_ttft_ms, ttft_stddev_ms) = if ttft_values.is_empty() {
            (None, None, None, None)
        } else {
            let len = ttft_values.len();
            let median = ttft_values[len / 2];
            let p95 = ttft_values[(len as f64 * 0.95) as usize].min(ttft_values[len - 1]);
            let p99 = ttft_values[(len as f64 * 0.99) as usize].min(ttft_values[len - 1]);
            let mean = avg_ttft_ms.unwrap_or(0.0);
            let variance = ttft_values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / len as f64;
            (Some(median), Some(p95), Some(p99), Some(variance.sqrt()))
        };

        // Derived rates
        let output_input_ratio = match (avg_output_tokens, avg_input_tokens) {
            (Some(o), Some(i)) if i > 0.0 => Some(o / i),
            _ => None,
        };
        let total_tokens_per_request = match (avg_input_tokens, avg_output_tokens) {
            (Some(i), Some(o)) => Some(i + o),
            _ => None,
        };
        let thinking_frequency = thinking_count as f64 / n;
        let thinking_token_ratio = match (avg_thinking_tokens, avg_output_tokens) {
            (Some(t), Some(o)) if o > 0.0 => Some(t / o),
            _ => None,
        };
        let cache_hit_rate = cache_read_count as f64 / n;
        let cache_creation_rate = cache_creation_count as f64 / n;
        let error_rate = error_count as f64 / n;
        let rate_limit_rate = rate_limit_count as f64 / n;
        let overload_rate = overload_count as f64 / n;
        let server_error_rate = server_error_count as f64 / n;
        let stall_rate = stall_req_count as f64 / n;
        let end_turn_rate = end_turn_count as f64 / n;
        let max_tokens_hit_rate = max_tokens_count as f64 / n;
        let stream_completion_rate = if stream_count > 0 {
            1.0 - (interrupted_stream_count as f64 / stream_count as f64)
        } else {
            1.0
        };

        Ok(serde_json::json!({
            "sample_count": sample_count,
            // Timing
            "avg_ttft_ms": avg_ttft_ms,
            "median_ttft_ms": median_ttft_ms,
            "p95_ttft_ms": p95_ttft_ms,
            "p99_ttft_ms": p99_ttft_ms,
            "min_ttft_ms": min_ttft_ms,
            "max_ttft_ms": max_ttft_ms,
            "ttft_stddev_ms": ttft_stddev_ms,
            "avg_duration_ms": avg_duration_ms,
            // Tokens
            "avg_input_tokens": avg_input_tokens,
            "avg_output_tokens": avg_output_tokens,
            "avg_thinking_tokens": avg_thinking_tokens,
            "output_input_ratio": output_input_ratio,
            "total_tokens_per_request": total_tokens_per_request,
            "thinking_frequency": thinking_frequency,
            "thinking_token_ratio": thinking_token_ratio,
            // Cache
            "cache_hit_rate": cache_hit_rate,
            "avg_cache_read_tokens": avg_cache_read_tokens,
            "cache_creation_rate": cache_creation_rate,
            // Errors
            "error_rate": error_rate,
            "rate_limit_rate": rate_limit_rate,
            "overload_rate": overload_rate,
            "server_error_rate": server_error_rate,
            // Streaming
            "stall_rate": stall_rate,
            "end_turn_rate": end_turn_rate,
            "max_tokens_hit_rate": max_tokens_hit_rate,
            "stream_completion_rate": stream_completion_rate,
        }))
    }

    pub fn count_anomalies_for_request(&self, request_id: &str) -> Result<u64, rusqlite::Error> {
        let db = self.db.lock();
        let count: i64 = db.query_row(
            "SELECT COUNT(*) FROM anomalies WHERE request_id = ?1",
            params![request_id],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }

    pub fn get_model_profile_sample_count(
        &self,
        model: &str,
    ) -> Result<Option<u64>, rusqlite::Error> {
        let db = self.db.lock();
        let mut stmt =
            db.prepare("SELECT sample_count FROM model_profiles WHERE model_name = ?1")?;
        let mut rows = stmt.query(params![model])?;
        if let Some(row) = rows.next()? {
            let sample_count: i64 = row.get(0)?;
            Ok(Some(sample_count.max(0) as u64))
        } else {
            Ok(None)
        }
    }

    pub fn get_model_profile_observed(
        &self,
        model: &str,
    ) -> Result<Option<serde_json::Value>, rusqlite::Error> {
        let db = self.db.lock();
        let mut stmt =
            db.prepare("SELECT observed_json FROM model_profiles WHERE model_name = ?1")?;
        let mut rows = stmt.query(params![model])?;
        if let Some(row) = rows.next()? {
            let observed_json: String = row.get(0)?;
            let observed =
                serde_json::from_str(&observed_json).unwrap_or_else(|_| serde_json::json!({}));
            Ok(Some(observed))
        } else {
            Ok(None)
        }
    }

    pub fn upsert_model_observed(
        &self,
        model: &str,
        observed: &serde_json::Value,
    ) -> Result<(), rusqlite::Error> {
        let now_ms = Utc::now().timestamp_millis();
        let observed_json = serde_json::to_string(observed).unwrap_or_else(|_| "{}".to_string());
        let db = self.db.lock();

        db.execute(
            "INSERT INTO model_profiles (model_name, observed_json, last_updated_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT(model_name) DO UPDATE SET observed_json = excluded.observed_json, last_updated_ms = excluded.last_updated_ms",
            params![model, observed_json, now_ms],
        )?;

        Ok(())
    }
    pub fn get_tool_usage_for_request(
        &self,
        request_id: &str,
    ) -> Result<Vec<serde_json::Value>, rusqlite::Error> {
        let db = self.db.lock();
        let mut stmt = db.prepare(
            "SELECT id, tool_name, tool_input_json, success, is_error FROM tool_usage WHERE request_id = ?1"
        )?;
        let rows = stmt.query_map(params![request_id], |row| {
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

    pub fn insert_tool_usage(
        &self,
        request_id: &str,
        tool_name: &str,
        tool_input_json: &str,
    ) -> Result<(), rusqlite::Error> {
        let db = self.db.lock();
        let id = uuid::Uuid::new_v4().to_string();
        db.execute(
            "INSERT INTO tool_usage (id, request_id, tool_name, tool_input_json) VALUES (?1, ?2, ?3, ?4)",
            params![id, request_id, tool_name, tool_input_json],
        )?;
        Ok(())
    }

    pub fn get_anomaly_by_id(
        &self,
        anomaly_id: &str,
    ) -> Result<Option<serde_json::Value>, rusqlite::Error> {
        let db = self.db.lock();
        let mut stmt = db.prepare(
            "SELECT id, request_id, kind, severity, summary, hypothesis, evidence_json, created_at_ms FROM anomalies WHERE id = ?1"
        )?;
        let result = stmt.query_row(params![anomaly_id], |row| {
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

    /// Delete all data from the v2 store (requests, bodies, tool_usage, anomalies, model_profiles).
    pub fn clear_all(&self) -> Result<(), rusqlite::Error> {
        let db = self.db.lock();
        // Order matters: child tables with FK references first
        db.execute_batch(
            "DELETE FROM request_bodies_fts;
             DELETE FROM request_bodies;
             DELETE FROM tool_usage;
             DELETE FROM anomalies;
             DELETE FROM model_profiles;
             DELETE FROM requests;",
        )?;
        Ok(())
    }

    pub fn list_all_model_stats(&self) -> Result<Vec<serde_json::Value>, rusqlite::Error> {
        let db = self.db.lock();
        let mut stmt = db.prepare(
            "SELECT model, COUNT(*) as count, \
             AVG(CASE WHEN ttft_ms IS NOT NULL THEN ttft_ms END) as avg_ttft, \
             SUM(CASE WHEN status_code >= 400 THEN 1 ELSE 0 END) as error_count, \
             MAX(timestamp_ms) as last_seen \
             FROM requests GROUP BY model ORDER BY count DESC",
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

    pub fn replace_explanations_for_request(
        &self,
        request_id: &str,
        explanations: &[crate::explain::Explanation],
    ) -> Result<(), rusqlite::Error> {
        let db = self.db.lock();
        for explanation in explanations {
            db.execute(
                "UPDATE anomalies SET evidence_json = ?1 WHERE request_id = ?2 AND kind = ?3",
                params![
                    serde_json::to_string(&explanation.evidence_json)
                        .unwrap_or_else(|_| "{}".to_string()),
                    request_id,
                    explanation.anomaly_kind.to_lowercase(),
                ],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{RequestRecord, RequestStatusKind};
    use chrono::Utc;
    use rusqlite::params;

    fn create_store() -> Store {
        let path = std::env::temp_dir().join(format!("proxy-v2-{}.db", uuid::Uuid::new_v4()));
        Store::new(&path).unwrap()
    }

    #[test]
    fn initialize_schema_creates_core_tables() {
        let store = create_store();
        let tables = store.list_table_names().unwrap();

        assert!(tables.contains(&"requests".to_string()));
        assert!(tables.contains(&"request_bodies".to_string()));
        assert!(tables.contains(&"request_bodies_fts".to_string()));
        assert!(tables.contains(&"tool_usage".to_string()));
        assert!(tables.contains(&"anomalies".to_string()));
        assert!(tables.contains(&"model_profiles".to_string()));
    }

    #[test]
    fn foreign_key_enforcement_rejects_orphan_request_bodies() {
        let store = create_store();
        let db = store.db.lock();

        let err = db
            .execute(
                "INSERT INTO request_bodies(request_id, request_body, response_body, truncated) VALUES(?1, ?2, ?3, ?4)",
                params!["missing-request", "input", "output", 0],
            )
            .unwrap_err();

        assert!(matches!(err, rusqlite::Error::SqliteFailure(_, _)));
        assert!(err.to_string().contains("FOREIGN KEY constraint failed"));
    }

    #[test]
    fn request_bodies_fts_triggers_sync_insert_update_delete() {
        let store = create_store();
        let db = store.db.lock();

        db.execute(
            "INSERT INTO requests(id, timestamp_ms, method, path, model) VALUES(?1, ?2, ?3, ?4, ?5)",
            params!["req-1", 1_i64, "POST", "/v1/messages", "claude-sonnet"],
        )
        .unwrap();

        db.execute(
            "INSERT INTO request_bodies(request_id, request_body, response_body, truncated) VALUES(?1, ?2, ?3, ?4)",
            params!["req-1", "alpha body", "first response", 0],
        )
        .unwrap();

        let insert_count: i64 = db
            .query_row(
                "SELECT count(*) FROM request_bodies_fts WHERE request_bodies_fts MATCH 'alpha'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(insert_count, 1);

        db.execute(
            "UPDATE request_bodies SET request_body = ?1, response_body = ?2 WHERE request_id = ?3",
            params!["beta body", "second response", "req-1"],
        )
        .unwrap();

        let old_term_count: i64 = db
            .query_row(
                "SELECT count(*) FROM request_bodies_fts WHERE request_bodies_fts MATCH 'alpha'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let new_term_count: i64 = db
            .query_row(
                "SELECT count(*) FROM request_bodies_fts WHERE request_bodies_fts MATCH 'beta'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_term_count, 0);
        assert_eq!(new_term_count, 1);

        db.execute(
            "DELETE FROM request_bodies WHERE request_id = ?1",
            params!["req-1"],
        )
        .unwrap();

        let deleted_term_count: i64 = db
            .query_row(
                "SELECT count(*) FROM request_bodies_fts WHERE request_bodies_fts MATCH 'beta'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(deleted_term_count, 0);
    }

    #[test]
    fn initialize_schema_creates_critical_request_indexes() {
        let store = create_store();
        let db = store.db.lock();

        let indexes = [
            "idx_requests_timestamp",
            "idx_requests_session",
            "idx_requests_model",
            "idx_requests_analyzed",
        ];

        for index in indexes {
            let exists: i64 = db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1)",
                    params![index],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(exists, 1, "expected index {index} to exist");
        }
    }

    fn sample_request(id: &str, model: &str) -> RequestRecord {
        RequestRecord {
            id: id.to_string(),
            session_id: Some("session-1".to_string()),
            timestamp: Utc::now(),
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            model: model.to_string(),
            stream: true,
            status_code: Some(200),
            status_kind: RequestStatusKind::Success,
            ttft_ms: Some(120.0),
            duration_ms: Some(450.0),
            input_tokens: Some(100),
            output_tokens: Some(250),
            cache_read_tokens: Some(20),
            cache_creation_tokens: Some(10),
            thinking_tokens: Some(5),
            request_size_bytes: 2048,
            response_size_bytes: 4096,
            stall_count: 0,
            stall_details_json: "[]".to_string(),
            error_summary: None,
            stop_reason: Some("end_turn".to_string()),
            content_block_types_json: "[\"text\"]".to_string(),
            anomalies_json: "[]".to_string(),
            analyzed: false,
        }
    }

    #[test]
    fn add_request_and_search_by_fts() {
        let store = create_store();

        let request = sample_request("req-1", "claude-opus-4-1");
        store.add_request(&request).unwrap();
        store
            .write_bodies(
                "req-1",
                "{\"input\":\"latency issue\"}",
                "{\"output\":\"observed stall\"}",
                false,
            )
            .unwrap();

        let ids = store.search_request_ids("stall", 10, 0).unwrap();
        assert_eq!(ids, vec!["req-1".to_string()]);
    }

    #[test]
    fn add_request_rejects_request_size_overflow() {
        let store = create_store();
        let mut request = sample_request("req-overflow", "claude-opus-4-1");
        request.request_size_bytes = u64::MAX;

        let err = store.add_request(&request).unwrap_err();
        assert!(matches!(err, rusqlite::Error::ToSqlConversionFailure(_)));
    }

    #[test]
    fn write_bodies_replace_updates_fts_terms() {
        let store = create_store();
        let request = sample_request("req-replace", "claude-opus-4-1");
        store.add_request(&request).unwrap();

        store
            .write_bodies(
                "req-replace",
                "first payload old_term",
                "first response",
                false,
            )
            .unwrap();
        assert_eq!(
            store.search_request_ids("old_term", 10, 0).unwrap(),
            vec!["req-replace".to_string()]
        );

        store
            .write_bodies(
                "req-replace",
                "second payload new_term",
                "second response",
                false,
            )
            .unwrap();

        assert!(store
            .search_request_ids("old_term", 10, 0)
            .unwrap()
            .is_empty());
        assert_eq!(
            store.search_request_ids("new_term", 10, 0).unwrap(),
            vec!["req-replace".to_string()]
        );
    }

    #[test]
    fn search_request_ids_supports_limit_and_offset_pagination() {
        let store = create_store();

        for idx in 0..4 {
            let request_id = format!("req-page-{idx}");
            let request = sample_request(&request_id, "claude-opus-4-1");
            store.add_request(&request).unwrap();
            store
                .write_bodies(
                    &request_id,
                    &format!("shared pagination term {idx}"),
                    "response",
                    false,
                )
                .unwrap();
        }

        let all = store.search_request_ids("shared", 10, 0).unwrap();
        assert_eq!(all.len(), 4);

        let page = store.search_request_ids("shared", 2, 1).unwrap();
        assert_eq!(page, all[1..3].to_vec());
    }

    #[test]
    fn write_bodies_persists_truncated_flag() {
        let store = create_store();
        let request = sample_request("req-truncated", "claude-opus-4-1");
        store.add_request(&request).unwrap();

        store
            .write_bodies("req-truncated", "request", "response", true)
            .unwrap();

        let db = store.db.lock();
        let truncated: i64 = db
            .query_row(
                "SELECT truncated FROM request_bodies WHERE request_id = ?1",
                params!["req-truncated"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(truncated, 1);
    }

    #[test]
    fn search_request_ids_blank_query_returns_empty() {
        let store = create_store();
        let request = sample_request("req-blank", "claude-opus-4-1");
        store.add_request(&request).unwrap();
        store
            .write_bodies("req-blank", "some searchable body", "response", false)
            .unwrap();

        assert!(store.search_request_ids("", 10, 0).unwrap().is_empty());
        assert!(store
            .search_request_ids("   \t\n", 10, 0)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn tool_usage_insert_and_read() {
        let store = create_store();
        let db = store.db.lock();
        db.execute(
            "INSERT INTO requests(id, timestamp_ms, method, path, model) VALUES(?1, ?2, ?3, ?4, ?5)",
            params!["req-tool", 1_i64, "POST", "/v1/messages", "claude"],
        ).unwrap();
        drop(db);

        store
            .insert_tool_usage("req-tool", "read_file", r#"{"path":"foo.rs"}"#)
            .unwrap();
        let tools = store.get_tool_usage_for_request("req-tool").unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["tool_name"], "read_file");
    }

    #[test]
    fn anomaly_by_id_returns_none_for_missing() {
        let store = create_store();
        let result = store.get_anomaly_by_id("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn list_all_model_stats_groups_by_model() {
        let store = create_store();
        let db = store.db.lock();
        db.execute(
            "INSERT INTO requests(id, timestamp_ms, method, path, model, status_code) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params!["req-1", 1_i64, "POST", "/v1/messages", "claude-sonnet", 200],
        ).unwrap();
        db.execute(
            "INSERT INTO requests(id, timestamp_ms, method, path, model, status_code) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params!["req-2", 2_i64, "POST", "/v1/messages", "claude-sonnet", 500],
        ).unwrap();
        drop(db);

        let stats = store.list_all_model_stats().unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0]["model"], "claude-sonnet");
        assert_eq!(stats[0]["request_count"], 2);
        assert_eq!(stats[0]["error_count"], 1);
    }
}
