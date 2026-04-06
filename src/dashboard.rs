use crate::session_admin;
use crate::settings_admin::SettingsAdmin;
use crate::stats::*;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{delete, get, patch, post},
    Json, Router,
};
use std::sync::Arc;

pub async fn run_dashboard(store: Arc<StatsStore>, port: u16) -> Result<(), String> {
    let app = build_dashboard_app(store);

    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::AddrInUse {
                format!(
                    "Dashboard port {port} is already in use. Stop the existing process or run with --dashboard-port <free-port>."
                )
            } else {
                format!("Failed to bind dashboard port {port}: {err}")
            }
        })?;

    axum::serve(listener, app)
        .await
        .map_err(|err| format!("Dashboard server stopped unexpectedly: {err}"))
}

fn build_dashboard_app(store: Arc<StatsStore>) -> Router {
    Router::new()
        .route("/", get(serve_dashboard))
        .route("/api/stats", get(api_stats))
        .route("/api/entries", get(api_entries))
        .route("/api/sessions", get(api_sessions))
        .route("/api/correlations", get(api_correlations))
        .route("/api/explanations", get(api_explanations))
        .route("/api/timeline", get(api_timeline))
        .route("/api/session-graph", get(api_session_graph))
        .route("/api/session-details", get(api_session_details))
        .route("/api/session", delete(api_delete_session))
        .route("/api/settings/current", get(api_settings_current))
        .route(
            "/api/settings/history",
            get(api_settings_history).delete(api_settings_history_delete_all),
        )
        .route(
            "/api/settings/history/:revision_id",
            delete(api_settings_history_delete_revision),
        )
        .route(
            "/api/settings/history/:revision_id/tags",
            patch(api_settings_history_patch_tags),
        )
        .route("/api/settings/backups", get(api_settings_backups_legacy_gone))
        .route("/api/settings/apply", post(api_settings_apply))
        .route(
            "/api/settings/backups/delete-selected",
            post(api_settings_backups_delete_selected_legacy_gone),
        )
        .route(
            "/api/settings/backups/delete-all",
            post(api_settings_backups_delete_all_legacy_gone),
        )
        .route("/api/reset-memory", post(api_reset_memory))
        .route("/api/reset", post(api_reset))
        .route("/api/entry-body", get(api_entry_body))
        .route("/api/claude-sessions", get(api_claude_sessions))
        .route("/ws", get(ws_handler))
        .with_state(store)
}

async fn serve_dashboard() -> impl IntoResponse {
    Html(include_str!("dashboard.html"))
}

#[derive(serde::Deserialize, Default)]
struct StatsQuery {
    mode: Option<StatsMode>,
}

async fn api_stats(
    State(store): State<Arc<StatsStore>>,
    Query(q): Query<StatsQuery>,
) -> impl IntoResponse {
    let snapshot = match q.mode.unwrap_or(StatsMode::Live) {
        StatsMode::Live => store.get_live_stats_snapshot(),
        StatsMode::Historical => store.get_historical_stats_snapshot(),
    };
    axum::Json(snapshot)
}

#[derive(serde::Deserialize, Default)]
struct EntriesQuery {
    limit: Option<usize>,
    offset: Option<usize>,
    search: Option<String>,
    status: Option<String>,
    model: Option<String>,
    session_id: Option<String>,
    has_stalls: Option<bool>,
    has_anomalies: Option<bool>,
    min_ttft_ms: Option<f64>,
    min_duration_ms: Option<f64>,
    sort_by: Option<String>,
    sort_order: Option<String>,
    anomaly_ts_ms: Option<String>,
    window_ms: Option<String>,
    window_mode: Option<String>,
}

#[derive(serde::Serialize)]
struct EntriesAnomalyFocus {
    anomaly_ts_ms: i64,
    window_ms: i64,
    within_window_count: usize,
}

#[derive(serde::Serialize)]
struct EntriesResponse {
    entries: Vec<RequestEntryWithAnomalyMeta>,
    preselected_request_id: Option<String>,
    anomaly_focus: Option<EntriesAnomalyFocus>,
}

async fn api_entries(
    State(store): State<Arc<StatsStore>>,
    Query(q): Query<EntriesQuery>,
) -> impl IntoResponse {
    let anomaly_ts_ms = q
        .anomaly_ts_ms
        .as_deref()
        .and_then(|value| value.parse::<i64>().ok());
    let _window_mode = q.window_mode.as_deref();

    let filter = EntryFilter {
        search: q.search,
        status: q.status,
        model: q.model,
        session_id: q.session_id,
        session_id_null: None,
        has_stalls: q.has_stalls,
        has_anomalies: q.has_anomalies,
        min_ttft_ms: q.min_ttft_ms,
        min_duration_ms: q.min_duration_ms,
    };

    let limit = q.limit.unwrap_or(100);
    let offset = q.offset.unwrap_or(0);

    if let Some(anomaly_ts_ms) = anomaly_ts_ms {
        let window_ms = q
            .window_ms
            .as_deref()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(120_000)
            .clamp(1_000, 3_600_000);

        let focused = store.get_entries_with_anomaly_focus(limit, offset, &filter, anomaly_ts_ms, window_ms);
        let within_window_count = focused.entries.len();

        return axum::Json(EntriesResponse {
            entries: focused.entries,
            preselected_request_id: focused.preselected_request_id,
            anomaly_focus: Some(EntriesAnomalyFocus {
                anomaly_ts_ms,
                window_ms,
                within_window_count,
            }),
        });
    }

    let entries = store.get_entries(limit, offset, &filter, q.sort_by.as_deref(), q.sort_order.as_deref());

    axum::Json(EntriesResponse {
        entries: entries
            .into_iter()
            .map(|entry| RequestEntryWithAnomalyMeta {
                entry,
                distance_ms: None,
                within_window: None,
            })
            .collect(),
        preselected_request_id: None,
        anomaly_focus: None,
    })
}

async fn api_sessions(State(store): State<Arc<StatsStore>>) -> impl IntoResponse {
    let sessions = store.get_sessions();
    axum::Json(sessions)
}

#[derive(serde::Serialize)]
struct ApiSuccessEnvelope<T> {
    ok: bool,
    data: T,
}

#[derive(serde::Serialize)]
struct ApiFailureEnvelope {
    ok: bool,
    error: ApiErrorBody,
}

#[derive(serde::Serialize)]
struct ApiErrorBody {
    code: String,
    message: String,
    details: serde_json::Value,
}

