use crate::stats::*;
use axum::{
    body::Body,
    extract::State,
    http::{
        header::{CONNECTION, CONTENT_ENCODING, CONTENT_TYPE, TRANSFER_ENCODING},
        HeaderMap, Method, Uri,
    },
    response::IntoResponse,
    Router,
};
use chrono::Utc;
use futures_util::StreamExt;
use std::sync::Arc;
use std::time::Instant;

pub async fn run_proxy(store: Arc<StatsStore>, target: &str, port: u16) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .connect_timeout(std::time::Duration::from_secs(30))
        .pool_max_idle_per_host(10)
        .danger_accept_invalid_certs(true)
        .no_proxy()
        .build()
        .expect("Failed to create HTTP client");

    let state = ProxyState {
        client,
        target: target.to_string(),
        store,
    };

    let app = Router::new()
        .fallback(proxy_handler)
        .with_state(Arc::new(state));

    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::AddrInUse {
                format!(
                    "Proxy port {port} is already in use. Stop the existing process or run with --port <free-port>."
                )
            } else {
                format!("Failed to bind proxy port {port}: {err}")
            }
        })?;

    axum::serve(listener, app)
        .await
        .map_err(|err| format!("Proxy server stopped unexpectedly: {err}"))
}

#[derive(Clone)]
struct ProxyState {
    client: reqwest::Client,
    target: String,
    store: Arc<StatsStore>,
}

