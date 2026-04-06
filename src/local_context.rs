use crate::correlation::PayloadPolicy;
use crate::stats::{LocalEvent, SourceKind};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub async fn ingest_projects_once_async(
    root: PathBuf,
    policy: PayloadPolicy,
    checkpoint: Option<String>,
) -> Vec<LocalEvent> {
    tokio::task::spawn_blocking(move || ingest_projects_once(&root, policy, checkpoint.as_deref()))
        .await
        .unwrap_or_default()
}

pub fn ingest_shell_snapshots_once(
    root: &Path,
    policy: PayloadPolicy,
    checkpoint: Option<&str>,
) -> Vec<LocalEvent> {
    let shell_root = root.join("shell-snapshots");
    if !shell_root.exists() {
        return Vec::new();
    }

    let checkpoint_ms = checkpoint.and_then(|value| value.parse::<i64>().ok());
    let files = collect_project_files_iterative(&shell_root);

    let mut events = Vec::new();
    for path in files {
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.ends_with(".json"))
            .unwrap_or(false)
        {
            continue;
        }

        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };

        let Ok(raw_json) = serde_json::from_str::<Value>(&text) else {
            continue;
        };

        let event_time_ms = file_modified_ms(&path).unwrap_or_else(|| Utc::now().timestamp_millis());
        if let Some(checkpoint_ms) = checkpoint_ms {
            if event_time_ms < checkpoint_ms {
                continue;
            }
        }

        let (session_hint, model_hint) = extract_hints(&raw_json);
        let payload_json = match policy {
            PayloadPolicy::MetadataOnly => json!({
                "path": path.to_string_lossy(),
                "size_bytes": text.len(),
            }),
            PayloadPolicy::Redacted => {
                let keys = raw_json
                    .as_object()
                    .map(|obj| obj.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                json!({
                    "path": path.to_string_lossy(),
                    "keys": keys,
                    "redacted": true,
                })
            }
            PayloadPolicy::Full => raw_json.clone(),
        };

        events.push(LocalEvent {
            id: event_id_for_path(&path, event_time_ms),
            source_kind: SourceKind::ShellSnapshot,
            source_path: path.to_string_lossy().into_owned(),
            event_time_ms,
            session_hint,
            event_kind: "shell_snapshot".to_string(),
            model_hint,
            payload_policy: policy,
            payload_json,
        });
    }

    events
}

pub fn ingest_config_snapshots_once(
    root: &Path,
    policy: PayloadPolicy,
    _checkpoint: Option<&str>,
) -> Vec<LocalEvent> {
    let candidates = [
        root.join("settings.json"),
        root.join("settings.local.json"),
    ];

    let mut events = Vec::new();

    for path in candidates {
        if !path.exists() {
            continue;
        }

        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };

        let Ok(raw_json) = serde_json::from_str::<Value>(&text) else {
            continue;
        };

        let event_time_ms = file_modified_ms(&path).unwrap_or_else(|| Utc::now().timestamp_millis());
        let (session_hint, model_hint) = extract_hints(&raw_json);

        let content_hash = content_hash(&text);
        let baseline_file = path.with_extension(".baseline_hash");

        let baseline_hash = fs::read_to_string(&baseline_file).ok();
        if baseline_hash.as_deref() == Some(content_hash.as_str()) {
            continue;
        }

        let _ = fs::write(&baseline_file, &content_hash);

        if baseline_hash.is_none() {
            continue;
        }

        let payload_json = match policy {
            PayloadPolicy::MetadataOnly => json!({
                "path": path.to_string_lossy(),
                "size_bytes": text.len(),
                "content_hash": content_hash,
            }),
            PayloadPolicy::Redacted => {
                let keys = raw_json
                    .as_object()
                    .map(|obj| obj.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                json!({
                    "path": path.to_string_lossy(),
                    "keys": keys,
                    "content_hash": content_hash,
                    "redacted": true,
                })
            }
            PayloadPolicy::Full => raw_json.clone(),
        };

        events.push(LocalEvent {
            id: event_id_for_path(&path, event_time_ms),
            source_kind: SourceKind::Config,
            source_path: path.to_string_lossy().into_owned(),
            event_time_ms,
            session_hint,
            event_kind: "config_change".to_string(),
            model_hint,
            payload_policy: policy,
            payload_json,
        });
    }

    events
}