#[derive(serde::Deserialize, Default)]
struct SettingsHistoryQuery {
    limit: Option<usize>,
    offset: Option<usize>,
    q: Option<String>,
    search: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct SettingsHistoryTagsPatchRequest {
    add: Option<Vec<String>>,
    remove: Option<Vec<String>>,
}

#[derive(serde::Deserialize, Default)]
struct SettingsHistoryDeleteAllRequest {
    confirm: Option<bool>,
}

fn settings_admin_error_status(code: &str) -> StatusCode {
    match code {
        "invalid_payload" => StatusCode::BAD_REQUEST,
        "not_found" => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn settings_admin_error_response(err: crate::settings_admin::SettingsAdminError) -> axum::response::Response {
    (
        settings_admin_error_status(&err.code),
        axum::Json(ApiFailureEnvelope {
            ok: false,
            error: ApiErrorBody {
                code: err.code,
                message: err.message,
                details: serde_json::json!({}),
            },
        }),
    )
        .into_response()
}

async fn api_settings_current(State(store): State<Arc<StatsStore>>) -> impl IntoResponse {
    let admin = SettingsAdmin::new(store);
    match admin.get_current() {
        Ok(current) => {
            let current = current.unwrap_or(crate::settings_admin::SettingsCurrentResponse {
                updated_at_ms: 0,
                proxy_settings: crate::settings_admin::ProxySettingsDocument {
                    raw_json: serde_json::Value::Object(Default::default()),
                },
                claude_settings: crate::settings_admin::ClaudeSettingsDocument {
                    raw_json: serde_json::Value::Object(Default::default()),
                },
                db_file_mismatch: false,
                file_recreated_from_db: false,
            });

            (
                StatusCode::OK,
                axum::Json(ApiSuccessEnvelope {
                    ok: true,
                    data: current,
                }),
            )
                .into_response()
        }
        Err(err) => settings_admin_error_response(err),
    }
}

async fn api_settings_history(
    State(store): State<Arc<StatsStore>>,
    Query(q): Query<SettingsHistoryQuery>,
) -> impl IntoResponse {
    let admin = SettingsAdmin::new(store);
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let offset = q.offset.unwrap_or(0);
    let search = q
        .q
        .as_deref()
        .or(q.search.as_deref());

    match admin.get_history(limit, offset, search) {
        Ok(history) => (
            StatusCode::OK,
            axum::Json(ApiSuccessEnvelope {
                ok: true,
                data: history,
            }),
        )
            .into_response(),
        Err(err) => settings_admin_error_response(err),
    }
}

async fn api_settings_history_patch_tags(
    State(store): State<Arc<StatsStore>>,
    Path(revision_id): Path<i64>,
    axum::Json(payload): axum::Json<SettingsHistoryTagsPatchRequest>,
) -> impl IntoResponse {
    let admin = SettingsAdmin::new(store);
    let add = payload.add.unwrap_or_default();
    let remove = payload.remove.unwrap_or_default();

    match admin.patch_history_tags(revision_id, &add, &remove) {
        Ok(history_item) => (
            StatusCode::OK,
            axum::Json(ApiSuccessEnvelope {
                ok: true,
                data: history_item,
            }),
        )
            .into_response(),
        Err(err) => settings_admin_error_response(err),
    }
}

async fn api_settings_history_delete_revision(
    State(store): State<Arc<StatsStore>>,
    Path(revision_id): Path<i64>,
) -> impl IntoResponse {
    let admin = SettingsAdmin::new(store);
    match admin.delete_history_revision(revision_id) {
        Ok(()) => (
            StatusCode::OK,
            axum::Json(ApiSuccessEnvelope {
                ok: true,
                data: serde_json::json!({"deleted": true}),
            }),
        )
            .into_response(),
        Err(err) => settings_admin_error_response(err),
    }
}

async fn api_settings_history_delete_all(
    State(store): State<Arc<StatsStore>>,
    axum::Json(payload): axum::Json<SettingsHistoryDeleteAllRequest>,
) -> impl IntoResponse {
    if payload.confirm != Some(true) {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(ApiFailureEnvelope {
                ok: false,
                error: ApiErrorBody {
                    code: "invalid_payload".to_string(),
                    message: "confirm=true is required".to_string(),
                    details: serde_json::json!({"field": "confirm"}),
                },
            }),
        )
            .into_response();
    }

    let admin = SettingsAdmin::new(store);
    match admin.delete_history_all() {
        Ok(deleted_count) => (
            StatusCode::OK,
            axum::Json(ApiSuccessEnvelope {
                ok: true,
                data: serde_json::json!({"deleted_count": deleted_count}),
            }),
        )
            .into_response(),
        Err(err) => settings_admin_error_response(err),
    }
}

async fn api_settings_backups_legacy_gone() -> impl IntoResponse {
    (
        StatusCode::GONE,
        axum::Json(ApiFailureEnvelope {
            ok: false,
            error: ApiErrorBody {
                code: "gone".to_string(),
                message: "settings backup endpoints have been removed; use settings history revision APIs instead".to_string(),
                details: serde_json::json!({
                    "replacement": "/api/settings/history",
                    "guidance": "Use /api/settings/history for listing and /api/settings/history/:revision_id for targeted delete operations."
                }),
            },
        }),
    )
        .into_response()
}

async fn api_settings_backups_delete_selected_legacy_gone() -> impl IntoResponse {
    api_settings_backups_legacy_gone().await
}

async fn api_settings_backups_delete_all_legacy_gone() -> impl IntoResponse {
    api_settings_backups_legacy_gone().await
}

async fn api_settings_apply(
    State(store): State<Arc<StatsStore>>,
    axum::Json(payload): axum::Json<serde_json::Value>,
) -> impl IntoResponse {
    let admin = SettingsAdmin::new(store);
    match admin.apply_settings(payload) {
        Ok(current) => (
            StatusCode::OK,
            axum::Json(ApiSuccessEnvelope {
                ok: true,
                data: current,
            }),
        )
            .into_response(),
        Err(err) => settings_admin_error_response(err),
    }
}

#[derive(serde::Deserialize)]
struct CorrelationsQuery {
    request_id: String,
    limit: Option<usize>,
}

#[derive(serde::Deserialize)]
struct ExplanationsQuery {
    request_id: String,
    limit: Option<usize>,
}

#[derive(serde::Deserialize)]
struct TimelineQuery {
    session_id: String,
    from: Option<i64>,
    to: Option<i64>,
    limit: Option<usize>,
}

#[derive(serde::Deserialize)]
struct SessionGraphQuery {
    session_id: String,
    limit: Option<usize>,
}

#[derive(serde::Serialize)]
struct TimelineItem {
    timestamp_ms: i64,
    kind: String,
    request_id: Option<String>,
    local_event_id: Option<String>,
    label: String,
}

async fn api_correlations(
    State(store): State<Arc<StatsStore>>,
    Query(q): Query<CorrelationsQuery>,
) -> impl IntoResponse {
    let links = store.get_correlations_for_request(&q.request_id, q.limit.unwrap_or(50));
    axum::Json(links)
}

async fn api_explanations(
    State(store): State<Arc<StatsStore>>,
    Query(q): Query<ExplanationsQuery>,
) -> impl IntoResponse {
    let rows = store.get_explanations_for_request(&q.request_id, q.limit.unwrap_or(10));
    axum::Json(rows)
}

async fn api_timeline(
    State(store): State<Arc<StatsStore>>,
    Query(q): Query<TimelineQuery>,
) -> impl IntoResponse {
    let cap = q.limit.unwrap_or(200).min(1000);

    let mut request_items: Vec<TimelineItem> = store
        .get_entries(cap, 0, &EntryFilter {
            session_id: Some(q.session_id.clone()),
            ..EntryFilter::default()
        }, None, None)
        .into_iter()
        .filter_map(|entry| {
            let ts = entry.timestamp.timestamp_millis();
            if q.from.is_some_and(|from| ts < from) || q.to.is_some_and(|to| ts > to) {
                return None;
            }

            Some(TimelineItem {
                timestamp_ms: ts,
                kind: "request".into(),
                request_id: Some(entry.id),
                local_event_id: None,
                label: format!("{} {}", entry.method, entry.path),
            })
        })
        .collect();

    let mut event_items: Vec<TimelineItem> = store
        .get_local_events(Some(&q.session_id), cap)
        .into_iter()
        .filter_map(|event| {
            let ts = event.event_time_ms;
            if q.from.is_some_and(|from| ts < from) || q.to.is_some_and(|to| ts > to) {
                return None;
            }

            Some(TimelineItem {
                timestamp_ms: ts,
                kind: "local_event".into(),
                request_id: None,
                local_event_id: Some(event.id),
                label: event.event_kind,
            })
        })
        .collect();

    request_items.append(&mut event_items);
    request_items.sort_by_key(|item| item.timestamp_ms);
    if request_items.len() > cap {
        request_items.truncate(cap);
    }

    axum::Json(request_items)
}

async fn api_session_graph(
    State(store): State<Arc<StatsStore>>,
    Query(q): Query<SessionGraphQuery>,
) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(200).min(1000);
    let graph = store
        .get_session_graph(&q.session_id, limit)
        .unwrap_or(SessionGraph {
            session_id: q.session_id,
            nodes: Vec::new(),
            edges: Vec::new(),
        });

    axum::Json(graph)
}

#[derive(serde::Deserialize)]
struct SessionDetailsQuery {
    session_id: String,
    project_path: Option<String>,
    limit: Option<usize>,
    include_full_text: Option<bool>,
}

#[derive(serde::Deserialize)]
struct DeleteSessionRequest {
    session_id: String,
}

fn build_unknown_session_details(store: &StatsStore, limit: usize) -> SessionDetailsResponse {
    let cap = limit.max(1);
    let null_filter = EntryFilter {
        session_id_null: Some(true),
        ..EntryFilter::default()
    };
    // Fetch cap+1 to detect truncation without scanning all rows.
    let entries = store.get_entries(cap + 1, 0, &null_filter, Some("timestamp_ms"), Some("desc"));
    let truncated = entries.len() > cap;
    let entries: Vec<_> = entries.into_iter().take(cap).collect();

    let request_rows = entries
        .iter()
        .map(|entry| SessionRequestSummary {
            id: entry.id.clone(),
            timestamp_ms: entry.timestamp.timestamp_millis(),
            status: Some(entry.status.code_str().to_string()),
            ttft_ms: entry.ttft_ms,
            duration_ms: Some(entry.duration_ms),
            path: Some(entry.path.clone()),
            model: Some(entry.model.clone()),
        })
        .collect::<Vec<_>>();

    let timeline = request_rows
        .iter()
        .map(|request| SessionTimelineItem {
            id: request.id.clone(),
            timestamp_ms: request.timestamp_ms,
            kind: "request".to_string(),
            label: format!("request {}", request.path.clone().unwrap_or_else(|| "-".to_string())),
            request_id: Some(request.id.clone()),
            local_event_id: None,
            conversation_id: None,
        })
        .collect::<Vec<_>>();

    let proxy_error_count_total = entries.iter().filter(|entry| entry.status.is_error()).count() as u64;
    let proxy_stall_count_total = entries.iter().map(|entry| entry.stalls.len() as u64).sum();

    SessionDetailsResponse {
        session_id: "unknown".to_string(),
        presence: if entries.is_empty() {
            SessionPresence::None
        } else {
            SessionPresence::ProxyOnly
        },
        project_paths: Vec::new(),
        selected_project_path: None,
        summary: SessionDetailsSummary {
            proxy_request_count_total: entries.len() as u64,
            proxy_error_count_total,
            proxy_stall_count_total,
            local_event_count_total: 0,
            conversation_message_count_total: 0,
            last_proxy_activity_ms: entries.first().map(|entry| entry.timestamp.timestamp_millis()),
            last_local_activity_ms: None,
        },
        requests: request_rows,
        timeline,
        conversation: Vec::new(),
        truncated_sections: SessionTruncatedSections {
            requests: truncated,
            timeline: truncated,
            conversation: false,
            full_text: false,
        },
    }
}

async fn api_session_details(
    State(store): State<Arc<StatsStore>>,
    Query(q): Query<SessionDetailsQuery>,
) -> impl IntoResponse {
    let session_id = q.session_id.trim();
    if session_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "session_id is required"})),
        )
            .into_response();
    }

    let requested_limit = q.limit.unwrap_or(200).max(1);
    let details = if session_id == "unknown" {
        build_unknown_session_details(&store, requested_limit)
    } else {
        store.get_session_details(
            session_id,
            q.project_path.as_deref(),
            requested_limit,
            q.include_full_text.unwrap_or(false),
        )
    };

    if matches!(details.presence, SessionPresence::None) {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "not found"})),
        )
            .into_response();
    }

    (StatusCode::OK, Json(details)).into_response()
}


