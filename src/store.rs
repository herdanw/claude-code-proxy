use parking_lot::Mutex;
use rusqlite::Connection;
use std::path::Path;
use std::sync::Arc;

pub struct Store {
    db: Arc<Mutex<Connection>>,
}

impl Store {
    pub fn new(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_schema_creates_core_tables() {
        let path = std::env::temp_dir().join(format!("proxy-v2-{}.db", uuid::Uuid::new_v4()));
        let store = Store::new(&path).unwrap();
        let tables = store.list_table_names().unwrap();

        assert!(tables.contains(&"requests".to_string()));
        assert!(tables.contains(&"request_bodies".to_string()));
        assert!(tables.contains(&"request_bodies_fts".to_string()));
        assert!(tables.contains(&"tool_usage".to_string()));
        assert!(tables.contains(&"anomalies".to_string()));
        assert!(tables.contains(&"model_profiles".to_string()));
        assert!(tables.contains(&"sessions".to_string()));
    }
}
