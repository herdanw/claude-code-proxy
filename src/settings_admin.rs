use crate::stats::StatsStore;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};


const BACKUP_RETENTION_MAX: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsHistorySource {
    Revision,
    Legacy,
}

fn resolve_settings_history_source() -> SettingsHistorySource {
    match std::env::var("SETTINGS_HISTORY_SOURCE") {
        Ok(value) if value.eq_ignore_ascii_case("legacy") => SettingsHistorySource::Legacy,
        _ => SettingsHistorySource::Revision,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxySettingsDocument {
    pub raw_json: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeSettingsDocument {
    pub raw_json: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsCurrentResponse {
    pub updated_at_ms: i64,
    pub proxy_settings: ProxySettingsDocument,
    pub claude_settings: ClaudeSettingsDocument,
    pub db_file_mismatch: bool,
    pub file_recreated_from_db: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsHistoryRecord {
    pub revision_id: i64,
    pub id: String,
    pub saved_at_ms: i64,
    pub outcome: String,
    pub error_message: Option<String>,
    pub backup_id: Option<String>,
    pub tags: Vec<String>,
    pub proxy_settings: ProxySettingsDocument,
    pub claude_settings: ClaudeSettingsDocument,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsBackupRecord {
    pub id: String,
    pub path: String,
    pub created_at_ms: i64,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsAdminError {
    pub code: String,
    pub message: String,
}

pub struct SettingsAdmin {
    db_path: PathBuf,
    claude_dir: PathBuf,
    history_source: SettingsHistorySource,
    #[cfg(test)]
    fail_primary_db_write_once: AtomicBool,
}

impl SettingsAdmin {
    pub fn new(store: Arc<StatsStore>) -> Self {
        Self {
            db_path: store.database_path().to_path_buf(),
            claude_dir: store.claude_dir().to_path_buf(),
            history_source: resolve_settings_history_source(),
            #[cfg(test)]
            fail_primary_db_write_once: AtomicBool::new(false),
        }
    }

    pub fn get_current(&self) -> Result<Option<SettingsCurrentResponse>, SettingsAdminError> {
        let conn = self.open_db()?;
        self.ensure_schema(&conn)?;

        // Step 1: Query DB for singleton row
        let db_snapshot: Option<(i64, String, String)> = conn
            .query_row(
                "
                SELECT updated_at_ms, proxy_settings_json, claude_settings_json
                FROM settings_current
                WHERE singleton_id = 1
                ",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|err| SettingsAdminError {
                code: "db_read_failed".to_string(),
                message: format!("failed to load current settings snapshot: {err}"),
            })?;

        // Step 2: Read disk file
        let disk_snapshot = self.load_current_from_settings_file()?;

        match (db_snapshot, disk_snapshot) {
            // Both exist: compare DB claude_settings vs disk content
            (Some((updated_at_ms, proxy_json_text, db_claude_json_text)), Some(disk)) => {
                let db_claude_value: Value = serde_json::from_str(&db_claude_json_text)
                    .unwrap_or(Value::Object(Default::default()));
                let disk_claude_value = &disk.claude_settings.raw_json;

                // Normalize both to canonical JSON strings for comparison
                let db_normalized = serde_json::to_string(&db_claude_value).unwrap_or_default();
                let disk_normalized = serde_json::to_string(disk_claude_value).unwrap_or_default();
                let db_file_mismatch = db_normalized != disk_normalized;

                Ok(Some(SettingsCurrentResponse {
                    updated_at_ms,
                    proxy_settings: ProxySettingsDocument {
                        raw_json: serde_json::from_str(&proxy_json_text)
                            .unwrap_or(Value::Object(Default::default())),
                    },
                    claude_settings: ClaudeSettingsDocument {
                        raw_json: disk_claude_value.clone(),
                    },
                    db_file_mismatch,
                    file_recreated_from_db: false,
                }))
            }
            // DB only, no file: recreate settings.json from DB snapshot
            (Some((updated_at_ms, proxy_json_text, db_claude_json_text)), None) => {
                let db_claude_value: Value = serde_json::from_str(&db_claude_json_text)
                    .unwrap_or(Value::Object(Default::default()));

                // Recreate settings.json from DB snapshot
                let serialized = serde_json::to_string_pretty(&db_claude_value)
                    .map_err(|err| SettingsAdminError {
                        code: "settings_write_failed".to_string(),
                        message: format!("failed to serialize DB snapshot for file recreation: {err}"),
                    })?;

                fs::create_dir_all(&self.claude_dir).map_err(|err| SettingsAdminError {
                    code: "settings_write_failed".to_string(),
                    message: format!("failed to ensure claude directory exists: {err}"),
                })?;

                self.write_settings_atomically(&serialized)?;

                Ok(Some(SettingsCurrentResponse {
                    updated_at_ms,
                    proxy_settings: ProxySettingsDocument {
                        raw_json: serde_json::from_str(&proxy_json_text)
                            .unwrap_or(Value::Object(Default::default())),
                    },
                    claude_settings: ClaudeSettingsDocument {
                        raw_json: db_claude_value,
                    },
                    db_file_mismatch: false,
                    file_recreated_from_db: true,
                }))
            }
            // File only, no DB
            (None, Some(disk)) => Ok(Some(disk)),
            // Neither
            (None, None) => Ok(None),
        }
    }


    pub fn get_history(
        &self,
        limit: usize,
        offset: usize,
        search: Option<&str>,
    ) -> Result<Vec<SettingsHistoryRecord>, SettingsAdminError> {
        match self.history_source {
            SettingsHistorySource::Revision => self.get_history_from_revision(limit, offset, search),
            SettingsHistorySource::Legacy => self.get_history_from_legacy(limit, offset, search),
        }
    }

    fn get_history_from_revision(
        &self,
        limit: usize,
        offset: usize,
        search: Option<&str>,
    ) -> Result<Vec<SettingsHistoryRecord>, SettingsAdminError> {
        let conn = self.open_db()?;
        self.ensure_schema(&conn)?;

        let mut stmt = conn
            .prepare(
                "
                SELECT r.id, r.legacy_id, r.created_at_ms, r.outcome, r.error_message, r.settings_json
                FROM settings_revision r
                ORDER BY r.created_at_ms DESC, r.id DESC
                ",
            )
            .map_err(|err| SettingsAdminError {
                code: "db_read_failed".to_string(),
                message: format!("failed to prepare revision history query: {err}"),
            })?;

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(|err| SettingsAdminError {
                code: "db_read_failed".to_string(),
                message: format!("failed to query revision history: {err}"),
            })?;

        let mut history = Vec::new();
        for row in rows {
            let (revision_id, legacy_id, created_at_ms, outcome, error_message, settings_json) =
                row.map_err(|err| SettingsAdminError {
                    code: "db_read_failed".to_string(),
                    message: format!("failed to decode revision history row: {err}"),
                })?;

            let settings_value: Value = serde_json::from_str(&settings_json)
                .unwrap_or(Value::Object(Default::default()));

            let mut tag_stmt = conn
                .prepare(
                    "
                    SELECT tag
                    FROM settings_revision_tags
                    WHERE revision_id = ?1
                    ORDER BY tag ASC
                    ",
                )
                .map_err(|err| SettingsAdminError {
                    code: "db_read_failed".to_string(),
                    message: format!("failed to prepare revision tags query: {err}"),
                })?;

            let mut tags: Vec<String> = tag_stmt
                .query_map(params![revision_id], |row| row.get(0))
                .map_err(|err| SettingsAdminError {
                    code: "db_read_failed".to_string(),
                    message: format!("failed to query revision tags: {err}"),
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|err| SettingsAdminError {
                    code: "db_read_failed".to_string(),
                    message: format!("failed to decode revision tags: {err}"),
                })?;

            if tags.is_empty() {
                tags = derive_settings_tags(&settings_value);
            }

            history.push(SettingsHistoryRecord {
                revision_id,
                id: legacy_id,
                saved_at_ms: created_at_ms,
                outcome,
                error_message,
                backup_id: None,
                tags,
                proxy_settings: ProxySettingsDocument {
                    raw_json: settings_value.clone(),
                },
                claude_settings: ClaudeSettingsDocument {
                    raw_json: settings_value,
                },
            });
        }

        self.filter_and_paginate_history(history, limit, offset, search)
    }

    fn get_history_from_legacy(
        &self,
        limit: usize,
        offset: usize,
        search: Option<&str>,
    ) -> Result<Vec<SettingsHistoryRecord>, SettingsAdminError> {
        let conn = self.open_db()?;
        self.ensure_schema(&conn)?;

        let normalized_search = search
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(normalize_search_text);

        if normalized_search.is_none() {
            let mut stmt = conn
                .prepare(
                    "
                    SELECT id, saved_at_ms, outcome, error_message, backup_id, tags_json, proxy_settings_json, claude_settings_json
                    FROM settings_history
                    ORDER BY saved_at_ms DESC, id DESC
                    LIMIT ?1 OFFSET ?2
                    ",
                )
                .map_err(|err| SettingsAdminError {
                    code: "db_read_failed".to_string(),
                    message: format!("failed to prepare legacy history query: {err}"),
                })?;

            let rows = stmt
                .query_map(params![limit as i64, offset as i64], |row| {
                    let tags_json: String = row.get(5)?;
                    let proxy_json: String = row.get(6)?;
                    let claude_json: String = row.get(7)?;

                    let proxy_raw_json = serde_json::from_str(&proxy_json)
                        .unwrap_or(Value::Object(Default::default()));
                    let claude_raw_json = serde_json::from_str(&claude_json)
                        .unwrap_or(Value::Object(Default::default()));

                    let mut tags = parse_tags_json(&tags_json);
                    if tags.is_empty() {
                        tags = derive_settings_tags(&claude_raw_json);
                    }

                    Ok(SettingsHistoryRecord {
                        revision_id: 0,
                        id: row.get(0)?,
                        saved_at_ms: row.get(1)?,
                        outcome: row.get(2)?,
                        error_message: row.get(3)?,
                        backup_id: row.get(4)?,
                        tags,
                        proxy_settings: ProxySettingsDocument {
                            raw_json: proxy_raw_json,
                        },
                        claude_settings: ClaudeSettingsDocument {
                            raw_json: claude_raw_json,
                        },
                    })
                })
                .map_err(|err| SettingsAdminError {
                    code: "db_read_failed".to_string(),
                    message: format!("failed to query legacy history: {err}"),
                })?;

            let mut history = Vec::new();
            for row in rows {
                history.push(row.map_err(|err| SettingsAdminError {
                    code: "db_read_failed".to_string(),
                    message: format!("failed to decode legacy history row: {err}"),
                })?);
            }

            return Ok(history);
        }

        let mut stmt = conn
            .prepare(
                "
                SELECT id, saved_at_ms, outcome, error_message, backup_id, tags_json, proxy_settings_json, claude_settings_json
                FROM settings_history
                ORDER BY saved_at_ms DESC, id DESC
                ",
            )
            .map_err(|err| SettingsAdminError {
                code: "db_read_failed".to_string(),
                message: format!("failed to prepare legacy history search query: {err}"),
            })?;

        let rows = stmt
            .query_map([], |row| {
                let tags_json: String = row.get(5)?;
                let proxy_json: String = row.get(6)?;
                let claude_json: String = row.get(7)?;

                let proxy_raw_json =
                    serde_json::from_str(&proxy_json).unwrap_or(Value::Object(Default::default()));
                let claude_raw_json = serde_json::from_str(&claude_json)
                    .unwrap_or(Value::Object(Default::default()));

                let mut tags = parse_tags_json(&tags_json);
                if tags.is_empty() {
                    tags = derive_settings_tags(&claude_raw_json);
                }

                Ok(SettingsHistoryRecord {
                    revision_id: 0,
                    id: row.get(0)?,
                    saved_at_ms: row.get(1)?,
                    outcome: row.get(2)?,
                    error_message: row.get(3)?,
                    backup_id: row.get(4)?,
                    tags,
                    proxy_settings: ProxySettingsDocument {
                        raw_json: proxy_raw_json,
                    },
                    claude_settings: ClaudeSettingsDocument {
                        raw_json: claude_raw_json,
                    },
                })
            })
            .map_err(|err| SettingsAdminError {
                code: "db_read_failed".to_string(),
                message: format!("failed to query legacy history search: {err}"),
            })?;

        let mut history = Vec::new();
        for row in rows {
            history.push(row.map_err(|err| SettingsAdminError {
                code: "db_read_failed".to_string(),
                message: format!("failed to decode legacy history search row: {err}"),
            })?);
        }

        self.filter_and_paginate_history(history, limit, offset, search)
    }

    fn filter_and_paginate_history(
        &self,
        mut history: Vec<SettingsHistoryRecord>,
        limit: usize,
        offset: usize,
        search: Option<&str>,
    ) -> Result<Vec<SettingsHistoryRecord>, SettingsAdminError> {
        let query = search
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(normalize_search_text);

        if let Some(query) = query {
            history.retain(|item| {
                let tag_match = item
                    .tags
                    .iter()
                    .map(normalize_search_text)
                    .any(|tag| tag.contains(&query));

                let text_match = normalize_search_text(&format!(
                    "{} {} {} {} {} {}",
                    item.id,
                    item.outcome,
                    item.error_message.as_deref().unwrap_or_default(),
                    item.backup_id.as_deref().unwrap_or_default(),
                    item.proxy_settings.raw_json,
                    item.claude_settings.raw_json,
                ))
                .contains(&query);

                tag_match || text_match
            });
        }

        Ok(history.into_iter().skip(offset).take(limit).collect())
    }

    pub fn patch_history_tags(
        &self,
        revision_id: i64,
        add: &[String],
        remove: &[String],
    ) -> Result<SettingsHistoryRecord, SettingsAdminError> {
        if revision_id <= 0 {
            return Err(SettingsAdminError {
                code: "not_found".to_string(),
                message: "settings revision not found".to_string(),
            });
        }

        let mut add_tags = Vec::new();
        for value in add {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(SettingsAdminError {
                    code: "invalid_payload".to_string(),
                    message: "tag values must be non-empty".to_string(),
                });
            }
            if let Some(normalized) = canonicalize_tag(trimmed) {
                add_tags.push(normalized);
            } else {
                return Err(SettingsAdminError {
                    code: "invalid_payload".to_string(),
                    message: "tag values must be non-empty".to_string(),
                });
            }
        }

        let mut remove_tags = Vec::new();
        for value in remove {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(SettingsAdminError {
                    code: "invalid_payload".to_string(),
                    message: "tag values must be non-empty".to_string(),
                });
            }
            if let Some(normalized) = canonicalize_tag(trimmed) {
                remove_tags.push(normalized);
            } else {
                return Err(SettingsAdminError {
                    code: "invalid_payload".to_string(),
                    message: "tag values must be non-empty".to_string(),
                });
            }
        }

        let mut conn = self.open_db()?;
        self.ensure_schema(&conn)?;

        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|err| SettingsAdminError {
                code: "db_write_failed".to_string(),
                message: format!("failed to open settings tag patch transaction: {err}"),
            })?;

        let legacy_id: Option<String> = tx
            .query_row(
                "SELECT legacy_id FROM settings_revision WHERE id = ?1",
                params![revision_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|err| SettingsAdminError {
                code: "db_read_failed".to_string(),
                message: format!("failed to load settings revision: {err}"),
            })?;

        let Some(legacy_id) = legacy_id else {
            return Err(SettingsAdminError {
                code: "not_found".to_string(),
                message: "settings revision not found".to_string(),
            });
        };

        for tag in &add_tags {
            tx.execute(
                "
                INSERT OR IGNORE INTO settings_revision_tags(revision_id, tag)
                VALUES (?1, ?2)
                ",
                params![revision_id, tag],
            )
            .map_err(|err| SettingsAdminError {
                code: "db_write_failed".to_string(),
                message: format!("failed to add settings revision tag: {err}"),
            })?;
        }

        for tag in &remove_tags {
            tx.execute(
                "
                DELETE FROM settings_revision_tags
                WHERE revision_id = ?1 AND tag = ?2
                ",
                params![revision_id, tag],
            )
            .map_err(|err| SettingsAdminError {
                code: "db_write_failed".to_string(),
                message: format!("failed to remove settings revision tag: {err}"),
            })?;
        }

        let tags = {
            let mut tags_stmt = tx
                .prepare(
                    "
                    SELECT tag
                    FROM settings_revision_tags
                    WHERE revision_id = ?1
                    ORDER BY tag ASC
                    ",
                )
                .map_err(|err| SettingsAdminError {
                    code: "db_read_failed".to_string(),
                    message: format!("failed to list patched settings revision tags: {err}"),
                })?;
            let tag_rows = tags_stmt
                .query_map(params![revision_id], |row| row.get::<_, String>(0))
                .map_err(|err| SettingsAdminError {
                    code: "db_read_failed".to_string(),
                    message: format!("failed to list patched settings revision tags: {err}"),
                })?;
            let mut tags = Vec::new();
            for row in tag_rows {
                tags.push(row.map_err(|err| SettingsAdminError {
                    code: "db_read_failed".to_string(),
                    message: format!("failed to decode patched settings revision tags: {err}"),
                })?);
            }
            tags
        };


        let tags_json = serde_json::to_string(&tags).map_err(|err| SettingsAdminError {
            code: "db_write_failed".to_string(),
            message: format!("failed to serialize patched settings tags: {err}"),
        })?;

        tx.execute(
            "UPDATE settings_history SET tags_json = ?1 WHERE id = ?2",
            params![tags_json, legacy_id],
        )
        .map_err(|err| SettingsAdminError {
            code: "db_write_failed".to_string(),
            message: format!("failed to persist patched settings history tags: {err}"),
        })?;

        tx.commit().map_err(|err| SettingsAdminError {
            code: "db_write_failed".to_string(),
            message: format!("failed to commit settings tag patch: {err}"),
        })?;

        self.get_history_by_revision_id(revision_id)?
            .ok_or_else(|| SettingsAdminError {
                code: "not_found".to_string(),
                message: "settings revision not found".to_string(),
            })
    }

    pub fn delete_history_revision(&self, revision_id: i64) -> Result<(), SettingsAdminError> {
        if revision_id <= 0 {
            return Err(SettingsAdminError {
                code: "not_found".to_string(),
                message: "settings revision not found".to_string(),
            });
        }

        let mut conn = self.open_db()?;
        self.ensure_schema(&conn)?;

        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|err| SettingsAdminError {
                code: "db_write_failed".to_string(),
                message: format!("failed to open history delete transaction: {err}"),
            })?;

        let legacy_id: Option<String> = tx
            .query_row(
                "SELECT legacy_id FROM settings_revision WHERE id = ?1",
                params![revision_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|err| SettingsAdminError {
                code: "db_read_failed".to_string(),
                message: format!("failed to load settings revision before delete: {err}"),
            })?;

        let Some(legacy_id) = legacy_id else {
            return Err(SettingsAdminError {
                code: "not_found".to_string(),
                message: "settings revision not found".to_string(),
            });
        };

        tx.execute(
            "DELETE FROM settings_revision WHERE id = ?1",
            params![revision_id],
        )
        .map_err(|err| SettingsAdminError {
            code: "db_write_failed".to_string(),
            message: format!("failed to delete settings revision: {err}"),
        })?;

        tx.execute(
            "DELETE FROM settings_history WHERE id = ?1",
            params![legacy_id],
        )
        .map_err(|err| SettingsAdminError {
            code: "db_write_failed".to_string(),
            message: format!("failed to delete settings history row: {err}"),
        })?;

        tx.commit().map_err(|err| SettingsAdminError {
            code: "db_write_failed".to_string(),
            message: format!("failed to commit settings revision delete: {err}"),
        })?;

        Ok(())
    }

    pub fn delete_history_all(&self) -> Result<usize, SettingsAdminError> {
        let mut conn = self.open_db()?;
        self.ensure_schema(&conn)?;

        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|err| SettingsAdminError {
                code: "db_write_failed".to_string(),
                message: format!("failed to open history clear transaction: {err}"),
            })?;

        let deleted_count = tx
            .execute("DELETE FROM settings_revision", [])
            .map_err(|err| SettingsAdminError {
                code: "db_write_failed".to_string(),
                message: format!("failed to clear settings revisions: {err}"),
            })?;

        tx.execute("DELETE FROM settings_history", [])
            .map_err(|err| SettingsAdminError {
                code: "db_write_failed".to_string(),
                message: format!("failed to clear settings history rows: {err}"),
            })?;

        tx.commit().map_err(|err| SettingsAdminError {
            code: "db_write_failed".to_string(),
            message: format!("failed to commit settings history clear: {err}"),
        })?;

        Ok(deleted_count)
    }

    fn get_history_by_revision_id(
        &self,
        revision_id: i64,
    ) -> Result<Option<SettingsHistoryRecord>, SettingsAdminError> {
        let conn = self.open_db()?;
        self.ensure_schema(&conn)?;

        let revision_row: Option<(i64, String, i64, String, Option<String>, String)> = conn
            .query_row(
                "
                SELECT id, legacy_id, created_at_ms, outcome, error_message, settings_json
                FROM settings_revision
                WHERE id = ?1
                ",
                params![revision_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|err| SettingsAdminError {
                code: "db_read_failed".to_string(),
                message: format!("failed to load settings revision by id: {err}"),
            })?;

        let Some((revision_id, legacy_id, created_at_ms, outcome, error_message, settings_json)) =
            revision_row
        else {
            return Ok(None);
        };

        let mut tags_stmt = conn
            .prepare(
                "
                SELECT tag
                FROM settings_revision_tags
                WHERE revision_id = ?1
                ORDER BY tag ASC
                ",
            )
            .map_err(|err| SettingsAdminError {
                code: "db_read_failed".to_string(),
                message: format!("failed to prepare settings revision tags lookup: {err}"),
            })?;

        let mut tags: Vec<String> = tags_stmt
            .query_map(params![revision_id], |row| row.get::<_, String>(0))
            .map_err(|err| SettingsAdminError {
                code: "db_read_failed".to_string(),
                message: format!("failed to query settings revision tags: {err}"),
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| SettingsAdminError {
                code: "db_read_failed".to_string(),
                message: format!("failed to decode settings revision tags: {err}"),
            })?;

        let backup_row: Option<(Option<String>, Option<String>)> = conn
            .query_row(
                "
                SELECT backup_id, tags_json
                FROM settings_history
                WHERE id = ?1
                ",
                params![&legacy_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|err| SettingsAdminError {
                code: "db_read_failed".to_string(),
                message: format!("failed to load legacy settings history metadata: {err}"),
            })?;

        if tags.is_empty() {
            if let Some((_, Some(tags_json))) = backup_row.as_ref() {
                tags = parse_tags_json(tags_json);
            }
        }

        let settings_value: Value =
            serde_json::from_str(&settings_json).unwrap_or(Value::Object(Default::default()));

        if tags.is_empty() {
            tags = derive_settings_tags(&settings_value);
        }

        let backup_id = backup_row.and_then(|(backup_id, _)| backup_id);

        Ok(Some(SettingsHistoryRecord {
            revision_id,
            id: legacy_id,
            saved_at_ms: created_at_ms,
            outcome,
            error_message,
            backup_id,
            tags,
            proxy_settings: ProxySettingsDocument {
                raw_json: settings_value.clone(),
            },
            claude_settings: ClaudeSettingsDocument {
                raw_json: settings_value,
            },
        }))
    }

    pub fn list_backups(&self) -> Result<Vec<SettingsBackupRecord>, SettingsAdminError> {
        let backup_dir = self.backup_dir();
        if !backup_dir.exists() {
            return Ok(Vec::new());
        }

        let mut backups = Vec::new();
        let entries = fs::read_dir(&backup_dir).map_err(|err| SettingsAdminError {
            code: "backup_list_failed".to_string(),
            message: format!("failed to read backup directory: {err}"),
        })?;

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };

            let metadata = match entry.metadata() {
                Ok(value) => value,
                Err(_) => continue,
            };

            let created_at_ms = metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|value| value.as_millis() as i64)
                .unwrap_or(0);

            backups.push(SettingsBackupRecord {
                id: file_name.to_string(),
                path: path.to_string_lossy().to_string(),
                created_at_ms,
                size_bytes: metadata.len(),
            });
        }

        backups.sort_by(|a, b| {
            b.created_at_ms
                .cmp(&a.created_at_ms)
                .then_with(|| b.id.cmp(&a.id))
        });

        Ok(backups)
    }

    pub fn apply_settings(
        &self,
        payload: Value,
    ) -> Result<SettingsCurrentResponse, SettingsAdminError> {
        let (effective_settings, quick_tags) = split_apply_payload(payload)?;

        fs::create_dir_all(&self.claude_dir).map_err(|err| SettingsAdminError {
            code: "settings_write_failed".to_string(),
            message: format!("failed to ensure claude directory exists: {err}"),
        })?;

        let now_ms = Utc::now().timestamp_millis();
        let backup_id = self.create_backup_if_present(now_ms)?;

        let serialized = serde_json::to_string_pretty(&effective_settings).map_err(|err| SettingsAdminError {
            code: "invalid_payload".to_string(),
            message: format!("failed to serialize settings payload: {err}"),
        })?;

        self.write_settings_atomically(&serialized)?;

        self.prune_backups_to_retention()?;

        let proxy_json = effective_settings.clone();
        let claude_json = effective_settings;

        let proxy_json_text = serde_json::to_string(&proxy_json).map_err(|err| SettingsAdminError {
            code: "invalid_payload".to_string(),
            message: format!("failed to serialize proxy settings snapshot: {err}"),
        })?;
        let claude_json_text = serde_json::to_string(&claude_json).map_err(|err| SettingsAdminError {
            code: "invalid_payload".to_string(),
            message: format!("failed to serialize claude settings snapshot: {err}"),
        })?;

        let settings_json_text = claude_json_text.clone();
        let content_hash = stable_content_hash(&settings_json_text);
        let mut deduped_tags = std::collections::BTreeSet::new();
        for tag in derive_settings_tags(&claude_json) {
            if let Some(normalized) = canonicalize_tag(&tag) {
                deduped_tags.insert(normalized);
            }
        }
        for tag in quick_tags {
            if let Some(normalized) = canonicalize_tag(&tag) {
                deduped_tags.insert(normalized);
            }
        }
        let tags: Vec<String> = deduped_tags.iter().cloned().collect();
        let tags_json_text = serde_json::to_string(&tags).map_err(|err| SettingsAdminError {
            code: "invalid_payload".to_string(),
            message: format!("failed to serialize settings tags: {err}"),
        })?;

        let mut conn = self.open_db()?;
        self.ensure_schema(&conn)?;

        let history_source = self.history_source;
        let write_outcome: Result<(), String> = if self.should_fail_primary_db_write_for_test() {
            Err("simulated primary db write failure".to_string())
        } else {
            self.persist_settings_snapshot(
                &mut conn,
                history_source,
                now_ms,
                backup_id.as_deref(),
                &tags_json_text,
                &proxy_json_text,
                &claude_json_text,
                &content_hash,
                &settings_json_text,
                &tags,
            )
        };


        if let Err(primary_err) = write_outcome {
            let _ = self.append_failure_history_row(
                &conn,
                now_ms,
                backup_id.as_deref(),
                &tags_json_text,
                &proxy_json_text,
                &claude_json_text,
                &content_hash,
                &settings_json_text,
                &primary_err,
            );

            return Err(SettingsAdminError {
                code: "db_write_failed".to_string(),
                message: format!(
                    "settings.json was written but database snapshot persistence failed: {primary_err}"
                ),
            });
        }

        Ok(SettingsCurrentResponse {
            updated_at_ms: now_ms,
            proxy_settings: ProxySettingsDocument {
                raw_json: proxy_json,
            },
            claude_settings: ClaudeSettingsDocument {
                raw_json: claude_json,
            },
            db_file_mismatch: false,
            file_recreated_from_db: false,
        })
    }

    pub fn delete_backups_selected(&self, ids: &[String]) -> Result<usize, SettingsAdminError> {
        let backup_dir = self.backup_dir();
        if !backup_dir.exists() {
            return Ok(0);
        }

        let canonical_backup_dir = backup_dir
            .canonicalize()
            .map_err(|err| SettingsAdminError {
                code: "backup_delete_failed".to_string(),
                message: format!("failed to resolve backup directory: {err}"),
            })?;

        let mut deleted = 0usize;
        for id in ids {
            self.ensure_safe_backup_id(id)?;

            let path = backup_dir.join(id);
            if !path.exists() || !path.is_file() {
                continue;
            }

            let canonical_path = path.canonicalize().map_err(|err| SettingsAdminError {
                code: "backup_delete_failed".to_string(),
                message: format!("failed to resolve backup file path: {err}"),
            })?;

            if !canonical_path.starts_with(&canonical_backup_dir) {
                return Err(SettingsAdminError {
                    code: "backup_delete_failed".to_string(),
                    message: "backup id resolves outside backup directory".to_string(),
                });
            }

            fs::remove_file(canonical_path).map_err(|err| SettingsAdminError {
                code: "backup_delete_failed".to_string(),
                message: format!("failed to delete backup file: {err}"),
            })?;
            deleted += 1;
        }

        Ok(deleted)
    }


    pub fn delete_backups_all(&self) -> Result<usize, SettingsAdminError> {
        let backup_dir = self.backup_dir();
        if !backup_dir.exists() {
            return Ok(0);
        }

        let mut deleted = 0usize;
        let entries = fs::read_dir(&backup_dir).map_err(|err| SettingsAdminError {
            code: "backup_delete_failed".to_string(),
            message: format!("failed to read backup directory: {err}"),
        })?;

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            fs::remove_file(path).map_err(|err| SettingsAdminError {
                code: "backup_delete_failed".to_string(),
                message: format!("failed to delete backup file: {err}"),
            })?;
            deleted += 1;
        }

        Ok(deleted)
    }

    #[cfg(test)]
    pub fn trigger_db_failure_once_for_test(&self) {
        self.fail_primary_db_write_once.store(true, Ordering::SeqCst);
    }

    #[cfg(not(test))]
    fn should_fail_primary_db_write_for_test(&self) -> bool {
        false
    }

    #[cfg(test)]
    fn should_fail_primary_db_write_for_test(&self) -> bool {
        self.fail_primary_db_write_once.swap(false, Ordering::SeqCst)
    }


    fn open_db(&self) -> Result<Connection, SettingsAdminError> {
        Connection::open(&self.db_path).map_err(|err| SettingsAdminError {
            code: "db_open_failed".to_string(),
            message: format!("failed to open sqlite database: {err}"),
        })
    }
    fn persist_settings_snapshot(
        &self,
        conn: &mut Connection,
        history_source: SettingsHistorySource,
        now_ms: i64,
        backup_id: Option<&str>,
        tags_json_text: &str,
        proxy_json_text: &str,
        claude_json_text: &str,
        content_hash: &str,
        settings_json_text: &str,
        tags: &[String],
    ) -> Result<(), String> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|err| format!("failed to open settings transaction: {err}"))?;

        tx.execute(
            "
            INSERT INTO settings_current (
                singleton_id,
                updated_at_ms,
                proxy_settings_json,
                claude_settings_json
            ) VALUES (1, ?1, ?2, ?3)
            ON CONFLICT(singleton_id) DO UPDATE SET
                updated_at_ms = excluded.updated_at_ms,
                proxy_settings_json = excluded.proxy_settings_json,
                claude_settings_json = excluded.claude_settings_json
            ",
            params![now_ms, proxy_json_text, claude_json_text],
        )
        .map_err(|err| format!("failed to upsert settings_current: {err}"))?;

        match history_source {
            SettingsHistorySource::Legacy => {
                let legacy_id = uuid::Uuid::new_v4().to_string();
                tx.execute(
                    "
                    INSERT INTO settings_history (
                        id,
                        saved_at_ms,
                        outcome,
                        error_message,
                        backup_id,
                        tags_json,
                        proxy_settings_json,
                        claude_settings_json,
                        content_hash,
                        settings_json,
                        source
                    ) VALUES (?1, ?2, 'success', NULL, ?3, ?4, ?5, ?6, ?7, ?8, 'settings_admin')
                    ",
                    params![
                        &legacy_id,
                        now_ms,
                        backup_id,
                        tags_json_text,
                        proxy_json_text,
                        claude_json_text,
                        content_hash,
                        settings_json_text,
                    ],
                )
                .map_err(|err| format!("failed to append legacy history row: {err}"))?;
            }
            SettingsHistorySource::Revision => {
                let revision_legacy_id = uuid::Uuid::new_v4().to_string();
                tx.execute(
                    "
                    INSERT INTO settings_revision (
                        legacy_id,
                        created_at_ms,
                        outcome,
                        error_message,
                        settings_json
                    ) VALUES (?1, ?2, 'success', NULL, ?3)
                    ",
                    params![&revision_legacy_id, now_ms, settings_json_text],
                )
                .map_err(|err| format!("failed to append settings revision row: {err}"))?;

                let revision_id: i64 = tx
                    .query_row(
                        "SELECT id FROM settings_revision WHERE legacy_id = ?1",
                        params![&revision_legacy_id],
                        |row| row.get(0),
                    )
                    .map_err(|err| format!("failed to load settings revision id: {err}"))?;

                for tag in tags {
                    tx.execute(
                        "
                        INSERT OR IGNORE INTO settings_revision_tags(revision_id, tag)
                        VALUES (?1, ?2)
                        ",
                        params![revision_id, tag],
                    )
                    .map_err(|err| format!("failed to append settings revision tags: {err}"))?;
                }
            }
        }

        tx.commit()
            .map_err(|err| format!("failed to commit settings transaction: {err}"))
    }

    fn settings_path(&self) -> PathBuf {
        self.claude_dir.join("settings.json")
    }

    fn load_current_from_settings_file(&self) -> Result<Option<SettingsCurrentResponse>, SettingsAdminError> {
        let settings_path = self.settings_path();

        let raw_text = match fs::read_to_string(&settings_path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(SettingsAdminError {
                    code: "settings_read_failed".to_string(),
                    message: format!("failed to read settings.json: {err}"),
                })
            }
        };

        let parsed: Value = serde_json::from_str(&raw_text).map_err(|err| SettingsAdminError {
            code: "settings_read_failed".to_string(),
            message: format!("failed to parse settings.json: {err}"),
        })?;

        if !parsed.is_object() {
            return Err(SettingsAdminError {
                code: "settings_read_failed".to_string(),
                message: "settings.json must contain a JSON object".to_string(),
            });
        }

        let updated_at_ms = fs::metadata(&settings_path)
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or_else(|| Utc::now().timestamp_millis());

        Ok(Some(SettingsCurrentResponse {
            updated_at_ms,
            proxy_settings: ProxySettingsDocument {
                raw_json: parsed.clone(),
            },
            claude_settings: ClaudeSettingsDocument { raw_json: parsed },
            db_file_mismatch: false,
            file_recreated_from_db: false,
        }))
    }

    fn backup_dir(&self) -> PathBuf {
        self.claude_dir.join("settings-backups")
    }

    fn create_backup_if_present(&self, now_ms: i64) -> Result<Option<String>, SettingsAdminError> {
        let settings_path = self.settings_path();
        if !settings_path.exists() {
            return Ok(None);
        }

        let bytes = fs::read(&settings_path).map_err(|err| SettingsAdminError {
            code: "backup_create_failed".to_string(),
            message: format!("failed to read existing settings.json for backup: {err}"),
        })?;

        let backup_dir = self.backup_dir();
        fs::create_dir_all(&backup_dir).map_err(|err| SettingsAdminError {
            code: "backup_create_failed".to_string(),
            message: format!("failed to create backup directory: {err}"),
        })?;

        let backup_id = format!(
            "settings-{}-{}.json",
            now_ms,
            uuid::Uuid::new_v4().as_simple()
        );
        let backup_path = backup_dir.join(&backup_id);

        fs::write(backup_path, bytes).map_err(|err| SettingsAdminError {
            code: "backup_create_failed".to_string(),
            message: format!("failed to write backup file: {err}"),
        })?;

        Ok(Some(backup_id))
    }

    fn prune_backups_to_retention(&self) -> Result<(), SettingsAdminError> {
        let backups = self.list_backups()?;
        if backups.len() <= BACKUP_RETENTION_MAX {
            return Ok(());
        }

        let to_remove = &backups[BACKUP_RETENTION_MAX..];
        for backup in to_remove {
            fs::remove_file(&backup.path).map_err(|err| SettingsAdminError {
                code: "backup_prune_failed".to_string(),
                message: format!("failed to prune backup file: {err}"),
            })?;
        }

        Ok(())
    }

    fn write_settings_atomically(&self, serialized: &str) -> Result<(), SettingsAdminError> {
        let settings_path = self.settings_path();
        let temp_path = self
            .claude_dir
            .join(format!("settings.json.tmp-{}", uuid::Uuid::new_v4().as_simple()));

        {
            let mut file = fs::File::create(&temp_path).map_err(|err| SettingsAdminError {
                code: "settings_write_failed".to_string(),
                message: format!("failed to create temporary settings file: {err}"),
            })?;
            file.write_all(format!("{serialized}\n").as_bytes())
                .map_err(|err| SettingsAdminError {
                    code: "settings_write_failed".to_string(),
                    message: format!("failed to write temporary settings file: {err}"),
                })?;
            file.sync_all().map_err(|err| SettingsAdminError {
                code: "settings_write_failed".to_string(),
                message: format!("failed to flush temporary settings file: {err}"),
            })?;
        }

        fs::rename(&temp_path, &settings_path).map_err(|err| {
            let _ = fs::remove_file(&temp_path);
            SettingsAdminError {
                code: "settings_write_failed".to_string(),
                message: format!("failed to atomically replace settings.json: {err}"),
            }
        })?;

        {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&settings_path)
                .map_err(|err| SettingsAdminError {
                    code: "settings_write_failed".to_string(),
                    message: format!("failed to open settings.json for sync: {err}"),
                })?;
            file.sync_all().map_err(|err| SettingsAdminError {
                code: "settings_write_failed".to_string(),
                message: format!("failed to sync settings.json after rename: {err}"),
            })?;
        }

        #[cfg(unix)]
        {
            let dir = fs::File::open(&self.claude_dir).map_err(|err| SettingsAdminError {
                code: "settings_write_failed".to_string(),
                message: format!("failed to open settings directory for sync: {err}"),
            })?;
            dir.sync_all().map_err(|err| SettingsAdminError {
                code: "settings_write_failed".to_string(),
                message: format!("failed to sync settings directory after rename: {err}"),
            })?;
        }

        Ok(())
    }

    fn ensure_safe_backup_id(&self, backup_id: &str) -> Result<(), SettingsAdminError> {
        if backup_id.trim().is_empty() {
            return Err(SettingsAdminError {
                code: "backup_delete_failed".to_string(),
                message: "backup id cannot be empty".to_string(),
            });
        }

        let candidate = Path::new(backup_id);
        if candidate.components().count() != 1 || candidate.extension().and_then(|ext| ext.to_str()) != Some("json") {
            return Err(SettingsAdminError {
                code: "backup_delete_failed".to_string(),
                message: "invalid backup id".to_string(),
            });
        }

        if backup_id.contains("..") || backup_id.contains('/') || backup_id.contains('\\') {
            return Err(SettingsAdminError {
                code: "backup_delete_failed".to_string(),
                message: "invalid backup id".to_string(),
            });
        }

        Ok(())
    }

    fn ensure_schema(&self, conn: &Connection) -> Result<(), SettingsAdminError> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS settings_current (
                singleton_id INTEGER PRIMARY KEY CHECK(singleton_id = 1),
                updated_at_ms INTEGER NOT NULL,
                proxy_settings_json TEXT NOT NULL,
                claude_settings_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS settings_history (
                id TEXT PRIMARY KEY,
                saved_at_ms INTEGER NOT NULL,
                outcome TEXT NOT NULL,
                error_message TEXT,
                backup_id TEXT,
                tags_json TEXT NOT NULL DEFAULT '[]',
                proxy_settings_json TEXT NOT NULL,
                claude_settings_json TEXT NOT NULL,
                content_hash TEXT NOT NULL DEFAULT '',
                settings_json TEXT NOT NULL DEFAULT '{}',
                source TEXT NOT NULL DEFAULT 'settings_admin'
            );

            CREATE INDEX IF NOT EXISTS idx_settings_history_saved_at_ms
                ON settings_history(saved_at_ms DESC);

            CREATE TABLE IF NOT EXISTS settings_revision (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                legacy_id TEXT NOT NULL UNIQUE,
                created_at_ms INTEGER NOT NULL,
                outcome TEXT NOT NULL,
                error_message TEXT,
                settings_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS settings_revision_tags (
                revision_id INTEGER NOT NULL,
                tag TEXT NOT NULL,
                PRIMARY KEY (revision_id, tag),
                FOREIGN KEY (revision_id) REFERENCES settings_revision(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_settings_revision_created_at_ms_id
                ON settings_revision(created_at_ms DESC, legacy_id DESC);
            CREATE INDEX IF NOT EXISTS idx_settings_revision_tags_tag
                ON settings_revision_tags(tag);

            CREATE TABLE IF NOT EXISTS settings_revision_migration_meta (
                key TEXT PRIMARY KEY,
                value INTEGER NOT NULL
            );
            ",
        )
        .map_err(|err| SettingsAdminError {
            code: "db_migration_failed".to_string(),
            message: format!("failed to initialize settings schema: {err}"),
        })?;

        self.migrate_settings_history_columns(conn)?;
        self.backfill_settings_revisions_from_history(conn)?;
        Ok(())
    }

    fn migrate_settings_history_columns(&self, conn: &Connection) -> Result<(), SettingsAdminError> {
        let mut stmt = conn
            .prepare("PRAGMA table_info(settings_history)")
            .map_err(|err| SettingsAdminError {
                code: "db_migration_failed".to_string(),
                message: format!("failed to inspect settings_history schema: {err}"),
            })?;

        let column_rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|err| SettingsAdminError {
                code: "db_migration_failed".to_string(),
                message: format!("failed to list settings_history columns: {err}"),
            })?;

        let mut columns = Vec::new();
        for row in column_rows {
            if let Ok(name) = row {
                columns.push(name);
            }
        }

        if !columns.iter().any(|name| name == "outcome") {
            conn.execute(
                "ALTER TABLE settings_history ADD COLUMN outcome TEXT NOT NULL DEFAULT 'success'",
                [],
            )
            .map_err(|err| SettingsAdminError {
                code: "db_migration_failed".to_string(),
                message: format!("failed to add outcome column: {err}"),
            })?;
        }

        if !columns.iter().any(|name| name == "error_message") {
            conn.execute(
                "ALTER TABLE settings_history ADD COLUMN error_message TEXT",
                [],
            )
            .map_err(|err| SettingsAdminError {
                code: "db_migration_failed".to_string(),
                message: format!("failed to add error_message column: {err}"),
            })?;
        }

        if !columns.iter().any(|name| name == "backup_id") {
            conn.execute(
                "ALTER TABLE settings_history ADD COLUMN backup_id TEXT",
                [],
            )
            .map_err(|err| SettingsAdminError {
                code: "db_migration_failed".to_string(),
                message: format!("failed to add backup_id column: {err}"),
            })?;
        }

        if !columns.iter().any(|name| name == "tags_json") {
            conn.execute(
                "ALTER TABLE settings_history ADD COLUMN tags_json TEXT NOT NULL DEFAULT '[]'",
                [],
            )
            .map_err(|err| SettingsAdminError {
                code: "db_migration_failed".to_string(),
                message: format!("failed to add tags_json column: {err}"),
            })?;

            conn.execute(
                "
                UPDATE settings_history
                SET tags_json = '[]'
                WHERE tags_json IS NULL OR TRIM(tags_json) = ''
                ",
                [],
            )
            .map_err(|err| SettingsAdminError {
                code: "db_migration_failed".to_string(),
                message: format!("failed to normalize tags_json column: {err}"),
            })?;
        }

        if !columns.iter().any(|name| name == "proxy_settings_json") {
            conn.execute(
                "ALTER TABLE settings_history ADD COLUMN proxy_settings_json TEXT NOT NULL DEFAULT '{}'",
                [],
            )
            .map_err(|err| SettingsAdminError {
                code: "db_migration_failed".to_string(),
                message: format!("failed to add proxy_settings_json column: {err}"),
            })?;
        }

        if !columns.iter().any(|name| name == "claude_settings_json") {
            conn.execute(
                "ALTER TABLE settings_history ADD COLUMN claude_settings_json TEXT NOT NULL DEFAULT '{}'",
                [],
            )
            .map_err(|err| SettingsAdminError {
                code: "db_migration_failed".to_string(),
                message: format!("failed to add claude_settings_json column: {err}"),
            })?;
        }

        if !columns.iter().any(|name| name == "content_hash") {
            conn.execute(
                "ALTER TABLE settings_history ADD COLUMN content_hash TEXT NOT NULL DEFAULT ''",
                [],
            )
            .map_err(|err| SettingsAdminError {
                code: "db_migration_failed".to_string(),
                message: format!("failed to add content_hash column: {err}"),
            })?;
        }

        if !columns.iter().any(|name| name == "settings_json") {
            conn.execute(
                "ALTER TABLE settings_history ADD COLUMN settings_json TEXT NOT NULL DEFAULT '{}'",
                [],
            )
            .map_err(|err| SettingsAdminError {
                code: "db_migration_failed".to_string(),
                message: format!("failed to add settings_json column: {err}"),
            })?;
        }

        if !columns.iter().any(|name| name == "source") {
            conn.execute(
                "ALTER TABLE settings_history ADD COLUMN source TEXT NOT NULL DEFAULT 'settings_admin'",
                [],
            )
            .map_err(|err| SettingsAdminError {
                code: "db_migration_failed".to_string(),
                message: format!("failed to add source column: {err}"),
            })?;
        }

        if columns.iter().any(|name| name == "settings_json") {
            conn.execute(
                "
                UPDATE settings_history
                SET
                    proxy_settings_json = CASE
                        WHEN proxy_settings_json = '{}' OR proxy_settings_json = '' THEN settings_json
                        ELSE proxy_settings_json
                    END,
                    claude_settings_json = CASE
                        WHEN claude_settings_json = '{}' OR claude_settings_json = '' THEN settings_json
                        ELSE claude_settings_json
                    END
                ",
                [],
            )
            .map_err(|err| SettingsAdminError {
                code: "db_migration_failed".to_string(),
                message: format!("failed to backfill proxy/claude settings json columns: {err}"),
            })?;
        }

        Ok(())
    }


    fn backfill_settings_revisions_from_history(
        &self,
        conn: &Connection,
    ) -> Result<(), SettingsAdminError> {
        let already_migrated = conn
            .query_row(
                "
                SELECT value
                FROM settings_revision_migration_meta
                WHERE key = 'backfill_completed'
                ",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|err| SettingsAdminError {
                code: "db_migration_failed".to_string(),
                message: format!("failed to read revision migration state: {err}"),
            })?
            .unwrap_or(0)
            != 0;

        if already_migrated {
            return Ok(());
        }

        let mut warning_count = 0i64;

        let mut select_stmt = conn
            .prepare(
                "
                SELECT id, saved_at_ms, outcome, error_message, tags_json, claude_settings_json, settings_json
                FROM settings_history
                ORDER BY saved_at_ms DESC, id DESC
                ",
            )
            .map_err(|err| SettingsAdminError {
                code: "db_migration_failed".to_string(),
                message: format!("failed to query legacy settings history rows: {err}"),
            })?;

        let rows = select_stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(|err| SettingsAdminError {
                code: "db_migration_failed".to_string(),
                message: format!("failed to iterate legacy settings history rows: {err}"),
            })?;

        let legacy_rows: Vec<(String, i64, String, Option<String>, Option<String>, Option<String>, Option<String>)> =
            rows.collect::<Result<Vec<_>, _>>().map_err(|err| SettingsAdminError {
                code: "db_migration_failed".to_string(),
                message: format!("failed to decode legacy settings history row: {err}"),
            })?;

        drop(select_stmt);

        for (legacy_id, saved_at_ms, outcome, error_message, tags_json, claude_json, settings_json) in legacy_rows {
            let settings_json_raw = claude_json
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .or_else(|| settings_json.as_deref().filter(|value| !value.trim().is_empty()))
                .unwrap_or("{}");

            let parsed_settings: Value = match serde_json::from_str::<Value>(settings_json_raw) {
                Ok(value) if value.is_object() => value,
                _ => {
                    warning_count += 1;
                    continue;
                }
            };

            let canonical_settings_json =
                serde_json::to_string(&parsed_settings).map_err(|err| SettingsAdminError {
                    code: "db_migration_failed".to_string(),
                    message: format!("failed to serialize revision settings json: {err}"),
                })?;

            conn.execute(
                "
                INSERT INTO settings_revision (
                    legacy_id,
                    created_at_ms,
                    outcome,
                    error_message,
                    settings_json
                ) VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(legacy_id) DO NOTHING
                ",
                params![
                    legacy_id,
                    saved_at_ms,
                    outcome,
                    error_message,
                    canonical_settings_json,
                ],
            )
            .map_err(|err| SettingsAdminError {
                code: "db_migration_failed".to_string(),
                message: format!("failed to insert settings revision row: {err}"),
            })?;

            let revision_id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM settings_revision WHERE legacy_id = ?1",
                    params![legacy_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|err| SettingsAdminError {
                    code: "db_migration_failed".to_string(),
                    message: format!("failed to load settings revision id: {err}"),
                })?;

            let Some(revision_id) = revision_id else {
                warning_count += 1;
                continue;
            };

            let parsed_tags = parse_tags_json(tags_json.as_deref().unwrap_or(""));
            let mut deduped_tags = std::collections::BTreeSet::new();
            for tag in parsed_tags {
                if let Some(normalized) = canonicalize_tag(&tag) {
                    deduped_tags.insert(normalized);
                }
            }

            for tag in deduped_tags {
                conn.execute(
                    "
                    INSERT OR IGNORE INTO settings_revision_tags(revision_id, tag)
                    VALUES (?1, ?2)
                    ",
                    params![revision_id, tag],
                )
                .map_err(|err| SettingsAdminError {
                    code: "db_migration_failed".to_string(),
                    message: format!("failed to insert settings revision tag: {err}"),
                })?;
            }
        }

        conn.execute(
            "
            INSERT INTO settings_revision_migration_meta(key, value)
            VALUES ('warning_count', ?1)
            ON CONFLICT(key) DO UPDATE SET value = excluded.value
            ",
            params![warning_count],
        )
        .map_err(|err| SettingsAdminError {
            code: "db_migration_failed".to_string(),
            message: format!("failed to persist revision migration warnings: {err}"),
        })?;

        conn.execute(
            "
            INSERT INTO settings_revision_migration_meta(key, value)
            VALUES ('backfill_completed', 1)
            ON CONFLICT(key) DO UPDATE SET value = excluded.value
            ",
            [],
        )
        .map_err(|err| SettingsAdminError {
            code: "db_migration_failed".to_string(),
            message: format!("failed to persist revision migration completion: {err}"),
        })?;

        Ok(())
    }

    fn append_failure_history_row(
        &self,
        conn: &Connection,
        saved_at_ms: i64,
        backup_id: Option<&str>,
        tags_json: &str,
        proxy_settings_json: &str,
        claude_settings_json: &str,
        content_hash: &str,
        settings_json_text: &str,
        error_message: &str,
    ) -> Result<(), SettingsAdminError> {
        let legacy_id = uuid::Uuid::new_v4().to_string();

        conn.execute(
            "
            INSERT INTO settings_history (
                id,
                saved_at_ms,
                outcome,
                error_message,
                backup_id,
                tags_json,
                proxy_settings_json,
                claude_settings_json,
                content_hash,
                settings_json,
                source
            ) VALUES (?1, ?2, 'failure', ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'settings_admin')
            ",
            params![
                &legacy_id,
                saved_at_ms,
                error_message,
                backup_id,
                tags_json,
                proxy_settings_json,
                claude_settings_json,
                content_hash,
                settings_json_text,
            ],
        )
        .map_err(|err| SettingsAdminError {
            code: "db_write_failed".to_string(),
            message: format!("failed to append failure history row: {err}"),
        })?;

        conn.execute(
            "
            INSERT INTO settings_revision (
                legacy_id,
                created_at_ms,
                outcome,
                error_message,
                settings_json
            ) VALUES (?1, ?2, 'failure', ?3, ?4)
            ",
            params![&legacy_id, saved_at_ms, error_message, settings_json_text],
        )
        .map_err(|err| SettingsAdminError {
            code: "db_write_failed".to_string(),
            message: format!("failed to append failure revision row: {err}"),
        })?;

        let revision_id: i64 = conn
            .query_row(
                "SELECT id FROM settings_revision WHERE legacy_id = ?1",
                params![&legacy_id],
                |row| row.get(0),
            )
            .map_err(|err| SettingsAdminError {
                code: "db_write_failed".to_string(),
                message: format!("failed to load failure revision id: {err}"),
            })?;

        let mut deduped_tags = std::collections::BTreeSet::new();
        for tag in parse_tags_json(tags_json) {
            if let Some(normalized) = canonicalize_tag(&tag) {
                deduped_tags.insert(normalized);
            }
        }

        for tag in deduped_tags {
            conn.execute(
                "
                INSERT OR IGNORE INTO settings_revision_tags(revision_id, tag)
                VALUES (?1, ?2)
                ",
                params![revision_id, tag],
            )
            .map_err(|err| SettingsAdminError {
                code: "db_write_failed".to_string(),
                message: format!("failed to append failure revision tag: {err}"),
            })?;
        }

        Ok(())
    }
}