async fn api_delete_session(
    State(store): State<Arc<StatsStore>>,
    Json(payload): Json<DeleteSessionRequest>,
) -> impl IntoResponse {
    let session_id = payload.session_id.trim();
    if session_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "session_id is required"})),
        )
            .into_response();
    }

    match session_admin::delete_session(&store, session_id) {
        Ok(result) if result.blocked_live => (StatusCode::CONFLICT, Json(serde_json::json!(result))).into_response(),
        Ok(result) if result.not_found => (StatusCode::NOT_FOUND, Json(serde_json::json!(result))).into_response(),
        Ok(result) => (StatusCode::OK, Json(serde_json::json!(result))).into_response(),
        Err(err) if err.contains("session_id is required") => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": err})),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err})),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct EntryBodyQuery {
    request_id: String,
}

async fn api_entry_body(
    State(store): State<Arc<StatsStore>>,
    Query(q): Query<EntryBodyQuery>,
) -> impl IntoResponse {
    match store.get_body(&q.request_id) {
        Some(body) => (StatusCode::OK, Json(serde_json::json!(body))).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "not found"})),
        )
            .into_response(),
    }
}

async fn api_claude_sessions(State(store): State<Arc<StatsStore>>) -> impl IntoResponse {
    axum::Json(store.get_claude_sessions())
}


async fn api_reset_memory(State(store): State<Arc<StatsStore>>) -> impl IntoResponse {
    axum::Json(store.clear_stats())
}

async fn api_reset(State(store): State<Arc<StatsStore>>) -> impl IntoResponse {
    axum::Json(store.clear_all())
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(store): State<Arc<StatsStore>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_connection(socket, store))
}