pub fn ingest_projects_once(
    root: &Path,
    policy: PayloadPolicy,
    checkpoint: Option<&str>,
) -> Vec<LocalEvent> {
    let projects_root = root.join("projects");
    if !projects_root.exists() {
        return Vec::new();
    }

    let checkpoint_ms = checkpoint.and_then(|value| value.parse::<i64>().ok());
    let files = collect_project_files_iterative(&projects_root);

    let mut events = Vec::new();
    for path in files {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };

        if !file_name.ends_with(".json") {
            continue;
        }

        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };

        let Ok(raw_json) = serde_json::from_str::<Value>(&text) else {
            continue;
        };

        let event_time_ms = file_modified_ms(&path).unwrap_or_else(|| Utc::now().timestamp_millis());
        if let Some(checkpoint_ms) = checkpoint_ms {
            if event_time_ms < checkpoint_ms {
                continue;
            }
        }

        let (session_hint, model_hint) = extract_hints(&raw_json);
        let payload_json = match policy {
            PayloadPolicy::MetadataOnly => {
                json!({
                    "path": path.to_string_lossy(),
                    "size_bytes": text.len(),
                })
            }
            PayloadPolicy::Redacted => {
                let keys = raw_json
                    .as_object()
                    .map(|obj| obj.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                json!({
                    "path": path.to_string_lossy(),
                    "keys": keys,
                    "redacted": true,
                })
            }
            PayloadPolicy::Full => raw_json.clone(),
        };

        events.push(LocalEvent {
            id: event_id_for_path(&path, event_time_ms),
            source_kind: SourceKind::ClaudeProject,
            source_path: path.to_string_lossy().into_owned(),
            event_time_ms,
            session_hint,
            event_kind: "project_metadata".to_string(),
            model_hint,
            payload_policy: policy,
            payload_json,
        });
    }

    events
}

fn collect_project_files_iterative(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };

            if metadata.file_type().is_symlink() {
                continue;
            }

            if metadata.is_dir() {
                stack.push(path);
            } else if metadata.is_file() {
                files.push(path);
            }
        }
    }

    files
}

fn file_modified_ms(path: &Path) -> Option<i64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let duration = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(duration.as_millis().min(i64::MAX as u128) as i64)
}

fn event_id_for_path(path: &Path, event_time_ms: i64) -> String {
    let mut hasher = DefaultHasher::new();
    path.to_string_lossy().hash(&mut hasher);
    event_time_ms.hash(&mut hasher);
    format!("local-project-{:x}", hasher.finish())
}