fn split_apply_payload(payload: Value) -> Result<(Value, Vec<String>), SettingsAdminError> {
    let Value::Object(mut root) = payload else {
        return Err(SettingsAdminError {
            code: "invalid_payload".to_string(),
            message: "settings payload must be a JSON object".to_string(),
        });
    };

    if let Some(settings_value) = root.remove("settings") {
        let quick_tags = match root.remove("quick_tags") {
            None => Vec::new(),
            Some(Value::Array(items)) => {
                let mut tags = Vec::with_capacity(items.len());
                for item in items {
                    let Some(tag) = item.as_str() else {
                        return Err(SettingsAdminError {
                            code: "invalid_payload".to_string(),
                            message: "quick_tags must be an array of strings".to_string(),
                        });
                    };
                    tags.push(tag.to_string());
                }
                tags
            }
            Some(_) => {
                return Err(SettingsAdminError {
                    code: "invalid_payload".to_string(),
                    message: "quick_tags must be an array of strings".to_string(),
                })
            }
        };

        if !root.is_empty() {
            return Err(SettingsAdminError {
                code: "invalid_payload".to_string(),
                message: "settings wrapper cannot be combined with additional root fields"
                    .to_string(),
            });
        }

        let Value::Object(settings) = settings_value else {
            return Err(SettingsAdminError {
                code: "invalid_payload".to_string(),
                message: "settings must be a JSON object when provided".to_string(),
            });
        };

        return Ok((Value::Object(settings), quick_tags));
    }

    Ok((Value::Object(root), Vec::new()))
}