async fn ws_connection(mut socket: WebSocket, store: Arc<StatsStore>) {
    let mut rx = store.broadcast_tx.subscribe();

    // Send initial stats
    let stats = store.get_live_stats_snapshot();
    if let Ok(json) = serde_json::to_string(&stats) {
        let msg = format!("{{\"type\":\"stats\",\"data\":{json}}}");
        let _ = socket.send(Message::Text(msg)).await;
    }

    // Periodic stats + real-time entries
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(2));

    loop {
        tokio::select! {
            // Broadcast new entries
            msg = rx.recv() => {
                match msg {
                    Ok(text) => {
                        if socket.send(Message::Text(text)).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            // Periodic stats update
            _ = interval.tick() => {
                let stats = store.get_live_stats_snapshot();
                if let Ok(json) = serde_json::to_string(&stats) {
                    let msg = format!("{{\"type\":\"stats\",\"data\":{json}}}");
                    if socket.send(Message::Text(msg)).await.is_err() {
                        break;
                    }
                }
            }
            // Client disconnect
            msg = socket.recv() => {
                if msg.is_none() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Method, Request, StatusCode},
    };
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn dashboard_html_includes_correlation_panel() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("id=\"correlation-panel\""));
        assert!(html.contains("loadCorrelations("));
        assert!(html.contains("/api/correlations?request_id="));
        assert!(html.contains("tr.addEventListener('click'"));
        assert!(html.contains("selectedRequestId = e.id"));
    }

    #[tokio::test]
    async fn dashboard_html_includes_explanations_and_timeline_sections() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("id=\"request-explanations\""));
        assert!(html.contains("id=\"session-timeline\""));
        assert!(html.contains("loadExplanations("));
        assert!(html.contains("loadSessionTimeline("));
    }

    #[tokio::test]
    async fn dashboard_html_includes_session_graph_section() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("id=\"session-graph\""));
        assert!(html.contains("id=\"session-graph-list\""));
        assert!(html.contains("loadSessionGraph("));
        assert!(html.contains("/api/session-graph?session_id="));
    }

    #[tokio::test]
    async fn dashboard_html_load_entries_supports_anomaly_focus_params() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("params.set('anomaly_ts_ms'"));
        assert!(html.contains("params.set('window_ms'"));
        assert!(html.contains("preselectRequestRow("));
    }

    #[tokio::test]
    async fn dashboard_html_includes_anomaly_focus_controls() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("id=\"anomaly-focus-bar\""));
        assert!(html.contains("id=\"clear-anomaly-focus-btn\""));
        assert!(html.contains("id=\"expand-anomaly-window-btn\""));
    }

    #[tokio::test]
    async fn dashboard_html_routes_anomaly_click_to_focus_flow() {
        let html = include_str!("dashboard.html");

        assert!(html.contains("function applyAnomalyFocus("));
        assert!(html.contains("function clearAnomalyFocus("));
        assert!(html.contains("function preselectRequestRow("));
        assert!(html.contains("applyAnomalyFocus(entry, a)"));

        let update_fn_start = html.find("function updateAnomalies(").expect("updateAnomalies exists");
        let apply_fn_start = html.find("function applyAnomalyFocus(").expect("applyAnomalyFocus exists");
        let update_anomalies_body = &html[update_fn_start..apply_fn_start];
        assert!(!update_anomalies_body.contains("openRequestModal(entry)"));
    }

    #[tokio::test]
    async fn dashboard_html_session_not_found_clears_all_sections() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("detailsResp.status === 404"));
        assert!(html.contains("Session not found."));
        assert!(html.contains("No timeline data for this session."));
        assert!(html.contains("No graph data for this session."));
    }

    #[tokio::test]
    async fn dashboard_html_modal_bodies_handles_404_as_empty_state() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("if (resp.status === 404)"));
        assert!(html.contains("No request/response bodies saved for this entry."));
    }

    #[tokio::test]
    async fn dashboard_html_includes_expand_anomaly_window_action() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("expandAnomalyWindow("));
        assert!(html.contains("expand-anomaly-window-btn').addEventListener('click', expandAnomalyWindow"));
        assert!(html.contains("windowMs: DEFAULT_ANOMALY_WINDOW_MS"));
        assert!(html.contains("expanded: false"));
        assert!(html.contains("params.set('anomaly_ts_ms'"));
        assert!(html.contains("params.set('window_ms', String(anomalyFocus.windowMs))"));
        assert!(html.contains("clearAnomalyFocus({ restoreSnapshot: false, reload: false });"));
        assert!(html.contains("if (!anomalyFocus) return;"));
        assert!(!html.contains("if (!anomalyFocus || entries.length > 0) return;"));
        assert!(html.contains("if (anomalyFocus) {"));
        assert!(html.contains("loadEntries();"));
        assert!(html.contains("anomalyFocusHasEmptyResults"));
    }


    #[tokio::test]
    async fn dashboard_html_declares_settings_history_index_state() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("const settingsHistoryById = new Map();"));
    }

    #[tokio::test]
    async fn dashboard_html_select_session_caches_conversation_preview_payload() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("const cacheKey = String(sessionId);"));
        assert!(html.contains("const cached = sessionDetails;"));
        assert!(html.contains("setSessionConversationCache(cacheKey, cached);"));
    }

    #[tokio::test]
    async fn dashboard_html_settings_tab_uses_history_left_editor_right_layout() {
        let html = include_str!("dashboard.html");
        let history_index = html
            .find("id=\"settings-history-panel\"")
            .expect("settings history panel exists");
        let editor_index = html
            .find("id=\"settings-editor-panel\"")
            .expect("settings editor panel exists");
        assert!(history_index < editor_index);
    }

    #[tokio::test]
    async fn dashboard_html_settings_history_rows_include_delete_affordance() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("data-settings-history-delete-revision-id"));
        assert!(html.contains("data-settings-history-revision-id"));
        assert!(html.contains("deleteSettingsHistoryRevision("));
    }

    #[tokio::test]
    async fn dashboard_html_settings_history_rows_avoid_nested_interactive_controls() {
        let html = include_str!("dashboard.html");
        assert!(!html.contains("<button type=\"button\" class=\"correlation-item\""));
        assert!(html.contains("role=\"button\""));
        assert!(html.contains("event.key === 'Enter' || event.key === ' '"));
    }

    #[tokio::test]
    async fn dashboard_html_settings_history_row_key_activation_ignores_delete_button_keyboard_events() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("event.target?.closest('[data-settings-history-delete-revision-id]')"));
    }

    #[tokio::test]
    async fn dashboard_html_settings_history_preview_supports_double_click_touch_and_enter() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("rowEl.addEventListener('dblclick', preview);"));
        assert!(html.contains("data-settings-history-preview-revision-id"));
        assert!(html.contains("previewButton.addEventListener('click'"));
        assert!(html.contains("if (event.key === 'Enter')"));
        assert!(html.contains("preview();"));
    }

    #[tokio::test]
    async fn dashboard_html_settings_history_preview_uses_full_json_without_editor_overwrite() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("const fullJson = JSON.stringify(normalized, null, 2);"));
        assert!(html.contains("previewContent.textContent = fullJson;"));
        assert!(html.contains("function previewSettingsHistoryItem(historyId)"));
    }

    #[tokio::test]
    async fn dashboard_html_settings_editor_overwrite_requires_explicit_load_action() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("id=\"settings-load-selected-btn\""));
        assert!(html.contains("function loadSelectedSettingsIntoEditor()"));
        assert!(html.contains("if (isSettingsEditorDirty() && !window.confirm('Replace editor contents with selected settings revision?'))"));
        assert!(html.contains("editor.value = JSON.stringify(normalized, null, 2);"));
        assert!(html.contains("settings-load-selected-btn')?.addEventListener('click', loadSelectedSettingsIntoEditor)"));
    }

    #[tokio::test]
    async fn dashboard_html_settings_history_marks_preview_target_with_aria_current() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("const isPreview = id === selectedSettingsPreviewId;"));
        assert!(html.contains("button.setAttribute('aria-current', isPreview ? 'true' : 'false');"));
        assert!(html.contains("aria-current=\"${isPreview ? 'true' : 'false'}\""));
    }

    #[tokio::test]
    async fn dashboard_html_settings_editor_dirty_state_preserved_until_explicit_load() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("function isSettingsEditorDirty()"));
        assert!(html.contains("editor.dataset.lastLoadedSnapshot = JSON.stringify(normalized);"));
        assert!(html.contains("} else if (editor.dataset.lastLoadedSnapshot == null) {"));
    }

    #[tokio::test]
    async fn dashboard_html_settings_editor_includes_quick_tags_input() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("id=\"settings-quick-tags-input\""));
        assert!(html.contains("id=\"settings-quick-tags-hint\""));
        assert!(html.contains("quick_tags"));
    }

    #[tokio::test]
    async fn dashboard_html_settings_history_includes_clear_all_control() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("id=\"settings-history-clear-all-btn\""));
        assert!(html.contains("clearAllSettingsHistory("));
    }

    #[tokio::test]
    async fn dashboard_html_settings_history_search_uses_unified_q_query() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("params.set('q', query);"));
        assert!(!html.contains("params.set('search', query);"));
    }

    #[tokio::test]
    async fn dashboard_html_settings_removes_backup_panel_controls() {
        let html = include_str!("dashboard.html");
        assert!(!html.contains("id=\"settings-delete-selected-btn\""));
        assert!(!html.contains("id=\"settings-delete-all-btn\""));
        assert!(!html.contains("id=\"settings-backups-list\""));
    }

    #[tokio::test]
    async fn dashboard_html_settings_mismatch_callout_has_keep_actions() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("id=\"settings-mismatch-callout\""));
        assert!(html.contains("id=\"settings-keep-disk-btn\""));
        assert!(html.contains(">Keep disk<"));
        assert!(html.contains("id=\"settings-keep-db-snapshot-btn\""));
        assert!(html.contains(">Keep DB snapshot<"));
    }

    #[tokio::test]
    async fn dashboard_html_settings_mismatch_tracks_disk_and_db_snapshots() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("let settingsMismatchActive = false;"));
        assert!(html.contains("let settingsDiskLoadedSnapshot = null;"));
        assert!(html.contains("let settingsDbSnapshotCandidate = null;"));
        assert!(html.contains("editor.dataset.lastLoadedSnapshot = JSON.stringify(normalized);"));
        assert!(html.contains("settingsMismatchActive = payload?.data?.db_file_mismatch === true;"));
    }

    #[tokio::test]
    async fn dashboard_html_settings_keep_disk_reuses_apply_helper() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("async function applySettingsPayload("));
        assert!(html.contains("if (settingsApplyInFlight) return false;"));
        assert!(html.contains("const keepDiskBtn = document.getElementById('settings-keep-disk-btn');"));
        assert!(html.contains("const code = body?.error?.code || body?.code || null;"));
    }

    #[tokio::test]
    async fn dashboard_html_settings_keep_db_snapshot_is_editor_only() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("function keepDbSnapshotInEditor()"));
        assert!(html.contains("if (settingsApplyInFlight) return;"));
        assert!(html.contains("Loaded DB snapshot into editor (not yet applied)"));
        assert!(html.contains("// Do not update lastLoadedSnapshot; keep dirty."));
    }

    #[tokio::test]
    async fn dashboard_html_settings_keep_disk_applies_immediately() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("async function keepDiskAndApplyNow()"));
        assert!(html.contains("Kept disk settings and applied"));
        assert!(html.contains("Invalid JSON: cannot Keep disk until syntax is fixed"));
    }

    #[tokio::test]
    async fn dashboard_html_settings_mismatch_actions_are_wired() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("settings-keep-disk-btn')?.addEventListener('click', keepDiskAndApplyNow)"));
        assert!(html.contains("settings-keep-db-snapshot-btn')?.addEventListener('click', keepDbSnapshotInEditor)"));
        assert!(html.contains("function renderSettingsMismatchCallout()"));
        assert!(html.contains("renderSettingsMismatchCallout();"));
    }

    #[tokio::test]
    async fn settings_current_mismatch_persists_until_apply_reconciliation() {
        let root = std::env::temp_dir().join(format!(
            "claude-proxy-dashboard-mismatch-{}",
            uuid::Uuid::new_v4()
        ));
        let storage_dir = root.join("storage");
        let claude_dir = root.join("claude");
        std::fs::create_dir_all(&storage_dir).unwrap();
        std::fs::create_dir_all(&claude_dir).unwrap();

        let store = Arc::new(StatsStore::new(100, storage_dir, 20.0, 8.0, 2_097_152, claude_dir.clone()));
        let admin = crate::settings_admin::SettingsAdmin::new(store);

        // Apply initial settings to populate both DB and file
        admin.apply_settings(serde_json::json!({"theme": "dark"})).unwrap();

        // Mutate settings.json on disk directly
        let settings_path = claude_dir.join("settings.json");
        std::fs::write(&settings_path, serde_json::to_string_pretty(&serde_json::json!({"theme": "light", "extra": true})).unwrap()).unwrap();

        // GET current should detect mismatch
        let current = admin.get_current().unwrap().expect("should have current");
        assert!(current.db_file_mismatch, "should detect DB vs disk mismatch");
        assert!(!current.file_recreated_from_db);
        // claude_settings should be disk content
        assert_eq!(current.claude_settings.raw_json, serde_json::json!({"theme": "light", "extra": true}));

        // Apply the disk content to reconcile
        admin.apply_settings(serde_json::json!({"theme": "light", "extra": true})).unwrap();

        // GET current should now show no mismatch
        let current = admin.get_current().unwrap().expect("should have current");
        assert!(!current.db_file_mismatch, "mismatch should be resolved after apply");
    }

    #[tokio::test]
    async fn settings_current_file_recreated_from_db_when_file_missing() {
        let root = std::env::temp_dir().join(format!(
            "claude-proxy-dashboard-recreate-{}",
            uuid::Uuid::new_v4()
        ));
        let storage_dir = root.join("storage");
        let claude_dir = root.join("claude");
        std::fs::create_dir_all(&storage_dir).unwrap();
        std::fs::create_dir_all(&claude_dir).unwrap();

        let store = Arc::new(StatsStore::new(100, storage_dir, 20.0, 8.0, 2_097_152, claude_dir.clone()));
        let admin = crate::settings_admin::SettingsAdmin::new(store);

        // Apply settings to populate DB
        admin.apply_settings(serde_json::json!({"model": "opus"})).unwrap();

        // Delete settings.json
        let settings_path = claude_dir.join("settings.json");
        std::fs::remove_file(&settings_path).unwrap();
        assert!(!settings_path.exists());

        // GET current should recreate file from DB
        let current = admin.get_current().unwrap().expect("should have current");
        assert!(current.file_recreated_from_db, "should indicate file was recreated");
        assert!(!current.db_file_mismatch);
        assert_eq!(current.claude_settings.raw_json, serde_json::json!({"model": "opus"}));

        // File should now exist on disk
        assert!(settings_path.exists());
        let disk_content: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(disk_content, serde_json::json!({"model": "opus"}));
    }


    #[tokio::test]
    async fn dashboard_html_search_treats_missing_session_as_unknown() {
        let html = include_str!("dashboard.html");
        assert!(html.contains("e.session_id || 'unknown'"));
    }

    #[tokio::test]
    async fn dashboard_html_non_explicit_focus_clears_do_not_restore_snapshot() {
        let html = include_str!("dashboard.html");
        let expected = "clearAnomalyFocus({ restoreSnapshot: false, reload: false });";
        assert_eq!(html.matches(expected).count(), 4);
    }


    fn sample_entry() -> RequestEntry {
        RequestEntry {
            id: "dashboard-test-entry".into(),
            timestamp: chrono::Utc::now(),
            session_id: Some("session-1".into()),
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
            request_size_bytes: 10,
            response_size_bytes: 20,
            stalls: Vec::new(),
            error: None,
            anomalies: Vec::new(),
        }
    }


    #[tokio::test]
    async fn entries_endpoint_anomaly_focus_includes_metadata_and_preselection() {
        let log_dir = std::env::temp_dir().join(format!("claude-proxy-dashboard-anomaly-metadata-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = Arc::new(StatsStore::new(20, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone()));
        let anomaly_ts_ms = chrono::Utc::now().timestamp_millis();

        let mut nearest = sample_entry();
        nearest.id = "req-nearest".into();
        nearest.timestamp = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(anomaly_ts_ms + 150).unwrap();
        store.add_entry(nearest);

        let mut second = sample_entry();
        second.id = "req-second".into();
        second.timestamp = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(anomaly_ts_ms - 5_000).unwrap();
        store.add_entry(second);

        let mut outside_window = sample_entry();
        outside_window.id = "req-outside".into();
        outside_window.timestamp = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(anomaly_ts_ms + 130_000).unwrap();
        store.add_entry(outside_window);

        let app = build_dashboard_app(store);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/api/entries?anomaly_ts_ms={anomaly_ts_ms}&window_ms=120000"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();

        let entries = payload.get("entries").and_then(|v| v.as_array()).unwrap();
        assert_eq!(entries.len(), 2);

        let first_id = entries
            .first()
            .and_then(|entry| entry.get("id"))
            .and_then(|v| v.as_str())
            .unwrap();

        assert_eq!(
            payload.get("preselected_request_id").and_then(|v| v.as_str()),
            Some(first_id)
        );

        let anomaly_focus = payload.get("anomaly_focus").and_then(|v| v.as_object()).unwrap();
        assert_eq!(anomaly_focus.get("window_ms").and_then(|v| v.as_i64()), Some(120_000));

        for entry in entries {
            assert_eq!(entry.get("within_window").and_then(|v| v.as_bool()), Some(true));
            let distance = entry.get("distance_ms").and_then(|v| v.as_i64()).unwrap();
            assert!(distance <= 120_000);
        }

        let _ = std::fs::remove_dir_all(&log_dir);
    }
    #[tokio::test]
    async fn entries_endpoint_without_anomaly_focus_returns_null_focus() {
        let log_dir = std::env::temp_dir().join(format!("claude-proxy-dashboard-no-anomaly-envelope-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = Arc::new(StatsStore::new(10, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone()));
        store.add_entry(sample_entry());

        let app = build_dashboard_app(store);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/entries")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(payload.get("entries").and_then(|v| v.as_array()).is_some());
        assert!(payload.get("anomaly_focus").is_some());
        assert!(payload.get("anomaly_focus").unwrap().is_null());
        assert!(payload.get("preselected_request_id").is_some());
        assert!(payload.get("preselected_request_id").unwrap().is_null());

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[tokio::test]
    async fn entries_endpoint_invalid_anomaly_ts_falls_back_to_normal_envelope() {
        let log_dir = std::env::temp_dir().join(format!(
            "claude-proxy-dashboard-invalid-anomaly-ts-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = Arc::new(StatsStore::new(10, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone()));
        let mut entry = sample_entry();
        entry.id = "req-invalid-anomaly-fallback".into();
        store.add_entry(entry);

        let app = build_dashboard_app(store);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/entries?anomaly_ts_ms=not-a-number&window_ms=120000")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(payload.get("entries").and_then(|v| v.as_array()).is_some());
        assert!(payload.get("anomaly_focus").is_some_and(|v| v.is_null()));
        assert!(payload
            .get("preselected_request_id")
            .is_some_and(|v| v.is_null()));

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[tokio::test]
    async fn entry_body_endpoint_missing_request_returns_404() {
        let log_dir = std::env::temp_dir().join(format!("claude-proxy-dashboard-entry-body-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = Arc::new(StatsStore::new(10, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone()));
        let app = build_dashboard_app(store);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/entry-body?request_id=missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.get("error").and_then(|v| v.as_str()), Some("not found"));

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[tokio::test]
    async fn session_details_endpoint_unknown_session_includes_null_session_requests() {
        let log_dir = std::env::temp_dir().join(format!("claude-proxy-dashboard-session-details-unknown-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = Arc::new(StatsStore::new(10, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone()));
        let mut entry = sample_entry();
        entry.id = "req-unknown-session".into();
        entry.session_id = None;
        store.add_entry(entry);

        let app = build_dashboard_app(store);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/session-details?session_id=unknown")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.get("session_id").and_then(|v| v.as_str()), Some("unknown"));
        assert!(payload
            .get("requests")
            .and_then(|v| v.as_array())
            .is_some_and(|rows| !rows.is_empty()));

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[tokio::test]
    async fn delete_session_endpoint_live_session_returns_conflict() {
        let log_dir = std::env::temp_dir().join(format!("claude-proxy-dashboard-delete-session-live-{}", uuid::Uuid::new_v4()));
        let claude_dir = std::env::temp_dir().join(format!("claude-proxy-dashboard-delete-session-live-claude-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();
        std::fs::create_dir_all(&claude_dir).unwrap();

        let store = Arc::new(StatsStore::new(10, log_dir.clone(), 20.0, 8.0, 2_097_152, claude_dir.clone()));
        let mut pending = sample_entry();
        pending.id = "req-live-session".into();
        pending.session_id = Some("session-live".into());
        pending.status = RequestStatus::Pending;
        store.add_entry(pending);

        let app = build_dashboard_app(store);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/api/session")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"session_id":"session-live"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.get("blocked_live").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(payload.get("session_id").and_then(|v| v.as_str()), Some("session-live"));

        let _ = std::fs::remove_dir_all(&log_dir);
        let _ = std::fs::remove_dir_all(&claude_dir);
    }

    #[tokio::test]
    async fn delete_session_endpoint_missing_session_returns_not_found_result() {
        let log_dir = std::env::temp_dir().join(format!("claude-proxy-dashboard-delete-session-{}", uuid::Uuid::new_v4()));
        let claude_dir = std::env::temp_dir().join(format!("claude-proxy-dashboard-delete-session-claude-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();
        std::fs::create_dir_all(&claude_dir).unwrap();

        let store = Arc::new(StatsStore::new(10, log_dir.clone(), 20.0, 8.0, 2_097_152, claude_dir.clone()));
        let app = build_dashboard_app(store);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/api/session")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"session_id":"missing-session"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.get("not_found").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(payload.get("session_id").and_then(|v| v.as_str()), Some("missing-session"));

        let _ = std::fs::remove_dir_all(&log_dir);
        let _ = std::fs::remove_dir_all(&claude_dir);
    }

    #[tokio::test]
    async fn delete_session_endpoint_empty_session_id_returns_bad_request() {
        let log_dir = std::env::temp_dir().join(format!("claude-proxy-dashboard-delete-session-empty-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = Arc::new(StatsStore::new(10, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone()));
        let app = build_dashboard_app(store);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/api/session")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"session_id":""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.get("error").and_then(|v| v.as_str()), Some("session_id is required"));

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[tokio::test]
    async fn session_details_endpoint_empty_session_id_returns_bad_request() {
        let log_dir = std::env::temp_dir().join(format!("claude-proxy-dashboard-session-details-empty-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = Arc::new(StatsStore::new(10, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone()));
        let app = build_dashboard_app(store);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/session-details?session_id=")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.get("error").and_then(|v| v.as_str()), Some("session_id is required"));

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[tokio::test]
    async fn reset_memory_endpoint_clears_entries_but_keeps_persisted_data() {
        let log_dir = std::env::temp_dir().join(format!("claude-proxy-dashboard-memory-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = Arc::new(StatsStore::new(100, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone()));
        store.add_entry(sample_entry());
        assert_eq!(store.get_entries(10, 0, &EntryFilter::default(), None, None).len(), 1);
        assert_eq!(store.persisted_entry_count(), 1);

        let app = build_dashboard_app(store.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/reset-memory")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(store.get_live_stats().total_requests, 0);
        assert_eq!(store.get_entries(10, 0, &EntryFilter::default(), None, None).len(), 1);
        assert_eq!(store.persisted_entry_count(), 1);
        assert!(store.database_path().exists());

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[tokio::test]
    async fn reset_endpoint_clears_entries_and_database_rows() {
        let log_dir = std::env::temp_dir().join(format!("claude-proxy-dashboard-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = Arc::new(StatsStore::new(100, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone()));
        store.add_entry(sample_entry());
        assert_eq!(store.get_entries(10, 0, &EntryFilter::default(), None, None).len(), 1);
        assert_eq!(store.persisted_entry_count(), 1);

        let app = build_dashboard_app(store.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/reset")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(store.get_entries(10, 0, &EntryFilter::default(), None, None).is_empty());
        assert_eq!(store.persisted_entry_count(), 0);
        assert!(store.database_path().exists());

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[tokio::test]
    async fn stats_endpoint_returns_live_snapshot_by_default() {
        let log_dir = std::env::temp_dir().join(format!("claude-proxy-dashboard-stats-live-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = Arc::new(StatsStore::new(10, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone()));
        store.add_entry(sample_entry());

        let app = build_dashboard_app(store);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let snapshot: StatsSnapshot = serde_json::from_slice(&body).unwrap();

        assert_eq!(snapshot.mode, StatsMode::Live);
        assert_eq!(snapshot.stats.total_requests, 1);
        assert!(snapshot.coverage_start.is_some());

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[tokio::test]
    async fn stats_endpoint_returns_historical_snapshot() {
        let log_dir = std::env::temp_dir().join(format!("claude-proxy-dashboard-stats-historical-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = Arc::new(StatsStore::new(1, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone()));

        let mut first = sample_entry();
        first.id = "dashboard-live-1".into();
        first.timestamp = chrono::Utc::now() - chrono::Duration::seconds(2);
        store.add_entry(first);

        let mut second = sample_entry();
        second.id = "dashboard-live-2".into();
        second.timestamp = chrono::Utc::now() - chrono::Duration::seconds(1);
        second.status = RequestStatus::ClientError(403);
        second.error = Some("forbidden".into());
        store.add_entry(second);

        let app = build_dashboard_app(store);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/stats?mode=historical")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let snapshot: StatsSnapshot = serde_json::from_slice(&body).unwrap();

        assert_eq!(snapshot.mode, StatsMode::Historical);
        assert_eq!(snapshot.stats.total_requests, 2);
        assert_eq!(snapshot.stats.total_errors, 1);
        assert!(snapshot.coverage_start.is_some());
        assert!(snapshot.coverage_end.is_some());

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[tokio::test]
    async fn explanations_endpoint_returns_rows() {
        let log_dir = std::env::temp_dir().join(format!("claude-proxy-dashboard-explanations-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = Arc::new(StatsStore::new(10, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone()));

        let mut req = sample_entry();
        req.id = "req-1".into();
        req.session_id = Some("session-1".into());
        store.add_entry(req);

        store.upsert_explanation(&Explanation {
            id: "exp-1".into(),
            request_id: "req-1".into(),
            anomaly_kind: "slow_ttft".into(),
            rank: 1,
            confidence: 0.88,
            summary: "Model changed before TTFT spike".into(),
            evidence_json: serde_json::json!({"source":"config"}),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        });

        let app = build_dashboard_app(store);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/explanations?request_id=req-1&limit=5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let rows: Vec<Explanation> = serde_json::from_slice(&body).unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "exp-1");
        assert_eq!(rows[0].request_id, "req-1");

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[tokio::test]
    async fn timeline_endpoint_returns_ordered_events() {
        let log_dir = std::env::temp_dir().join(format!("claude-proxy-dashboard-timeline-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = Arc::new(StatsStore::new(10, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone()));

        let base_ts = chrono::Utc::now();

        let mut req = sample_entry();
        req.id = "req-timeline-1".into();
        req.session_id = Some("session-1".into());
        req.timestamp = base_ts;
        req.path = "/v1/messages".into();
        store.add_entry(req);

        store.upsert_local_event(&LocalEvent {
            id: "evt-timeline-1".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "projects/demo/session.json".into(),
            event_time_ms: (base_ts + chrono::Duration::milliseconds(500)).timestamp_millis(),
            session_hint: Some("session-1".into()),
            event_kind: "session_touch".into(),
            model_hint: None,
            payload_policy: crate::correlation::PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({}),
        });

        let app = build_dashboard_app(store);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/timeline?session_id=session-1&limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let items: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();

        assert_eq!(items.len(), 2);
        let first_ts = items[0].get("timestamp_ms").and_then(|v| v.as_i64()).unwrap();
        let second_ts = items[1].get("timestamp_ms").and_then(|v| v.as_i64()).unwrap();
        assert!(first_ts <= second_ts);

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[tokio::test]
    async fn session_graph_endpoint_returns_graph_payload() {
        let log_dir = std::env::temp_dir().join(format!("claude-proxy-dashboard-session-graph-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = Arc::new(StatsStore::new(20, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone()));

        let mut req = sample_entry();
        req.id = "req-graph-1".into();
        req.session_id = Some("s-1".into());
        store.add_entry(req);

        store.upsert_local_event(&LocalEvent {
            id: "evt-graph-1".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "projects/demo/session.json".into(),
            event_time_ms: chrono::Utc::now().timestamp_millis(),
            session_hint: Some("s-1".into()),
            event_kind: "session_touch".into(),
            model_hint: None,
            payload_policy: crate::correlation::PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({}),
        });

        store.upsert_request_correlation(&RequestCorrelation {
            id: "corr-graph-1".into(),
            request_id: "req-graph-1".into(),
            local_event_id: "evt-graph-1".into(),
            link_type: CorrelationLinkType::SessionHint,
            confidence: 0.9,
            reason: "session match".into(),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        });

        store.upsert_explanation(&Explanation {
            id: "exp-graph-1".into(),
            request_id: "req-graph-1".into(),
            anomaly_kind: "slow_ttft".into(),
            rank: 1,
            confidence: 0.8,
            summary: "Config drift likely".into(),
            evidence_json: serde_json::json!({}),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        });

        let app = build_dashboard_app(store);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/session-graph?session_id=s-1&limit=20")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let graph: SessionGraph = serde_json::from_slice(&body).unwrap();

        assert_eq!(graph.session_id, "s-1");
        assert!(graph.nodes.iter().any(|n| n.kind == "request"));
        assert!(!graph.edges.is_empty());

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[tokio::test]
    async fn correlations_endpoint_returns_links_for_request() {
        let log_dir = std::env::temp_dir().join(format!("claude-proxy-dashboard-correlations-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&log_dir).unwrap();

        let store = Arc::new(StatsStore::new(10, log_dir.clone(), 20.0, 8.0, 2_097_152, log_dir.clone()));

        let mut req_1 = sample_entry();
        req_1.id = "req-1".into();
        store.add_entry(req_1);

        let mut req_2 = sample_entry();
        req_2.id = "req-2".into();
        store.add_entry(req_2);

        store.upsert_local_event(&LocalEvent {
            id: "evt-1".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "project/a/session.json".into(),
            event_time_ms: chrono::Utc::now().timestamp_millis(),
            session_hint: Some("session-1".into()),
            event_kind: "session_touch".into(),
            model_hint: None,
            payload_policy: crate::correlation::PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({}),
        });

        store.upsert_local_event(&LocalEvent {
            id: "evt-2".into(),
            source_kind: SourceKind::ClaudeProject,
            source_path: "project/b/session.json".into(),
            event_time_ms: chrono::Utc::now().timestamp_millis(),
            session_hint: Some("session-1".into()),
            event_kind: "session_touch".into(),
            model_hint: None,
            payload_policy: crate::correlation::PayloadPolicy::MetadataOnly,
            payload_json: serde_json::json!({}),
        });

        store.upsert_request_correlation(&RequestCorrelation {
            id: "corr-1".into(),
            request_id: "req-1".into(),
            local_event_id: "evt-1".into(),
            link_type: CorrelationLinkType::SessionHint,
            confidence: 0.92,
            reason: "matched session hint".into(),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        });

        store.upsert_request_correlation(&RequestCorrelation {
            id: "corr-2".into(),
            request_id: "req-2".into(),
            local_event_id: "evt-2".into(),
            link_type: CorrelationLinkType::Temporal,
            confidence: 0.51,
            reason: "nearby timestamp".into(),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        });

        let app = build_dashboard_app(store);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/correlations?request_id=req-1&limit=5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let links: Vec<RequestCorrelation> = serde_json::from_slice(&body).unwrap();

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].id, "corr-1");
        assert_eq!(links[0].request_id, "req-1");

        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[tokio::test]
    async fn settings_history_endpoint_uses_q_search_param() {
        let root = std::env::temp_dir().join(format!(
            "claude-proxy-dashboard-settings-history-q-{}",
            uuid::Uuid::new_v4()
        ));
        let storage_dir = root.join("storage");
        let claude_dir = root.join("claude");
        std::fs::create_dir_all(&storage_dir).unwrap();
        std::fs::create_dir_all(&claude_dir).unwrap();

        let store = Arc::new(StatsStore::new(
            10,
            storage_dir.clone(),
            20.0,
            8.0,
            2_097_152,
            claude_dir,
        ));
        let admin = SettingsAdmin::new(store.clone());

        admin
            .apply_settings(serde_json::json!({
                "settings": {"note": "alpha beta"},
                "quick_tags": ["release"]
            }))
            .unwrap();
        admin
            .apply_settings(serde_json::json!({
                "settings": {"note": "other"},
                "quick_tags": ["staging"]
            }))
            .unwrap();

        let app = build_dashboard_app(store);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/settings/history?q=%20%20RELEASE%20%20")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(payload.get("ok").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            payload
                .get("data")
                .and_then(|v| v.as_array())
                .map(|rows| rows.len()),
            Some(1)
        );

        let _ = std::fs::remove_dir_all(&root);
    }


    #[tokio::test]
    async fn settings_history_endpoint_supports_legacy_search_param() {
        let root = std::env::temp_dir().join(format!(
            "claude-proxy-dashboard-settings-history-search-{}",
            uuid::Uuid::new_v4()
        ));
        let storage_dir = root.join("storage");
        let claude_dir = root.join("claude");
        std::fs::create_dir_all(&storage_dir).unwrap();
        std::fs::create_dir_all(&claude_dir).unwrap();

        let store = Arc::new(StatsStore::new(
            10,
            storage_dir,
            20.0,
            8.0,
            2_097_152,
            claude_dir,
        ));
        let admin = SettingsAdmin::new(store.clone());

        admin
            .apply_settings(serde_json::json!({
                "settings": {"note": "alpha beta"},
                "quick_tags": ["release"]
            }))
            .unwrap();
        admin
            .apply_settings(serde_json::json!({
                "settings": {"note": "other"},
                "quick_tags": ["staging"]
            }))
            .unwrap();

        let app = build_dashboard_app(store);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/settings/history?search=%20%20RELEASE%20%20")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(payload.get("ok").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            payload
                .get("data")
                .and_then(|v| v.as_array())
                .map(|rows| rows.len()),
            Some(1)
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn settings_history_endpoint_prefers_q_over_legacy_search_when_both_present() {
        let root = std::env::temp_dir().join(format!(
            "claude-proxy-dashboard-settings-history-q-preferred-{}",
            uuid::Uuid::new_v4()
        ));
        let storage_dir = root.join("storage");
        let claude_dir = root.join("claude");
        std::fs::create_dir_all(&storage_dir).unwrap();
        std::fs::create_dir_all(&claude_dir).unwrap();

        let store = Arc::new(StatsStore::new(
            10,
            storage_dir,
            20.0,
            8.0,
            2_097_152,
            claude_dir,
        ));
        let admin = SettingsAdmin::new(store.clone());

        admin
            .apply_settings(serde_json::json!({
                "settings": {"note": "alpha beta"},
                "quick_tags": ["release"]
            }))
            .unwrap();
        admin
            .apply_settings(serde_json::json!({
                "settings": {"note": "other"},
                "quick_tags": ["staging"]
            }))
            .unwrap();

        let app = build_dashboard_app(store);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/settings/history?q=staging&search=release")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(payload.get("ok").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            payload
                .get("data")
                .and_then(|v| v.as_array())
                .map(|rows| rows.len()),
            Some(1)
        );
        let tags = payload
            .get("data")
            .and_then(|v| v.as_array())
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("tags"))
            .and_then(|tags| tags.as_array())
            .map(|tags| {
                tags.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        assert!(tags.contains(&"staging"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn settings_history_tags_patch_missing_revision_uses_not_found_envelope() {
        let root = std::env::temp_dir().join(format!(
            "claude-proxy-dashboard-settings-tags-missing-{}",
            uuid::Uuid::new_v4()
        ));
        let storage_dir = root.join("storage");
        let claude_dir = root.join("claude");
        std::fs::create_dir_all(&storage_dir).unwrap();
        std::fs::create_dir_all(&claude_dir).unwrap();

        let store = Arc::new(StatsStore::new(
            10,
            storage_dir,
            20.0,
            8.0,
            2_097_152,
            claude_dir,
        ));
        let app = build_dashboard_app(store);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri("/api/settings/history/999999/tags")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"add":["alpha"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.get("ok").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(
            payload
                .get("error")
                .and_then(|v| v.get("code"))
                .and_then(|v| v.as_str()),
            Some("not_found")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn settings_history_delete_all_requires_confirm_true_payload() {
        let root = std::env::temp_dir().join(format!(
            "claude-proxy-dashboard-settings-history-delete-all-confirm-{}",
            uuid::Uuid::new_v4()
        ));
        let storage_dir = root.join("storage");
        let claude_dir = root.join("claude");
        std::fs::create_dir_all(&storage_dir).unwrap();
        std::fs::create_dir_all(&claude_dir).unwrap();

        let store = Arc::new(StatsStore::new(
            10,
            storage_dir,
            20.0,
            8.0,
            2_097_152,
            claude_dir,
        ));

        let app = build_dashboard_app(store);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/api/settings/history")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.get("ok").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(
            payload
                .get("error")
                .and_then(|v| v.get("code"))
                .and_then(|v| v.as_str()),
            Some("invalid_payload")
        );
        assert_eq!(
            payload
                .get("error")
                .and_then(|v| v.get("message"))
                .and_then(|v| v.as_str()),
            Some("confirm=true is required")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn settings_history_delete_all_on_empty_history_returns_zero_deleted_count() {
        let root = std::env::temp_dir().join(format!(
            "claude-proxy-dashboard-settings-history-delete-all-empty-{}",
            uuid::Uuid::new_v4()
        ));
        let storage_dir = root.join("storage");
        let claude_dir = root.join("claude");
        std::fs::create_dir_all(&storage_dir).unwrap();
        std::fs::create_dir_all(&claude_dir).unwrap();

        let store = Arc::new(StatsStore::new(
            10,
            storage_dir,
            20.0,
            8.0,
            2_097_152,
            claude_dir,
        ));

        let app = build_dashboard_app(store);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/api/settings/history")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"confirm":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload.get("ok").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            payload
                .get("data")
                .and_then(|v| v.get("deleted_count"))
                .and_then(|v| v.as_i64()),
            Some(0)
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn settings_backup_endpoints_return_deterministic_410_with_guidance_payload() {
        let root = std::env::temp_dir().join(format!(
            "claude-proxy-dashboard-settings-backup-gone-{}",
            uuid::Uuid::new_v4()
        ));
        let storage_dir = root.join("storage");
        let claude_dir = root.join("claude");
        std::fs::create_dir_all(&storage_dir).unwrap();
        std::fs::create_dir_all(&claude_dir).unwrap();

        let store = Arc::new(StatsStore::new(
            10,
            storage_dir,
            20.0,
            8.0,
            2_097_152,
            claude_dir,
        ));

        let app = build_dashboard_app(store);

        let get_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/settings/backups")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(get_response.status(), StatusCode::GONE);
        let get_body = axum::body::to_bytes(get_response.into_body(), usize::MAX).await.unwrap();
        let get_payload: serde_json::Value = serde_json::from_slice(&get_body).unwrap();
        assert_eq!(get_payload.get("ok").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(
            get_payload
                .get("error")
                .and_then(|v| v.get("code"))
                .and_then(|v| v.as_str()),
            Some("gone")
        );
        assert_eq!(
            get_payload
                .get("error")
                .and_then(|v| v.get("details"))
                .and_then(|v| v.get("replacement"))
                .and_then(|v| v.as_str()),
            Some("/api/settings/history")
        );

        let delete_selected_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/settings/backups/delete-selected")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"backup_ids":["a.json"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(delete_selected_response.status(), StatusCode::GONE);

        let delete_all_response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/settings/backups/delete-all")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"confirm":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(delete_all_response.status(), StatusCode::GONE);

        let _ = std::fs::remove_dir_all(&root);
    }
    #[tokio::test]
    async fn settings_history_endpoint_returns_numeric_revision_id() {
        let root = std::env::temp_dir().join(format!(
            "claude-proxy-dashboard-settings-history-revision-id-{}",
            uuid::Uuid::new_v4()
        ));
        let storage_dir = root.join("storage");
        let claude_dir = root.join("claude");
        std::fs::create_dir_all(&storage_dir).unwrap();
        std::fs::create_dir_all(&claude_dir).unwrap();

        let store = Arc::new(StatsStore::new(
            10,
            storage_dir,
            20.0,
            8.0,
            2_097_152,
            claude_dir,
        ));
        let admin = SettingsAdmin::new(store.clone());
        admin
            .apply_settings(serde_json::json!({
                "settings": {"note": "revision-id-check"},
                "quick_tags": ["release"]
            }))
            .unwrap();

        let app = build_dashboard_app(store);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/settings/history?limit=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();

        let first_row = payload
            .get("data")
            .and_then(|v| v.as_array())
            .and_then(|rows| rows.first())
            .expect("expected at least one history row");

        let revision_id = first_row
            .get("revision_id")
            .and_then(|v| v.as_i64())
            .expect("revision_id should be numeric");
        assert!(revision_id > 0);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn settings_history_delete_revision_endpoint_accepts_numeric_revision_id() {
        let root = std::env::temp_dir().join(format!(
            "claude-proxy-dashboard-settings-history-delete-revision-id-{}",
            uuid::Uuid::new_v4()
        ));
        let storage_dir = root.join("storage");
        let claude_dir = root.join("claude");
        std::fs::create_dir_all(&storage_dir).unwrap();
        std::fs::create_dir_all(&claude_dir).unwrap();

        let store = Arc::new(StatsStore::new(
            10,
            storage_dir,
            20.0,
            8.0,
            2_097_152,
            claude_dir,
        ));
        let admin = SettingsAdmin::new(store.clone());
        admin
            .apply_settings(serde_json::json!({
                "settings": {"note": "delete-revision-id-check"}
            }))
            .unwrap();

        let app = build_dashboard_app(store.clone());
        let history_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/settings/history?limit=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(history_response.status(), StatusCode::OK);
        let history_body = axum::body::to_bytes(history_response.into_body(), usize::MAX).await.unwrap();
        let history_payload: serde_json::Value = serde_json::from_slice(&history_body).unwrap();
        let revision_id = history_payload
            .get("data")
            .and_then(|v| v.as_array())
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("revision_id"))
            .and_then(|v| v.as_i64())
            .expect("expected numeric revision_id");

        let delete_response = app
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri(format!("/api/settings/history/{revision_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(delete_response.status(), StatusCode::OK);
        let delete_body = axum::body::to_bytes(delete_response.into_body(), usize::MAX).await.unwrap();
        let delete_payload: serde_json::Value = serde_json::from_slice(&delete_body).unwrap();
        assert_eq!(delete_payload.get("ok").and_then(|v| v.as_bool()), Some(true));

        let _ = std::fs::remove_dir_all(&root);
    }
}
