use crate::stats::{SessionDeleteDbRowsByTable, StatsStore};
use std::path::{Path, PathBuf};

const MAX_FOLDER_CONFIDENCE_SCAN_DEPTH: usize = 8;
const MAX_FOLDER_CONFIDENCE_FILES_SCANNED: usize = 2_000;
const MAX_FOLDER_CONFIDENCE_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionDeleteResult {
    pub session_id: String,
    pub blocked_live: bool,
    pub deleted_db_rows_by_table: SessionDeleteDbRowsByTable,
    pub deleted_folders: Vec<String>,
    pub skipped_folders: Vec<String>,
    pub not_found: bool,
}

pub fn delete_session(store: &StatsStore, session_id: &str) -> Result<SessionDeleteResult, String> {
    let trimmed = session_id.trim();
    if trimmed.is_empty() {
        return Err("session_id is required".to_string());
    }

    let sessions = store.get_claude_sessions();
    let aggregated_session = sessions
        .iter()
        .find(|session| session.session_id == trimmed);
    let aggregated_present = aggregated_session.is_some();
    if aggregated_session.is_some_and(|session| session.is_live) {
        return Ok(SessionDeleteResult {
            session_id: trimmed.to_string(),
            blocked_live: true,
            deleted_db_rows_by_table: SessionDeleteDbRowsByTable::default(),
            deleted_folders: Vec::new(),
            skipped_folders: Vec::new(),
            not_found: false,
        });
    }

    let mut skipped_folders = Vec::new();
    let folder_candidate = resolve_confident_folder_candidate(
        store,
        aggregated_session,
        trimmed,
        &mut skipped_folders,
    );
    let confidently_mapped_folder_exists = folder_candidate
        .as_ref()
        .is_some_and(|candidate| candidate.exists() && candidate.is_dir());

    let db_outcome = store.delete_session_db_rows_with_live_guard(trimmed)?;
    if db_outcome.blocked_live {
        return Ok(SessionDeleteResult {
            session_id: trimmed.to_string(),
            blocked_live: true,
            deleted_db_rows_by_table: db_outcome.deleted_db_rows_by_table,
            deleted_folders: Vec::new(),
            skipped_folders,
            not_found: false,
        });
    }

    let mut deleted_folders = Vec::new();
    if let Some(candidate) = folder_candidate {
        if candidate.exists() && candidate.is_dir() {
            match std::fs::remove_dir_all(&candidate) {
                Ok(_) => deleted_folders.push(candidate.to_string_lossy().to_string()),
                Err(err) => skipped_folders.push(format!(
                    "{} (delete_failed: {err})",
                    candidate.to_string_lossy()
                )),
            }
        }
    }

    let counts = &db_outcome.deleted_db_rows_by_table;
    let db_any_rows = counts.request_bodies > 0
        || counts.request_correlations > 0
        || counts.explanations > 0
        || counts.requests > 0
        || counts.local_events > 0;

    let not_found = !aggregated_present && !db_any_rows && !confidently_mapped_folder_exists;

    Ok(SessionDeleteResult {
        session_id: trimmed.to_string(),
        blocked_live: false,
        deleted_db_rows_by_table: db_outcome.deleted_db_rows_by_table,
        deleted_folders,
        skipped_folders,
        not_found,
    })
}

fn resolve_confident_folder_candidate(
    store: &StatsStore,
    aggregated_session: Option<&crate::stats::ClaudeSession>,
    session_id: &str,
    skipped_folders: &mut Vec<String>,
) -> Option<PathBuf> {
    if let Some(candidate) = resolve_confident_folder_candidate_from_aggregated_session(
        store,
        aggregated_session,
        session_id,
        skipped_folders,
    ) {
        return Some(candidate);
    }

    resolve_confident_folder_candidate_from_projects_scan(store, session_id, skipped_folders)
}

fn resolve_confident_folder_candidate_from_aggregated_session(
    store: &StatsStore,
    aggregated_session: Option<&crate::stats::ClaudeSession>,
    session_id: &str,
    skipped_folders: &mut Vec<String>,
) -> Option<PathBuf> {
    let Some(session) = aggregated_session else {
        return None;
    };

    if session.project_paths.len() != 1 {
        if session.project_paths.len() > 1 {
            skipped_folders.push(format!(
                "ambiguous mapping for session {}: {}",
                session_id,
                session.project_paths.join(",")
            ));
        }
        return None;
    }

    resolve_project_root_candidate(store, &session.project_paths[0], skipped_folders)
}

