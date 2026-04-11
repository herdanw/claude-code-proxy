use crate::types::RequestRecord;
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Arc;

pub struct Store {
    db: Arc<Mutex<Connection>>,
}

impl Store {
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

            CREATE TABLE IF NOT EXISTS sessions (
                session_id TEXT PRIMARY KEY,
                first_seen_ms INTEGER NOT NULL,
                last_seen_ms INTEGER NOT NULL,
                request_count INTEGER NOT NULL DEFAULT 0,
                error_count INTEGER NOT NULL DEFAULT 0,
                total_input_tokens INTEGER NOT NULL DEFAULT 0,
                total_output_tokens INTEGER NOT NULL DEFAULT 0
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
                req.request_size_bytes as i64,
                req.response_size_bytes as i64,
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
            "INSERT OR REPLACE INTO request_bodies (request_id, request_body, response_body, truncated) VALUES (?1, ?2, ?3, ?4)",
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
        assert!(tables.contains(&"sessions".to_string()));
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
        assert!(err
            .to_string()
            .contains("FOREIGN KEY constraint failed"));
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

        db.execute("DELETE FROM request_bodies WHERE request_id = ?1", params!["req-1"])
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
}