fn content_hash(text: &str) -> String {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

fn extract_hints(raw_json: &Value) -> (Option<String>, Option<String>) {
    let session_hint = first_string_field(raw_json, &["session_id", "sessionId", "session"]);
    let model_hint = first_string_field(raw_json, &["model", "model_name", "modelName"]);
    (session_hint, model_hint)
}

fn first_string_field(value: &Value, keys: &[&str]) -> Option<String> {
    let obj = value.as_object()?;
    for key in keys {
        if let Some(found) = obj.get(*key).and_then(|v| v.as_str()) {
            if !found.is_empty() {
                return Some(found.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn ingest_projects_includes_events_at_checkpoint_timestamp() {
        let temp_root = std::env::temp_dir().join(format!(
            "claude-proxy-local-context-checkpoint-test-{}",
            uuid::Uuid::new_v4()
        ));
        let claude_dir = temp_root.join(".claude");
        let projects_dir = claude_dir.join("projects").join("project-checkpoint");
        fs::create_dir_all(&projects_dir).expect("should create test projects dir");

        let project_file = projects_dir.join("metadata.json");
        fs::write(
            &project_file,
            r#"{"session_id":"sess-eq","model":"claude-opus-4-1"}"#,
        )
        .expect("should write test metadata");

        let modified_ms = super::file_modified_ms(&project_file).expect("expected mtime");
        let events = ingest_projects_once(
            &claude_dir,
            PayloadPolicy::MetadataOnly,
            Some(&modified_ms.to_string()),
        );

        assert_eq!(events.len(), 1, "event at checkpoint should still be ingested");

        let _ = fs::remove_dir_all(&temp_root);
    }

    #[test]
    fn ingest_projects_skips_symlinked_directories() {
        let temp_root = std::env::temp_dir().join(format!(
            "claude-proxy-local-context-symlink-test-{}",
            uuid::Uuid::new_v4()
        ));
        let claude_dir = temp_root.join(".claude");
        let projects_root = claude_dir.join("projects");
        let real_project_dir = projects_root.join("real-project");
        let linked_target_dir = temp_root.join("outside-project");

        fs::create_dir_all(&real_project_dir).expect("should create real project dir");
        fs::create_dir_all(&linked_target_dir).expect("should create linked target dir");

        fs::write(
            real_project_dir.join("metadata.json"),
            r#"{"session_id":"sess-real","model":"claude-opus-4-1"}"#,
        )
        .expect("should write real metadata");

        fs::write(
            linked_target_dir.join("metadata.json"),
            r#"{"session_id":"sess-link","model":"claude-opus-4-1"}"#,
        )
        .expect("should write linked metadata");

        let link_path = projects_root.join("linked-project");
        create_dir_link(&linked_target_dir, &link_path);

        let events = ingest_projects_once(&claude_dir, PayloadPolicy::MetadataOnly, None);
        assert_eq!(events.len(), 1, "symlinked directory should be skipped");
        assert_eq!(events[0].session_hint.as_deref(), Some("sess-real"));

        let _ = fs::remove_dir_all(&temp_root);
    }

    #[cfg(unix)]
    fn create_dir_link(target: &std::path::Path, link: &std::path::Path) {
        std::os::unix::fs::symlink(target, link).expect("should create directory symlink");
    }

    #[cfg(windows)]
    fn create_dir_link(target: &std::path::Path, link: &std::path::Path) {
        let output = std::process::Command::new("cmd")
            .args([
                "/C",
                "mklink",
                "/J",
                &link.to_string_lossy(),
                &target.to_string_lossy(),
            ])
            .output()
            .expect("should invoke mklink");

        assert!(
            output.status.success(),
            "should create directory junction: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn ingest_shell_snapshot_respects_redacted_policy() {
        let temp_root = std::env::temp_dir().join(format!(
            "claude-proxy-shell-snapshot-test-{}",
            uuid::Uuid::new_v4()
        ));
        let claude_dir = temp_root.join(".claude");
        let shell_dir = claude_dir.join("shell-snapshots");
        fs::create_dir_all(&shell_dir).expect("should create shell snapshot dir");

        let shell_file = shell_dir.join("snapshot.json");
        fs::write(
            &shell_file,
            r#"{"session_id":"sess-shell-1","command":"export API_KEY=secret-token","token":"secret-token"}"#,
        )
        .expect("should write shell snapshot");

        let events = ingest_shell_snapshots_once(&claude_dir, PayloadPolicy::Redacted, None);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source_kind, SourceKind::ShellSnapshot);
        assert_eq!(events[0].payload_policy, PayloadPolicy::Redacted);
        let payload_text = events[0].payload_json.to_string().to_ascii_lowercase();
        assert!(!payload_text.contains("secret-token"), "redacted payload should not contain secret token");

        let _ = fs::remove_dir_all(&temp_root);
    }

    #[test]
    fn ingest_config_change_emits_config_drift_event() {
        let temp_root = std::env::temp_dir().join(format!(
            "claude-proxy-config-drift-test-{}",
            uuid::Uuid::new_v4()
        ));
        let claude_dir = temp_root.join(".claude");
        fs::create_dir_all(&claude_dir).expect("should create .claude dir");

        let config_path = claude_dir.join("settings.json");
        fs::write(&config_path, r#"{"model":"claude-haiku-4.5"}"#).expect("should write first config snapshot");

        let baseline = ingest_config_snapshots_once(&claude_dir, PayloadPolicy::MetadataOnly, None);
        assert_eq!(baseline.len(), 0, "first snapshot should establish baseline without drift event");

        fs::write(&config_path, r#"{"model":"claude-opus-4.6"}"#).expect("should write changed config snapshot");

        let drift_events = ingest_config_snapshots_once(&claude_dir, PayloadPolicy::MetadataOnly, None);
        assert_eq!(drift_events.len(), 1);
        assert_eq!(drift_events[0].source_kind, SourceKind::Config);
        assert_eq!(drift_events[0].event_kind, "config_change");

        let _ = fs::remove_dir_all(&temp_root);
    }

    #[test]
    fn ingest_projects_metadata_creates_events() {
        let temp_root = std::env::temp_dir().join(format!(
            "claude-proxy-local-context-test-{}",
            uuid::Uuid::new_v4()
        ));
        let claude_dir = temp_root.join(".claude");
        let projects_dir = claude_dir.join("projects").join("project-a");
        fs::create_dir_all(&projects_dir).expect("should create test projects dir");

        let project_file = projects_dir.join("metadata.json");
        fs::write(
            &project_file,
            r#"{"session_id":"sess-123","model":"claude-opus-4-1"}"#,
        )
        .expect("should write test metadata");

        let events = ingest_projects_once(&claude_dir, PayloadPolicy::MetadataOnly, None);

        assert_eq!(events.len(), 1, "expected one local event");
        let event = &events[0];
        assert_eq!(event.source_kind, SourceKind::ClaudeProject);
        assert_eq!(event.payload_policy, PayloadPolicy::MetadataOnly);
        assert_eq!(event.session_hint.as_deref(), Some("sess-123"));
        assert_eq!(event.model_hint.as_deref(), Some("claude-opus-4-1"));

        let _ = fs::remove_dir_all(&temp_root);
    }
}