fn resolve_confident_folder_candidate_from_projects_scan(
    store: &StatsStore,
    session_id: &str,
    skipped_folders: &mut Vec<String>,
) -> Option<PathBuf> {
    let projects_root = store.claude_dir().join("projects");
    let Ok(projects_root_canonical) = std::fs::canonicalize(&projects_root) else {
        skipped_folders.push(format!(
            "{} (projects_root_unavailable)",
            projects_root.to_string_lossy()
        ));
        return None;
    };

    let mut matched_projects = Vec::new();

    let Ok(entries) = std::fs::read_dir(&projects_root) else {
        return None;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let Some(project_name) = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
        else {
            continue;
        };

        let Some(project_root) =
            resolve_project_root_candidate(store, &project_name, skipped_folders)
        else {
            continue;
        };

        if !canonical_path_has_prefix(&project_root, &projects_root_canonical) {
            skipped_folders.push(format!(
                "{} (outside_allowed_root)",
                project_root.to_string_lossy()
            ));
            continue;
        }

        if project_contains_session_id(&project_root, session_id) {
            matched_projects.push(project_root);
        }
    }

    if matched_projects.len() > 1 {
        skipped_folders.push(format!(
            "ambiguous mapping for session {}: {}",
            session_id,
            matched_projects
                .iter()
                .map(|path| path.to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join(",")
        ));
        return None;
    }

    matched_projects.into_iter().next()
}

fn resolve_project_root_candidate(
    store: &StatsStore,
    project_name: &str,
    skipped_folders: &mut Vec<String>,
) -> Option<PathBuf> {
    let projects_root = store.claude_dir().join("projects");
    let Ok(projects_root_canonical) = std::fs::canonicalize(&projects_root) else {
        skipped_folders.push(format!(
            "{} (projects_root_unavailable)",
            projects_root.to_string_lossy()
        ));
        return None;
    };

    let candidate = projects_root.join(project_name);
    let Ok(candidate_canonical) = std::fs::canonicalize(&candidate) else {
        skipped_folders.push(format!(
            "{} (missing_or_uncanonicalizable)",
            candidate.to_string_lossy()
        ));
        return None;
    };

    if !canonical_path_has_prefix(&candidate_canonical, &projects_root_canonical) {
        skipped_folders.push(format!(
            "{} (outside_allowed_root)",
            candidate.to_string_lossy()
        ));
        return None;
    }

    Some(candidate_canonical)
}

fn project_contains_session_id(project_root: &Path, session_id: &str) -> bool {
    let mut files_scanned = 0usize;
    let mut stack = vec![(project_root.to_path_buf(), 0usize)];

    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_FOLDER_CONFIDENCE_SCAN_DEPTH {
            continue;
        }

        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();

            if path.is_dir() {
                if depth < MAX_FOLDER_CONFIDENCE_SCAN_DEPTH {
                    stack.push((path, depth + 1));
                }
                continue;
            }

            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }

            files_scanned += 1;
            if files_scanned > MAX_FOLDER_CONFIDENCE_FILES_SCANNED {
                return false;
            }

            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            if metadata.len() > MAX_FOLDER_CONFIDENCE_FILE_BYTES {
                continue;
            }

            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };

            let matched = json
                .get("sessionId")
                .or_else(|| json.get("session_id"))
                .or_else(|| json.get("session"))
                .and_then(|value| value.as_str())
                .is_some_and(|value| value == session_id);

            if matched {
                return true;
            }
        }
    }

    false
}

fn is_case_insensitive_fs() -> bool {
    cfg!(windows)
}