fn stable_content_hash(value: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn parse_tags_json(tags_json: &str) -> Vec<String> {
    let parsed = match serde_json::from_str::<Value>(tags_json) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };

    let Some(items) = parsed.as_array() else {
        return Vec::new();
    };

    items
        .iter()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}


fn normalize_search_text(value: impl AsRef<str>) -> String {
    value
        .as_ref()
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn canonicalize_tag(value: &str) -> Option<String> {
    let mut normalized = String::with_capacity(value.len());
    let mut previous_was_separator = false;

    for ch in value.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            normalized.push(lower);
            previous_was_separator = false;
            continue;
        }

        if matches!(lower, '-' | '_' | ':' | '.') {
            if !previous_was_separator && !normalized.is_empty() {
                normalized.push('-');
                previous_was_separator = true;
            }
        }
    }

    while normalized.ends_with('-') {
        normalized.pop();
    }

    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn add_tag(tags: &mut std::collections::BTreeSet<String>, value: impl AsRef<str>) {
    if let Some(tag) = canonicalize_tag(value.as_ref()) {
        tags.insert(tag);
    }
}

fn derive_settings_tags(settings: &Value) -> Vec<String> {
    let mut tags = std::collections::BTreeSet::<String>::new();

    add_tag(&mut tags, "all-settings");

    if let Some(root) = settings.as_object() {
        for key in root.keys() {
            add_tag(&mut tags, format!("has-{key}"));
        }

        if root.get("env").and_then(Value::as_object).is_some() {
            add_tag(&mut tags, "has-env");
        }

        if let Some(hooks) = root.get("hooks").and_then(Value::as_object) {
            add_tag(&mut tags, "has-hooks");
            for key in hooks.keys() {
                add_tag(&mut tags, format!("hook-{key}"));
            }
        }

        if let Some(plugins) = root.get("enabledPlugins").and_then(Value::as_object) {
            add_tag(&mut tags, "has-enabled-plugins");
            for key in plugins.keys() {
                add_tag(&mut tags, format!("plugin-{key}"));
            }
        }

        if root.get("permissions").is_some() {
            add_tag(&mut tags, "has-permissions");
        }

        for model_key in [
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            "ANTHROPIC_SUBAGENT_MODEL",
        ] {
            let maybe_model = root
                .get("env")
                .and_then(Value::as_object)
                .and_then(|env| env.get(model_key))
                .and_then(Value::as_str)
                .or_else(|| root.get(model_key).and_then(Value::as_str));

            if let Some(model_name) = maybe_model {
                add_tag(&mut tags, format!("model-{model_name}"));
            }
        }
    }

    tags.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::StatsStore;
    use serde_json::{json, Value};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    static SETTINGS_HISTORY_SOURCE_TEST_MUTEX: Mutex<()> = Mutex::new(());

    fn with_settings_history_source<T>(value: Option<&str>, action: impl FnOnce() -> T) -> T {
        let _guard = SETTINGS_HISTORY_SOURCE_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var("SETTINGS_HISTORY_SOURCE").ok();

        match value {
            Some(source) => unsafe { std::env::set_var("SETTINGS_HISTORY_SOURCE", source) },
            None => unsafe { std::env::remove_var("SETTINGS_HISTORY_SOURCE") },
        }

        let result = action();

        match previous {
            Some(source) => unsafe { std::env::set_var("SETTINGS_HISTORY_SOURCE", source) },
            None => unsafe { std::env::remove_var("SETTINGS_HISTORY_SOURCE") },
        }

        result
    }
    fn make_test_paths(name: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "claude-proxy-settings-admin-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        let storage_dir = root.join("storage");
        let claude_dir = root.join("claude");
        fs::create_dir_all(&storage_dir).unwrap();
        fs::create_dir_all(&claude_dir).unwrap();
        (storage_dir, claude_dir)
    }

    fn make_admin(storage_dir: PathBuf, claude_dir: PathBuf) -> SettingsAdmin {
        let store = Arc::new(StatsStore::new(
            100,
            storage_dir,
            20.0,
            8.0,
            2_097_152,
            claude_dir,
        ));
        SettingsAdmin::new(store)
    }

    fn read_json_file(path: &Path) -> Value {
        let text = fs::read_to_string(path).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn apply_settings_writes_settings_json_directly() {
        let (storage_dir, claude_dir) = make_test_paths("writes-json");
        let admin = make_admin(storage_dir, claude_dir.clone());

        let payload = json!({"theme": "dark", "debug": true});
        admin.apply_settings(payload.clone()).unwrap();

        let settings_path = claude_dir.join("settings.json");
        assert!(settings_path.exists());
        assert_eq!(read_json_file(&settings_path), payload);
    }

    #[test]
    fn apply_settings_creates_timestamped_backup_before_write() {
        let (storage_dir, claude_dir) = make_test_paths("backup-before-write");
        let settings_path = claude_dir.join("settings.json");
        fs::write(&settings_path, "{\"existing\":true}").unwrap();

        let admin = make_admin(storage_dir, claude_dir.clone());
        admin.apply_settings(json!({"existing": false})).unwrap();

        let backups = admin.list_backups().unwrap();
        assert_eq!(backups.len(), 1);

        let backup_path = claude_dir.join("settings-backups").join(&backups[0].id);
        assert_eq!(read_json_file(&backup_path), json!({"existing": true}));
    }

    #[test]
    fn apply_settings_prunes_backups_to_max_20() {
        let (storage_dir, claude_dir) = make_test_paths("retention");
        let settings_path = claude_dir.join("settings.json");
        fs::write(&settings_path, "{\"seed\":0}").unwrap();

        let admin = make_admin(storage_dir, claude_dir);

        for i in 0..25 {
            admin.apply_settings(json!({"version": i})).unwrap();
        }

        let backups = admin.list_backups().unwrap();
        assert_eq!(backups.len(), BACKUP_RETENTION_MAX);
    }

    #[test]
    fn current_settings_snapshot_persists_and_is_readable_after_reinit() {
        let (storage_dir, claude_dir) = make_test_paths("persist-current");
        {
            let admin = make_admin(storage_dir.clone(), claude_dir.clone());
            admin
                .apply_settings(json!({"model": "claude-opus-4.6", "max_tokens": 256}))
                .unwrap();
        }

        let admin_reinit = make_admin(storage_dir, claude_dir);
        let current = admin_reinit.get_current().unwrap().expect("current exists");
        assert_eq!(
            current.claude_settings.raw_json,
            json!({"model": "claude-opus-4.6", "max_tokens": 256})
        );
    }

    #[test]
    fn apply_settings_creates_exactly_one_revision_row() {
        let (storage_dir, claude_dir) = make_test_paths("apply-revision-row");
        let admin = make_admin(storage_dir.clone(), claude_dir);

        admin
            .apply_settings(json!({"model": "claude-opus-4.6", "max_tokens": 256}))
            .unwrap();

        let conn = Connection::open(storage_dir.join("proxy.db")).unwrap();
        let revision_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM settings_revision", [], |row| row.get(0))
            .unwrap();

        assert_eq!(revision_count, 1);
    }

    #[test]
    fn apply_settings_normalizes_quick_tags_into_revision_tags() {
        let (storage_dir, claude_dir) = make_test_paths("apply-quick-tags");
        let admin = make_admin(storage_dir.clone(), claude_dir);

        admin
            .apply_settings(json!({
                "settings": {"model": "claude-opus-4.6"},
                "quick_tags": [" Alpha ", "BETA_TAG", "beta.tag", "", "###"]
            }))
            .unwrap();

        let conn = Connection::open(storage_dir.join("proxy.db")).unwrap();
        let tags: Vec<String> = conn
            .prepare(
                "
                SELECT tag
                FROM settings_revision_tags
                WHERE revision_id = (SELECT id FROM settings_revision ORDER BY id DESC LIMIT 1)
                ORDER BY tag ASC
                ",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();

        assert!(tags.contains(&"alpha".to_string()));
        assert!(tags.contains(&"beta-tag".to_string()));
    }

    #[test]
    fn apply_settings_rejects_quick_tags_with_non_string_items() {
        let (storage_dir, claude_dir) = make_test_paths("apply-quick-tags-invalid-items");
        let admin = make_admin(storage_dir, claude_dir);

        let err = admin
            .apply_settings(json!({
                "settings": {"theme": "dark"},
                "quick_tags": ["valid", 42, "also-valid"]
            }))
            .expect_err("non-string quick_tags entries must be rejected");

        assert_eq!(err.code, "invalid_payload");
        assert!(err.message.contains("quick_tags must be an array of strings"));
    }

    #[test]
    fn apply_settings_unwrapped_payload_preserves_quick_tags_setting_key() {
        let (storage_dir, claude_dir) = make_test_paths("apply-unwrapped-quick-tags-preserved");
        let admin = make_admin(storage_dir, claude_dir);

        let payload = json!({
            "theme": "dark",
            "quick_tags": ["user-setting", 123],
            "debug": true
        });

        admin.apply_settings(payload.clone()).unwrap();

        let current = admin.get_current().unwrap().expect("current exists");
        assert_eq!(current.claude_settings.raw_json, payload);
    }

    #[test]
    fn apply_settings_deduplicates_duplicate_quick_tags_idempotently() {
        let (storage_dir, claude_dir) = make_test_paths("apply-quick-tag-dupes");
        let admin = make_admin(storage_dir.clone(), claude_dir);

        admin
            .apply_settings(json!({
                "settings": {"theme": "dark"},
                "quick_tags": ["alpha", "ALPHA", "alpha", "alpha__", "alpha---"]
            }))
            .unwrap();

        let conn = Connection::open(storage_dir.join("proxy.db")).unwrap();
        let alpha_count: i64 = conn
            .query_row(
                "
                SELECT COUNT(*)
                FROM settings_revision_tags
                WHERE revision_id = (SELECT id FROM settings_revision ORDER BY id DESC LIMIT 1)
                  AND tag = 'alpha'
                ",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(alpha_count, 1);
    }

    #[test]
    fn apply_settings_with_quick_tags_keeps_current_snapshot_behavior() {
        let (storage_dir, claude_dir) = make_test_paths("apply-quick-tags-current");
        let admin = make_admin(storage_dir, claude_dir);

        admin
            .apply_settings(json!({
                "settings": {"model": "claude-opus-4.6", "max_tokens": 333},
                "quick_tags": ["release"]
            }))
            .unwrap();

        let current = admin.get_current().unwrap().expect("current exists");
        assert_eq!(
            current.claude_settings.raw_json,
            json!({"model": "claude-opus-4.6", "max_tokens": 333})
        );
    }

    #[test]
    fn load_current_from_settings_file_avoids_precheck_exists_race() {
        let source = include_str!("settings_admin.rs");
        let start = source
            .find("fn load_current_from_settings_file")
            .expect("function should exist");
        let tail = &source[start..];
        let end = tail
            .find("fn backup_dir")
            .expect("next function boundary should exist");
        let body = &tail[..end];

        assert!(body.contains("match fs::read_to_string(&settings_path)"));
        assert!(body.contains("err.kind() == std::io::ErrorKind::NotFound"));
        assert!(!body.contains("if !settings_path.exists()"));
    }

    #[test]
    fn get_current_reads_settings_file_when_db_snapshot_missing() {
        let (storage_dir, claude_dir) = make_test_paths("current-fallback-file");
        let settings_path = claude_dir.join("settings.json");
        fs::write(
            &settings_path,
            serde_json::to_string_pretty(&json!({"theme": "dark", "max_tokens": 512})).unwrap(),
        )
        .unwrap();

        let admin = make_admin(storage_dir, claude_dir);
        let current = admin
            .get_current()
            .unwrap()
            .expect("settings should be loaded from settings.json when db snapshot is absent");

        assert_eq!(
            current.claude_settings.raw_json,
            json!({"theme": "dark", "max_tokens": 512})
        );
        assert_eq!(current.proxy_settings.raw_json, current.claude_settings.raw_json);
        assert!(current.updated_at_ms > 0);
    }

    #[test]
    fn history_is_append_only_across_multiple_applies() {
        let (storage_dir, claude_dir) = make_test_paths("append-history");
        let admin = make_admin(storage_dir, claude_dir);

        admin.apply_settings(json!({"value": 1})).unwrap();
        admin.apply_settings(json!({"value": 2})).unwrap();

        let history = admin.get_history(10, 0, None).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].outcome, "success");
        assert_eq!(history[1].outcome, "success");
    }

    #[test]
    fn failure_path_preserves_backup_and_appends_failure_history_row() {
        let (storage_dir, claude_dir) = make_test_paths("failure-history");
        let settings_path = claude_dir.join("settings.json");
        fs::write(&settings_path, "{\"initial\":true}").unwrap();

        let admin = make_admin(storage_dir, claude_dir.clone());
        admin.trigger_db_failure_once_for_test();
        let err = admin
            .apply_settings(json!({"initial": false}))
            .expect_err("simulated db failure");

        assert_eq!(err.code, "db_write_failed");

        let backups = admin.list_backups().unwrap();
        assert_eq!(backups.len(), 1);

        let history = admin.get_history(10, 0, None).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].outcome, "failure");
    }

    #[test]
    fn bulk_delete_selected_backups() {
        let (storage_dir, claude_dir) = make_test_paths("delete-selected");
        let settings_path = claude_dir.join("settings.json");
        fs::write(&settings_path, "{\"seed\":0}").unwrap();

        let admin = make_admin(storage_dir, claude_dir);

        for i in 0..3 {
            admin.apply_settings(json!({"n": i})).unwrap();
        }

        let backups = admin.list_backups().unwrap();
        assert!(backups.len() >= 3);

        let selected: Vec<String> = backups.iter().take(2).map(|b| b.id.clone()).collect();
        let deleted = admin.delete_backups_selected(&selected).unwrap();
        assert_eq!(deleted, 2);

        let remaining = admin.list_backups().unwrap();
        assert_eq!(remaining.len(), backups.len() - 2);
    }

    #[test]
    fn bulk_delete_all_backups() {
        let (storage_dir, claude_dir) = make_test_paths("delete-all");
        let settings_path = claude_dir.join("settings.json");
        fs::write(&settings_path, "{\"seed\":0}").unwrap();

        let admin = make_admin(storage_dir, claude_dir);

        for i in 0..4 {
            admin.apply_settings(json!({"n": i})).unwrap();
        }

        let deleted = admin.delete_backups_all().unwrap();
        assert!(deleted >= 1);
        assert!(admin.list_backups().unwrap().is_empty());
    }

    #[test]
    fn schema_initialization_creates_revision_tables() {
        let (storage_dir, claude_dir) = make_test_paths("revision-schema");
        let admin = make_admin(storage_dir.clone(), claude_dir);

        admin.get_current().unwrap();

        let conn = Connection::open(storage_dir.join("proxy.db")).unwrap();
        let revision_exists: i64 = conn
            .query_row(
                "
                SELECT COUNT(*)
                FROM sqlite_master
                WHERE type = 'table' AND name = 'settings_revision'
                ",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let revision_tags_exists: i64 = conn
            .query_row(
                "
                SELECT COUNT(*)
                FROM sqlite_master
                WHERE type = 'table' AND name = 'settings_revision_tags'
                ",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(revision_exists, 1);
        assert_eq!(revision_tags_exists, 1);
    }

    fn create_legacy_settings_history_table(conn: &Connection) {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS settings_history (
                id TEXT PRIMARY KEY,
                saved_at_ms INTEGER NOT NULL,
                outcome TEXT NOT NULL,
                error_message TEXT,
                tags_json TEXT,
                claude_settings_json TEXT,
                settings_json TEXT
            );
            ",
        )
        .unwrap();
    }

    fn insert_legacy_history_row(
        conn: &Connection,
        id: &str,
        saved_at_ms: i64,
        outcome: &str,
        error_message: Option<&str>,
        tags_json: &str,
        claude_settings_json: &str,
        settings_json: &str,
    ) {
        conn.execute(
            "
            INSERT INTO settings_history (
                id,
                saved_at_ms,
                outcome,
                error_message,
                tags_json,
                claude_settings_json,
                settings_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ",
            params![
                id,
                saved_at_ms,
                outcome,
                error_message,
                tags_json,
                claude_settings_json,
                settings_json
            ],
        )
        .unwrap();
    }

    #[test]
    fn legacy_history_backfills_into_revision_tables_preserving_order_and_field_mapping() {
        let (storage_dir, claude_dir) = make_test_paths("revision-backfill-order");
        let db_path = storage_dir.join("proxy.db");
        let conn = Connection::open(&db_path).unwrap();
        create_legacy_settings_history_table(&conn);

        insert_legacy_history_row(
            &conn,
            "a-100",
            1_000,
            "success",
            None,
            "[\"alpha\",\"beta\"]",
            "{\"model\":\"opus\"}",
            "{\"model\":\"opus\"}",
        );
        insert_legacy_history_row(
            &conn,
            "b-100",
            1_000,
            "failure",
            Some("write failed"),
            "[\"gamma\"]",
            "{\"model\":\"sonnet\"}",
            "{\"model\":\"sonnet\"}",
        );

        drop(conn);

        let admin = make_admin(storage_dir.clone(), claude_dir);
        admin.get_current().unwrap();

        let conn = Connection::open(db_path).unwrap();
        let rows: Vec<(String, i64, String, Option<String>, String)> = conn
            .prepare(
                "
                SELECT legacy_id, created_at_ms, outcome, error_message, settings_json
                FROM settings_revision
                ORDER BY created_at_ms DESC, legacy_id DESC
                ",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "b-100");
        assert_eq!(rows[0].1, 1_000);
        assert_eq!(rows[0].2, "failure");
        assert_eq!(rows[0].3.as_deref(), Some("write failed"));
        assert_eq!(
            serde_json::from_str::<Value>(&rows[0].4).unwrap(),
            json!({"model":"sonnet"})
        );

        assert_eq!(rows[1].0, "a-100");
        assert_eq!(rows[1].1, 1_000);
        assert_eq!(rows[1].2, "success");
        assert_eq!(rows[1].3, None);
        assert_eq!(
            serde_json::from_str::<Value>(&rows[1].4).unwrap(),
            json!({"model":"opus"})
        );

        let migrated_warnings: i64 = conn
            .query_row(
                "SELECT value FROM settings_revision_migration_meta WHERE key = 'warning_count'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(migrated_warnings, 0);
    }

    #[test]
    fn parse_tags_json_salvages_strings_from_mixed_type_array() {
        let parsed = parse_tags_json("[\"alpha\", 42, null, \"beta\", {\"k\":1}]");
        assert_eq!(parsed, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn legacy_backfill_prefers_claude_settings_json_when_payload_columns_conflict() {
        let (storage_dir, claude_dir) = make_test_paths("revision-backfill-payload-precedence");
        let db_path = storage_dir.join("proxy.db");
        let conn = Connection::open(&db_path).unwrap();
        create_legacy_settings_history_table(&conn);

        insert_legacy_history_row(
            &conn,
            "conflict-row",
            3_000,
            "success",
            None,
            "[]",
            "{\"source\":\"claude_settings_json\",\"flag\":true}",
            "{\"source\":\"settings_json\",\"flag\":false}",
        );

        drop(conn);

        let admin = make_admin(storage_dir.clone(), claude_dir);
        admin.get_current().unwrap();

        let conn = Connection::open(db_path).unwrap();
        let migrated_settings_json: String = conn
            .query_row(
                "SELECT settings_json FROM settings_revision WHERE legacy_id = 'conflict-row'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(
            serde_json::from_str::<Value>(&migrated_settings_json).unwrap(),
            json!({"source":"claude_settings_json","flag":true})
        );
    }

    #[test]
    fn history_non_search_path_keeps_sql_limit_offset_and_avoids_in_memory_skip_take() {
        let source = include_str!("settings_admin.rs");
        let start = source
            .find("pub fn get_history(")
            .expect("get_history function should exist");
        let tail = &source[start..];
        let end = tail
            .find("pub fn patch_history_tags(")
            .expect("next function boundary should exist");
        let body = &tail[..end];

        assert!(
            body.contains("if normalized_search.is_none()"),
            "get_history should branch for non-search path"
        );
        assert!(
            body.contains("LIMIT ?1 OFFSET ?2"),
            "non-search history path should use SQL LIMIT/OFFSET"
        );

        let non_search_branch_start = body
            .find("if normalized_search.is_none()")
            .expect("non-search branch should exist");
        let non_search_branch = &body[non_search_branch_start..];
        let return_ok_history = non_search_branch
            .find("return Ok(history);")
            .expect("non-search branch should return early with SQL-paginated rows");
        let early_branch = &non_search_branch[..return_ok_history];

        assert!(
            !early_branch.contains(".skip(offset).take(limit)"),
            "non-search branch should not apply in-memory skip/take pagination"
        );
    }

    #[test]
    fn history_search_is_case_insensitive_whitespace_normalized_and_tag_or_text_or() {
        let (storage_dir, claude_dir) = make_test_paths("history-search-task3");
        let admin = make_admin(storage_dir, claude_dir);

        admin
            .apply_settings(json!({
                "settings": {"note": "Alpha    Beta"},
                "quick_tags": ["release-candidate"]
            }))
            .unwrap();
        admin
            .apply_settings(json!({
                "settings": {"note": "other payload"},
                "quick_tags": ["staging"]
            }))
            .unwrap();

        let by_tag = admin.get_history(50, 0, Some("   RELEASE-candidate   ")).unwrap();
        assert_eq!(by_tag.len(), 1);

        let by_text = admin.get_history(50, 0, Some(" alpha   beta ")).unwrap();
        assert_eq!(by_text.len(), 1);

        let by_missing = admin.get_history(50, 0, Some("definitely-missing")).unwrap();
        assert!(by_missing.is_empty());
    }

    #[test]
    fn history_tags_patch_supports_add_remove_and_remove_missing_is_noop() {
        with_settings_history_source(None, || {
            let (storage_dir, claude_dir) = make_test_paths("history-tags-patch-task3");
            let admin = make_admin(storage_dir.clone(), claude_dir);

            admin
                .apply_settings(json!({
                    "settings": {"theme": "dark"},
                    "quick_tags": ["alpha"]
                }))
                .unwrap();

            let conn = Connection::open(storage_dir.join("proxy.db")).unwrap();
            let revision_id: i64 = conn
                .query_row("SELECT id FROM settings_revision ORDER BY id DESC LIMIT 1", [], |row| {
                    row.get(0)
                })
                .unwrap();
            drop(conn);

            let updated = admin
                .patch_history_tags(revision_id, &[
                    "Beta".to_string()
                ], &[
                    "alpha".to_string(),
                    "missing".to_string()
                ])
                .unwrap();

            assert!(updated.tags.contains(&"beta".to_string()));
            assert!(!updated.tags.contains(&"alpha".to_string()));
        });
    }

    #[test]
    fn history_tags_patch_rejects_empty_tag_values() {
        let (storage_dir, claude_dir) = make_test_paths("history-tags-empty-task3");
        let admin = make_admin(storage_dir.clone(), claude_dir);

        admin.apply_settings(json!({"theme": "dark"})).unwrap();
        let conn = Connection::open(storage_dir.join("proxy.db")).unwrap();
        let revision_id: i64 = conn
            .query_row("SELECT id FROM settings_revision ORDER BY id DESC LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();

        let err = admin
            .patch_history_tags(revision_id, &["   ".to_string()], &[])
            .expect_err("empty add tag should fail");
        assert_eq!(err.code, "invalid_payload");
    }

    #[test]
    fn history_tags_patch_missing_revision_returns_not_found() {
        let (storage_dir, claude_dir) = make_test_paths("history-tags-missing-task3");
        let admin = make_admin(storage_dir, claude_dir);

        let err = admin
            .patch_history_tags(9_999_999, &["alpha".to_string()], &[])
            .expect_err("missing revision id should fail");
        assert_eq!(err.code, "not_found");
    }

    #[test]
    fn history_source_default_is_revision_and_legacy_table_untouched_during_apply_cutover() {
        with_settings_history_source(None, || {
            let (storage_dir, claude_dir) = make_test_paths("history-source-default-revision");
            let db_path = storage_dir.join("proxy.db");
            let conn = Connection::open(&db_path).unwrap();
            create_legacy_settings_history_table(&conn);
            insert_legacy_history_row(
                &conn,
                "legacy-only-row",
                10,
                "success",
                None,
                "[]",
                "{\"legacy\":true}",
                "{\"legacy\":true}",
            );
            drop(conn);

            let admin = make_admin(storage_dir.clone(), claude_dir);
            admin
                .apply_settings(json!({
                    "settings": {"from_revision": true},
                    "quick_tags": ["cutover"]
                }))
                .unwrap();

            let conn = Connection::open(db_path).unwrap();
            let legacy_row_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM settings_history WHERE id = 'legacy-only-row'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(legacy_row_count, 1);

            let revision_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM settings_revision", [], |row| row.get(0))
                .unwrap();
            assert!(revision_count >= 2);

            let revision_source_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM settings_history WHERE source = 'settings_admin'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(revision_source_count >= 1);
        });
    }

    #[test]
    fn history_source_toggle_legacy_serves_legacy_rows_without_cutover_mutation() {
        let (storage_dir, claude_dir) = make_test_paths("history-source-toggle-legacy");
        let db_path = storage_dir.join("proxy.db");
        let conn = Connection::open(&db_path).unwrap();
        create_legacy_settings_history_table(&conn);

        insert_legacy_history_row(
            &conn,
            "legacy-a",
            100,
            "success",
            None,
            "[]",
            "{\"marker\":\"legacy-a\"}",
            "{\"marker\":\"legacy-a\"}",
        );
        insert_legacy_history_row(
            &conn,
            "legacy-b",
            200,
            "success",
            None,
            "[]",
            "{\"marker\":\"legacy-b\"}",
            "{\"marker\":\"legacy-b\"}",
        );
        drop(conn);

        with_settings_history_source(None, || {
            let admin = make_admin(storage_dir.clone(), claude_dir.clone());
            admin
                .apply_settings(json!({"settings": {"marker": "revision-candidate"}}))
                .unwrap();
        });

        let legacy_baseline_count: i64 = Connection::open(&db_path)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM settings_history", [], |row| row.get(0))
            .unwrap();

        with_settings_history_source(Some("legacy"), || {
            let admin = make_admin(storage_dir.clone(), claude_dir.clone());
            let legacy_history = admin.get_history(20, 0, None).unwrap();
            assert_eq!(legacy_history.len(), 2);
            let ids: Vec<String> = legacy_history.iter().map(|row| row.id.clone()).collect();
            assert!(ids.contains(&"legacy-a".to_string()));
            assert!(ids.contains(&"legacy-b".to_string()));

            admin
                .apply_settings(json!({"settings": {"marker": "legacy-write"}}))
                .unwrap();
        });

        let conn = Connection::open(db_path).unwrap();
        let legacy_after_toggle_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM settings_history", [], |row| row.get(0))
            .unwrap();
        assert_eq!(legacy_after_toggle_count, legacy_baseline_count + 1);

        let revision_rows_for_new_legacy_write: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM settings_revision WHERE legacy_id IN (SELECT id FROM settings_history WHERE source = 'settings_admin')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(revision_rows_for_new_legacy_write >= 1);
    }

    #[test]
    fn rollback_drill_revision_cutover_then_legacy_rollback_preserves_consistency_without_unexpected_mutation() {
        let (storage_dir, claude_dir) = make_test_paths("rollback-drill");
        let db_path = storage_dir.join("proxy.db");
        let conn = Connection::open(&db_path).unwrap();
        create_legacy_settings_history_table(&conn);

        insert_legacy_history_row(
            &conn,
            "legacy-seed-1",
            1_000,
            "success",
            None,
            "[]",
            "{\"phase\":\"legacy-baseline\",\"n\":1}",
            "{\"phase\":\"legacy-baseline\",\"n\":1}",
        );
        insert_legacy_history_row(
            &conn,
            "legacy-seed-2",
            2_000,
            "success",
            None,
            "[]",
            "{\"phase\":\"legacy-baseline\",\"n\":2}",
            "{\"phase\":\"legacy-baseline\",\"n\":2}",
        );
        drop(conn);

        with_settings_history_source(None, || {
            let admin = make_admin(storage_dir.clone(), claude_dir.clone());
            admin
                .apply_settings(json!({"settings": {"phase": "revision-cutover", "n": 3}}))
                .unwrap();

            let revision_history = admin.get_history(20, 0, None).unwrap();
            assert!(
                revision_history
                    .iter()
                    .any(|row| row.claude_settings.raw_json.get("phase") == Some(&json!("revision-cutover")))
            );
        });

        let count_after_cutover: i64 = Connection::open(&db_path)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM settings_history", [], |row| row.get(0))
            .unwrap();

        with_settings_history_source(Some("legacy"), || {
            let admin = make_admin(storage_dir.clone(), claude_dir.clone());
            let legacy_before = admin.get_history(20, 0, None).unwrap();
            let legacy_ids_before: Vec<String> = legacy_before.iter().map(|row| row.id.clone()).collect();
            assert!(legacy_ids_before.contains(&"legacy-seed-1".to_string()));
            assert!(legacy_ids_before.contains(&"legacy-seed-2".to_string()));

            admin
                .apply_settings(json!({"settings": {"phase": "legacy-rollback", "n": 4}}))
                .unwrap();

            let legacy_after = admin.get_history(20, 0, None).unwrap();
            assert!(legacy_after.len() >= legacy_before.len() + 1);
            assert!(
                legacy_after
                    .iter()
                    .any(|row| row.claude_settings.raw_json.get("phase") == Some(&json!("legacy-rollback")))
            );
        });

        let count_after_rollback_apply: i64 = Connection::open(&db_path)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM settings_history", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count_after_rollback_apply, count_after_cutover + 1);

        with_settings_history_source(Some("legacy"), || {
            let admin = make_admin(storage_dir.clone(), claude_dir.clone());
            let legacy_read = admin.get_history(20, 0, None).unwrap();
            assert!(
                legacy_read
                    .iter()
                    .any(|row| row.claude_settings.raw_json.get("phase") == Some(&json!("legacy-rollback")))
            );
        });
    }
}