async fn proxy_handler(
    State(state): State<Arc<ProxyState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> impl IntoResponse {
    let start = Instant::now();
    let path = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
    let target_url = format!("{}{}", state.target, path);

    // Read request body
    let body_bytes = match axum::body::to_bytes(body, 50 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return axum::http::Response::builder()
                .status(400)
                .body(Body::from(format!("Failed to read request body: {e}")))
                .unwrap();
        }
    };

    let request_size = body_bytes.len() as u64;

    // Parse request body for metadata
    let (model, stream, session_id) = parse_request_body(&body_bytes);

    // Build entry skeleton
    let entry_id = uuid::Uuid::new_v4().to_string();
    let mut entry = RequestEntry {
        id: entry_id,
        timestamp: Utc::now(),
        session_id,
        method: method.to_string(),
        path: path.to_string(),
        model,
        stream,
        status: RequestStatus::Pending,
        duration_ms: 0.0,
        ttft_ms: None,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_creation_tokens: None,
        thinking_tokens: None,
        stop_reason: None,
        request_size_bytes: request_size,
        response_size_bytes: 0,
        stalls: Vec::new(),
        error: None,
        anomalies: Vec::new(),
    };

    // Build outgoing request
    let mut req = state.client.request(method.clone(), &target_url);

    // Forward headers (skip hop-by-hop)
    for (key, value) in headers.iter() {
        let k = key.as_str().to_lowercase();
        if !matches!(
            k.as_str(),
            "host" | "content-length" | "transfer-encoding" | "connection"
        ) {
            req = req.header(key.clone(), value.clone());
        }
    }

    req = req.body(body_bytes.to_vec());

    // Send request
    let response = match req.send().await {
        Ok(resp) => resp,
        Err(e) => {
            entry.duration_ms = start.elapsed().as_secs_f64() * 1000.0;
            if e.is_timeout() {
                entry.status = RequestStatus::Timeout;
                entry.error = Some("Request timed out".into());
            } else if e.is_connect() {
                entry.status = RequestStatus::ConnectionError;
                entry.error = Some(format!("Connection failed: {e}"));
            } else {
                entry.status = RequestStatus::ProxyError;
                entry.error = Some(format!("{e}"));
            }
            state.store.add_entry(entry);
            return axum::http::Response::builder()
                .status(502)
                .body(Body::from("Bad Gateway"))
                .unwrap();
        }
    };

    let status_code = response.status().as_u16();
    entry.ttft_ms = Some(start.elapsed().as_secs_f64() * 1000.0);
    entry.status = RequestStatus::from_code(status_code);
    let upstream_headers = response.headers().clone();

    // Build response headers
    let resp_headers = build_response_headers(&upstream_headers);

    let content_type = upstream_headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let is_sse = stream && content_type.contains("text/event-stream");

    // Error responses (non-streaming)
    if status_code >= 400 {
        let resp_body = match response.bytes().await {
            Ok(b) => b,
            Err(e) => {
                entry.duration_ms = start.elapsed().as_secs_f64() * 1000.0;
                entry.error = Some(format!("Failed to read error response: {e}"));
                state.store.add_entry(entry);
                return axum::http::Response::builder()
                    .status(502)
                    .body(Body::from("Bad Gateway"))
                    .unwrap();
            }
        };

        entry.duration_ms = start.elapsed().as_secs_f64() * 1000.0;
        entry.response_size_bytes = resp_body.len() as u64;
        entry.error = Some(summarize_error_body(&upstream_headers, &resp_body));

        let entry_id_for_body = entry.id.clone();
        state.store.add_entry(entry);
        state.store.write_body(
            &entry_id_for_body,
            &String::from_utf8_lossy(&body_bytes),
            &String::from_utf8_lossy(&resp_body),
        );

        let mut resp = axum::http::Response::builder().status(status_code);
        for (k, v) in &resp_headers {
            resp = resp.header(k, v);
        }
        return resp.body(Body::from(resp_body)).unwrap();
    }

    // Streaming response (SSE)
    if is_sse {
        let stall_threshold = state.store.stall_threshold;
        let store = state.store.clone();
        let req_body_for_store = String::from_utf8_lossy(&body_bytes).to_string();

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(256);

        // Spawn stream reader
        tokio::spawn(async move {
            let mut stream = response.bytes_stream();
            let mut last_chunk_time = Instant::now();
            let mut total_bytes = 0u64;
            let mut stalls = Vec::new();
            let mut usage_data = UsageData::default();
            let mut stop_reason = None;
            let mut response_buffer = Vec::new();
            let mut sse_line_buffer = String::new();

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        let now = Instant::now();
                        let gap = (now - last_chunk_time).as_secs_f64();

                        if gap > stall_threshold {
                            stalls.push(StallEvent {
                                timestamp: Utc::now(),
                                duration_s: gap,
                                bytes_before: total_bytes,
                            });
                        }

                        last_chunk_time = now;
                        total_bytes += chunk.len() as u64;

                        // Extract usage and metadata from complete SSE lines only,
                        // preserving partial lines across chunk boundaries.
                        extract_sse_usage_and_metadata(
                            &chunk,
                            &mut sse_line_buffer,
                            &mut usage_data,
                            &mut stop_reason,
                        );

                        response_buffer.extend_from_slice(&chunk);
                        if tx.send(Ok(chunk)).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(std::io::Error::other(e))).await;
                        break;
                    }
                }
            }

            // Finalize entry
            let mut final_entry = entry;
            final_entry.duration_ms = start.elapsed().as_secs_f64() * 1000.0;
            final_entry.response_size_bytes = total_bytes;
            final_entry.stalls = stalls;
            apply_stream_usage_and_metadata(&mut final_entry, &usage_data, stop_reason);

            if !final_entry.stalls.is_empty() {
                let total_stall: f64 = final_entry.stalls.iter().map(|s| s.duration_s).sum();
                final_entry.error = Some(format!(
                    "{} stall(s), {:.1}s total",
                    final_entry.stalls.len(),
                    total_stall
                ));
            }

            let final_entry_id = final_entry.id.clone();
            store.add_entry(final_entry);
            let resp_body_str = String::from_utf8_lossy(&response_buffer).to_string();
            store.write_body(&final_entry_id, &req_body_for_store, &resp_body_str);
        });

        let body_stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        let body = Body::from_stream(body_stream);

        let mut resp = axum::http::Response::builder().status(status_code);
        for (k, v) in &resp_headers {
            resp = resp.header(k, v);
        }
        return resp.body(body).unwrap();
    }

    // Non-streaming response
    let resp_body = match response.bytes().await {
        Ok(b) => b,
        Err(e) => {
            entry.duration_ms = start.elapsed().as_secs_f64() * 1000.0;
            entry.error = Some(format!("Failed to read response: {e}"));
            state.store.add_entry(entry);
            return axum::http::Response::builder()
                .status(502)
                .body(Body::from("Bad Gateway"))
                .unwrap();
        }
    };

    entry.duration_ms = start.elapsed().as_secs_f64() * 1000.0;
    entry.response_size_bytes = resp_body.len() as u64;

    // Extract usage from JSON response
    if let Ok(resp_json) = serde_json::from_slice::<serde_json::Value>(&resp_body) {
        if let Some(usage) = resp_json.get("usage") {
            entry.input_tokens = usage.get("input_tokens").and_then(|v| v.as_u64());
            entry.output_tokens = usage.get("output_tokens").and_then(|v| v.as_u64());
            entry.cache_read_tokens = usage
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64());
            entry.cache_creation_tokens = usage
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64());
        }
    }

    let entry_id_for_body = entry.id.clone();
    state.store.add_entry(entry);
    state.store.write_body(
        &entry_id_for_body,
        &String::from_utf8_lossy(&body_bytes),
        &String::from_utf8_lossy(&resp_body),
    );

    let mut resp = axum::http::Response::builder().status(status_code);
    for (k, v) in &resp_headers {
        resp = resp.header(k, v);
    }
    resp.body(Body::from(resp_body)).unwrap()
}

// ─── Helpers ───