fn canonical_path_has_prefix(path: &Path, prefix: &Path) -> bool {
    let mut path_components = path.components();
    let mut prefix_components = prefix.components();

    loop {
        match prefix_components.next() {
            Some(prefix_component) => {
                let Some(path_component) = path_components.next() else {
                    return false;
                };

                let prefix_value = prefix_component.as_os_str().to_string_lossy();
                let path_value = path_component.as_os_str().to_string_lossy();

                let equal = if is_case_insensitive_fs() {
                    path_value.eq_ignore_ascii_case(&prefix_value)
                } else {
                    path_value == prefix_value
                };

                if !equal {
                    return false;
                }
            }
            None => return true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        correlation::PayloadPolicy,
        stats::{LocalEvent, RequestEntry, RequestStatus},
    };

    fn sample_entry() -> RequestEntry {
        RequestEntry {
            id: "session-admin-test-entry".into(),
            timestamp: chrono::Utc::now(),
            session_id: Some("session-admin".into()),
            method: "GET".into(),
            path: "/".into(),
            model: "claude-test".into(),
            stream: false,
            status: RequestStatus::Success(200),
            duration_ms: 100.0,
            ttft_ms: Some(25.0),
            input_tokens: Some(1),
            output_tokens: Some(2),
            cache_read_tokens: None,
            cache_creation_tokens: None,
            thinking_tokens: None,
            stop_reason: None,
            request_size_bytes: 10,
            response_size_bytes: 20,
            stalls: Vec::new(),
            error: None,
            anomalies: Vec::new(),
        }
    }

    fn set_file_modified_time_ms(path: &std::path::Path, modified_ms: i64) {
        #[cfg(windows)]
        {
            let escaped_path = path.to_string_lossy().replace('"', "`\"");
            let script = format!(
                "$ts = [DateTimeOffset]::FromUnixTimeMilliseconds({modified_ms}).UtcDateTime; (Get-Item -LiteralPath \"{escaped_path}\").LastWriteTimeUtc = $ts"
            );
            let status = std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", &script])
                .status()
                .expect("invoke powershell for mtime");
            assert!(status.success(), "failed to set file modified time");
        }

        #[cfg(not(windows))]
        {
            let seconds = modified_ms.div_euclid(1000).max(0);
            let status = std::process::Command::new("touch")
                .args(["-d", &format!("@{seconds}"), &path.to_string_lossy()])
                .status()
                .expect("invoke touch for mtime");
            assert!(status.success(), "failed to set file modified time");
        }
    }

    #[test]
    fn folder_confidence_scan_works_without_aggregated_session_presence() {
        let root = std::env::temp_dir().join(format!(
            "claude-proxy-session-admin-confidence-fallback-{}",
            uuid::Uuid::new_v4()
        ));
        let projects_root = root.join("projects");
        let project = projects_root.join("proj-fallback");
        std::fs::create_dir_all(&project).unwrap();

        std::fs::write(
            project.join("session.json"),
            serde_json::json!({ "session_id": "session-fallback-only" }).to_string(),
        )
        .unwrap();

        let store = StatsStore::new(20, root.clone(), 20.0, 8.0, 2_097_152, root.clone());

        let mut skipped_folders = Vec::new();
        let candidate = resolve_confident_folder_candidate(
            &store,
            None,
            "session-fallback-only",
            &mut skipped_folders,
        )
        .expect("expected confident fallback candidate");

        assert!(candidate.exists());
        assert!(
            candidate
                .to_string_lossy()
                .replace('\\', "/")
                .ends_with("/projects/proj-fallback"),
            "unexpected candidate: {}",
            candidate.to_string_lossy()
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn session_admin_delete_skips_ambiguous_folder_mapping() {
        let root = std::env::temp_dir().join(format!(
            "claude-proxy-session-admin-ambiguous-{}",
            uuid::Uuid::new_v4()
        ));
        let projects_root = root.join("projects");
        let proj_a = projects_root.join("proj-a");
        let proj_b = projects_root.join("proj-b");
        std::fs::create_dir_all(&proj_a).unwrap();
        std::fs::create_dir_all(&proj_b).unwrap();

        let stale_time = chrono::Utc::now() - chrono::Duration::minutes(10);

        let session_file_a = proj_a.join("session.json");
        std::fs::write(
            &session_file_a,
            serde_json::json!({ "session_id": "session-ambiguous" }).to_string(),
        )
        .unwrap();
        set_file_modified_time_ms(&session_file_a, stale_time.timestamp_millis());

        let session_file_b = proj_b.join("session.json");
        std::fs::write(
            &session_file_b,
            serde_json::json!({ "session_id": "session-ambiguous" }).to_string(),
        )
        .unwrap();
        set_file_modified_time_ms(&session_file_b, stale_time.timestamp_millis());

        let store = StatsStore::new(20, root.clone(), 20.0, 8.0, 2_097_152, root.clone());

        store.upsert_local_event(&LocalEvent {
            id: "evt-session-admin-ambiguous-stale".into(),
            source_kind: crate::stats::SourceKind::ClaudeProject,
            source_path: "projects/proj-a/session.json".into(),
            event_time_ms: stale_time.timestamp_millis(),
            session_hint: Some("session-ambiguous".into()),
            event_kind: "session_touch".into(),
            model_hint: None,
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({}),
        });

        let result =
            delete_session(&store, "session-ambiguous").expect("delete should return result");

        assert!(!result.blocked_live);
        assert!(result.deleted_folders.is_empty());
        assert!(result
            .skipped_folders
            .iter()
            .any(|reason| reason.contains("ambiguous mapping for session session-ambiguous")));
        assert!(!result.not_found);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn session_admin_delete_removes_folder_after_db_delete() {
        let root = std::env::temp_dir().join(format!(
            "claude-proxy-session-admin-delete-folder-{}",
            uuid::Uuid::new_v4()
        ));
        let projects_root = root.join("projects");
        let proj = projects_root.join("proj-delete");
        std::fs::create_dir_all(&proj).unwrap();

        let stale_time = chrono::Utc::now() - chrono::Duration::minutes(10);

        let session_file = proj.join("session.json");
        std::fs::write(
            &session_file,
            serde_json::json!({ "session_id": "session-delete" }).to_string(),
        )
        .unwrap();
        set_file_modified_time_ms(&session_file, stale_time.timestamp_millis());

        let store = StatsStore::new(20, root.clone(), 20.0, 8.0, 2_097_152, root.clone());

        let mut entry = sample_entry();
        entry.id = "req-session-admin-delete".into();
        entry.session_id = Some("session-delete".into());
        entry.timestamp = stale_time;
        store.add_entry(entry);

        store.upsert_local_event(&LocalEvent {
            id: "evt-session-admin-delete".into(),
            source_kind: crate::stats::SourceKind::ClaudeProject,
            source_path: "projects/proj-delete/session.json".into(),
            event_time_ms: stale_time.timestamp_millis(),
            session_hint: Some("session-delete".into()),
            event_kind: "session_touch".into(),
            model_hint: None,
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({}),
        });

        let expected_deleted_folder = std::fs::canonicalize(&proj)
            .expect("canonicalize project folder")
            .to_string_lossy()
            .to_string();

        let result = delete_session(&store, "session-delete").expect("delete should succeed");

        assert!(!result.blocked_live);
        assert_eq!(result.deleted_db_rows_by_table.requests, 1);
        assert_eq!(result.deleted_db_rows_by_table.local_events, 1);
        assert_eq!(result.deleted_folders, vec![expected_deleted_folder]);
        assert!(!proj.exists());
        assert_eq!(store.persisted_entry_count(), 0);
        assert_eq!(store.get_local_events(Some("session-delete"), 10).len(), 0);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn session_admin_delete_blocks_when_aggregated_session_is_live() {
        let root = std::env::temp_dir().join(format!(
            "claude-proxy-session-admin-live-{}",
            uuid::Uuid::new_v4()
        ));
        let projects_root = root.join("projects");
        let proj = projects_root.join("proj-live");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("session.json"),
            serde_json::json!({ "session_id": "session-live-aggregated" }).to_string(),
        )
        .unwrap();

        let store = StatsStore::new(20, root.clone(), 20.0, 8.0, 2_097_152, root.clone());

        let mut stale = sample_entry();
        stale.id = "req-session-admin-live-stale".into();
        stale.session_id = Some("session-live-aggregated".into());
        stale.timestamp = chrono::Utc::now() - chrono::Duration::minutes(10);
        store.add_entry(stale);

        store.upsert_local_event(&LocalEvent {
            id: "evt-session-admin-live-recent".into(),
            source_kind: crate::stats::SourceKind::ClaudeProject,
            source_path: "projects/proj-live/session.json".into(),
            event_time_ms: chrono::Utc::now().timestamp_millis(),
            session_hint: Some("session-live-aggregated".into()),
            event_kind: "session_touch".into(),
            model_hint: None,
            payload_policy: PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({}),
        });

        let result = delete_session(&store, "session-live-aggregated")
            .expect("delete should return blocked live result");

        assert!(result.blocked_live);
        assert_eq!(
            result.deleted_db_rows_by_table,
            SessionDeleteDbRowsByTable::default()
        );
        assert!(result.deleted_folders.is_empty());
        assert!(proj.exists());

        assert_eq!(store.persisted_entry_count(), 1);
        assert_eq!(store.get_local_events(Some("session-live-aggregated"), 10).len(), 1);

        let _ = std::fs::remove_dir_all(&root);
    }
}