fn parse_request_body(body: &[u8]) -> (String, bool, Option<String>) {
    let mut model = String::new();
    let mut stream = false;
    let mut session_id = None;

    if let Ok(json) = serde_json::from_slice::<serde_json::Value>(body) {
        model = json
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        stream = json
            .get("stream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Try to extract session info from metadata
        if let Some(meta) = json.get("metadata") {
            session_id = meta
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }
    }

    (model, stream, session_id)
}

#[derive(Default)]
struct UsageData {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
    cache_creation_tokens: Option<u64>,
    thinking_tokens: Option<u64>,
}

fn apply_stream_usage_and_metadata(
    final_entry: &mut RequestEntry,
    usage_data: &UsageData,
    stop_reason: Option<String>,
) {
    final_entry.input_tokens = usage_data.input_tokens;
    final_entry.output_tokens = usage_data.output_tokens;
    final_entry.cache_read_tokens = usage_data.cache_read_tokens;
    final_entry.cache_creation_tokens = usage_data.cache_creation_tokens;
    final_entry.thinking_tokens = usage_data.thinking_tokens;
    final_entry.stop_reason = stop_reason;
}

fn extract_sse_usage_and_metadata(
    chunk: &[u8],
    line_buffer: &mut String,
    usage: &mut UsageData,
    stop_reason: &mut Option<String>,
) {
    let text = match std::str::from_utf8(chunk) {
        Ok(t) => t,
        Err(_) => return,
    };

    process_sse_text_chunk(text, line_buffer, usage, stop_reason);
}

fn process_sse_text_chunk(
    chunk_text: &str,
    line_buffer: &mut String,
    usage: &mut UsageData,
    stop_reason: &mut Option<String>,
) {
    process_sse_text_chunk_with_stats(chunk_text, line_buffer, usage, stop_reason, None);
}

fn process_sse_text_chunk_with_stats(
    chunk_text: &str,
    line_buffer: &mut String,
    usage: &mut UsageData,
    stop_reason: &mut Option<String>,
    unknown_stats: Option<&mut crate::types::UnknownFieldStats>,
) {
    line_buffer.push_str(chunk_text);

    // We need to track the stats mutably across iterations
    let mut stats = unknown_stats;

    while let Some(newline_idx) = line_buffer.find('\n') {
        let mut line = line_buffer[..newline_idx].to_string();
        if line.ends_with('\r') {
            line.pop();
        }

        // Check for SSE event type lines ("event: <type>")
        if let Some(event_type) = line.strip_prefix("event: ") {
            if let Some(ref mut s) = stats {
                s.record_unknown_event(event_type.trim());
            }
        }

        process_sse_line(&line, usage, stop_reason);
        line_buffer.drain(..=newline_idx);
    }
}

fn process_sse_line(line: &str, usage: &mut UsageData, stop_reason: &mut Option<String>) {
    if !line.starts_with("data: ") {
        return;
    }

    if let Ok(data) = serde_json::from_str::<serde_json::Value>(&line[6..]) {
        if let Some(u) = data.get("usage") {
            if let Some(v) = u.get("input_tokens").and_then(|v| v.as_u64()) {
                usage.input_tokens = Some(v);
            }
            if let Some(v) = u.get("output_tokens").and_then(|v| v.as_u64()) {
                usage.output_tokens = Some(v);
            }
            if let Some(v) = u.get("cache_read_input_tokens").and_then(|v| v.as_u64()) {
                usage.cache_read_tokens = Some(v);
            }
            if let Some(v) = u
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
            {
                usage.cache_creation_tokens = Some(v);
            }
            if let Some(v) = u.get("thinking_tokens").and_then(|v| v.as_u64()) {
                usage.thinking_tokens = Some(v);
            }
        }

        if let Some(reason) = data.get("stop_reason").and_then(|v| v.as_str()) {
            *stop_reason = Some(reason.to_string());
        }
    }
}

fn build_response_headers(source: &HeaderMap) -> HeaderMap {
    let mut headers = HeaderMap::new();

    for (key, value) in source.iter() {
        if key != TRANSFER_ENCODING && key != CONNECTION {
            headers.insert(key.clone(), value.clone());
        }
    }

    headers
}

fn summarize_error_body(headers: &HeaderMap, body: &[u8]) -> String {
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");

    let content_encoding = headers
        .get(CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .unwrap_or("");

    if !content_encoding.is_empty() && !content_encoding.eq_ignore_ascii_case("identity") {
        return format!(
            "[{}-encoded {} response body, {} bytes]",
            content_encoding,
            describe_content_type(content_type),
            body.len()
        );
    }

    if looks_textual_content_type(content_type) {
        return truncate_for_log(&compact_whitespace(&String::from_utf8_lossy(body)), 500);
    }

    match std::str::from_utf8(body) {
        Ok(text) => truncate_for_log(&compact_whitespace(text), 500),
        Err(_) => format!(
            "[binary {} response body, {} bytes]",
            describe_content_type(content_type),
            body.len()
        ),
    }
}

fn looks_textual_content_type(content_type: &str) -> bool {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    mime.starts_with("text/")
        || mime.ends_with("+json")
        || mime.ends_with("+xml")
        || matches!(
            mime.as_str(),
            "application/json"
                | "application/xml"
                | "application/javascript"
                | "application/x-www-form-urlencoded"
                | "application/problem+json"
        )
}

fn describe_content_type(content_type: &str) -> String {
    let mime = content_type.split(';').next().unwrap_or("").trim();

    if mime.is_empty() {
        "response".to_string()
    } else {
        mime.to_string()
    }
}

fn compact_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_for_log(text: &str, max_chars: usize) -> String {
    let mut truncated = String::new();
    let mut chars = text.chars();

    for _ in 0..max_chars {
        match chars.next() {
            Some(ch) => truncated.push(ch),
            None => return truncated,
        }
    }

    if chars.next().is_some() {
        truncated.push('…');
    }

    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn build_response_headers_preserves_content_encoding() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(TRANSFER_ENCODING, HeaderValue::from_static("chunked"));

        let copied = build_response_headers(&headers);

        assert_eq!(copied.get(CONTENT_ENCODING).unwrap(), "gzip");
        assert_eq!(copied.get(CONTENT_TYPE).unwrap(), "application/json");
        assert!(copied.get(TRANSFER_ENCODING).is_none());
    }

    #[test]
    fn summarize_error_body_marks_compressed_payloads() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let summary = summarize_error_body(&headers, &[0x1f, 0x8b, 0x08, 0x00]);

        assert_eq!(
            summary,
            "[gzip-encoded application/json response body, 4 bytes]"
        );
    }

    #[test]
    fn summarize_error_body_compacts_text_payloads() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let summary = summarize_error_body(&headers, b"{\n  \"error\": \"forbidden\"\n}\n");

        assert_eq!(summary, "{ \"error\": \"forbidden\" }");
    }

    #[test]
    fn extract_sse_usage_captures_thinking_tokens_and_stop_reason() {
        let chunk = b"data: {\"type\":\"message_delta\",\"delta\":{},\"usage\":{\"input_tokens\":10,\"output_tokens\":20,\"cache_read_input_tokens\":5,\"cache_creation_input_tokens\":2,\"thinking_tokens\":7},\"stop_reason\":\"end_turn\"}\n\n";

        let mut usage = UsageData::default();
        let mut stop_reason = None;
        let mut line_buffer = String::new();
        extract_sse_usage_and_metadata(chunk, &mut line_buffer, &mut usage, &mut stop_reason);

        assert_eq!(usage.thinking_tokens, Some(7));
        assert_eq!(stop_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn extract_sse_usage_handles_fragmented_data_line_across_chunks() {
        let chunk1 = b"data: {\"type\":\"message_delta\",\"usage\":{\"thinking_tokens\":";
        let chunk2 = b"9},\"stop_reason\":\"end_turn\"}\n\n";

        let mut usage = UsageData::default();
        let mut stop_reason = None;
        let mut line_buffer = String::new();

        extract_sse_usage_and_metadata(chunk1, &mut line_buffer, &mut usage, &mut stop_reason);
        assert_eq!(usage.thinking_tokens, None);
        assert_eq!(stop_reason, None);

        extract_sse_usage_and_metadata(chunk2, &mut line_buffer, &mut usage, &mut stop_reason);
        assert_eq!(usage.thinking_tokens, Some(9));
        assert_eq!(stop_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn apply_stream_usage_and_metadata_sets_stop_reason_on_final_entry() {
        let mut entry = RequestEntry {
            id: "req-stop-reason".to_string(),
            timestamp: Utc::now(),
            session_id: Some("session-1".to_string()),
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            model: "claude-test".to_string(),
            stream: true,
            status: RequestStatus::Success(200),
            duration_ms: 10.0,
            ttft_ms: Some(1.0),
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            thinking_tokens: None,
            stop_reason: None,
            request_size_bytes: 10,
            response_size_bytes: 20,
            stalls: Vec::new(),
            error: None,
            anomalies: Vec::new(),
        };

        let usage = UsageData {
            input_tokens: Some(11),
            output_tokens: Some(22),
            cache_read_tokens: Some(3),
            cache_creation_tokens: Some(4),
            thinking_tokens: Some(5),
        };

        apply_stream_usage_and_metadata(&mut entry, &usage, Some("end_turn".to_string()));

        assert_eq!(entry.thinking_tokens, Some(5));
        assert_eq!(entry.stop_reason.as_deref(), Some("end_turn"));
    }
}
