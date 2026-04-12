mod analyzer;
mod correlation;
mod correlation_engine;
mod dashboard;
mod explainer;
mod local_context;
mod model_profile;
mod proxy;
mod session_admin;
mod settings_admin;
mod stats;
mod store;
mod types;

use analyzer::AnalyzerRules;
use clap::Parser;
use stats::StatsStore;
use std::path::PathBuf;
use std::sync::Arc;
use store::Store;
use tokio::time::{interval, Duration};

#[derive(Parser, Debug, Clone)]
#[command(name = "claude-proxy", about = "Ultra-fast API logging proxy for Claude Code")]
struct Args {
    #[arg(long)]
    target: String,

    #[arg(long, default_value_t = 8000)]
    port: u16,

    #[arg(long, default_value_t = 3000)]
    dashboard_port: u16,

    #[arg(long)]
    data_dir: Option<PathBuf>,

    #[arg(long)]
    model_config: Option<PathBuf>,

    #[arg(long, default_value_t = 0.5)]
    stall_threshold: f64,

    #[arg(long, default_value_t = 3000)]
    slow_ttft_threshold: u64,

    #[arg(long, default_value_t = 2 * 1024 * 1024)]
    max_body_size: usize,

    #[arg(long, default_value_t = false, action = clap::ArgAction::SetTrue)]
    open_browser: bool,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    let analyzer_rules = AnalyzerRules {
        slow_ttft_threshold_ms: args.slow_ttft_threshold as f64,
        stall_threshold_s: args.stall_threshold,
    };

    if let Some(model_config_path) = &args.model_config {
        eprintln!(
            "--model-config is not wired in this phase yet: {}",
            model_config_path.display()
        );
        std::process::exit(2);
    }

    let target = args.target.trim_end_matches('/').to_string();
    let data_dir = args.data_dir.unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_default()
            .join(".claude")
            .join("api-logs")
    });
    let _ = std::fs::create_dir_all(&data_dir);

    let claude_root = dirs::home_dir().unwrap_or_default().join(".claude");

    let store = Arc::new(StatsStore::new(
        50_000,
        data_dir.clone(),
        args.stall_threshold,
        (args.slow_ttft_threshold as f64) / 1000.0,
        args.max_body_size,
        claude_root,
    ));

    store.load_from_db();

    if let Ok(analyzer_store) = Store::new(&data_dir.join("proxy-v2.db")) {
        let analyzer_store = Arc::new(analyzer_store);
        let worker_rules = analyzer_rules.clone();
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(5));
            loop {
                ticker.tick().await;
                if let Err(err) = run_analyzer_tick_with_rules(analyzer_store.clone(), &worker_rules).await {
                    eprintln!("Analyzer tick failed: {err}");
                }
            }
        });
    }

    print_banner(
        &target,
        args.port,
        args.dashboard_port,
        &data_dir,
        store.database_path(),
    );

    let dashboard_url = format!("http://127.0.0.1:{}", args.dashboard_port);

    let store_dash = store.clone();
    let dash_port = args.dashboard_port;
    tokio::spawn(async move {
        if let Err(err) = dashboard::run_dashboard(store_dash, dash_port).await {
            eprintln!("Dashboard startup failed: {err}");
        }
    });

    if args.open_browser {
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        let _ = open::that(&dashboard_url);
    }

    if let Err(err) = proxy::run_proxy(store, &target, args.port).await {
        eprintln!("Proxy startup failed: {err}");
        std::process::exit(1);
    }
}

pub async fn run_analyzer_tick(store: Arc<Store>) -> Result<(), rusqlite::Error> {
    let rules = AnalyzerRules {
        slow_ttft_threshold_ms: 3000.0,
        stall_threshold_s: 0.5,
    };
    run_analyzer_tick_with_rules(store, &rules).await
}

async fn run_analyzer_tick_with_rules(
    store: Arc<Store>,
    rules: &AnalyzerRules,
) -> Result<(), rusqlite::Error> {
    let pending = store.list_unanalyzed_requests(200)?;

    for req in pending {
        let recent = store.list_recent_requests_for_model(&req.model, 50)?;
        let anomalies = analyzer::detect_anomalies(&req, rules, &recent);
        store.insert_anomalies(&req.id, &anomalies)?;
        store.mark_analyzed(&req.id)?;

        let sample_count = store.increment_model_sample_count(&req.model)?;
        if model_profile::should_auto_tune(sample_count) {
            let observed = store.compute_model_observed_stats(&req.model)?;
            store.upsert_model_observed(&req.model, &observed)?;
        }
    }

    Ok(())
}

fn print_banner(
    target: &str,
    proxy_port: u16,
    dash_port: u16,
    storage_dir: &std::path::Path,
    db_path: &std::path::Path,
) {
    let cyan = "\x1b[96m";
    let bold = "\x1b[1m";
    let dim = "\x1b[2m";
    let yellow = "\x1b[93m";
    let green = "\x1b[92m";
    let reset = "\x1b[0m";

    println!();
    println!("  {bold}══════════════════════════════════════════════════{reset}");
    println!("  {bold}{cyan}  Claude Code Proxy — Ultra-Fast API Monitor{reset}");
    println!("  {bold}══════════════════════════════════════════════════{reset}");
    println!();
    println!("  {green}▸{reset} Target API:    {bold}{target}{reset}");
    println!("  {green}▸{reset} Proxy:         {bold}http://127.0.0.1:{proxy_port}{reset}");
    println!("  {green}▸{reset} Dashboard:     {bold}http://127.0.0.1:{dash_port}{reset}");
    println!("  {green}▸{reset} Storage dir:   {dim}{}{reset}", storage_dir.display());
    println!("  {green}▸{reset} SQLite DB:     {dim}{}{reset}", db_path.display());
    println!();
    println!("  {yellow}Set in Claude Code:{reset}");
    println!("  {bold}\"ANTHROPIC_BASE_URL\": \"http://127.0.0.1:{proxy_port}\"{reset}");
    println!();
    println!("  {dim}Press Ctrl+C to stop{reset}");
    println!("  {bold}══════════════════════════════════════════════════{reset}");
    println!();
}

#[cfg(test)]
mod main {
    use super::*;

    mod tests {
        use super::*;
        use crate::store::Store;
        use crate::types::{RequestRecord, RequestStatusKind};
        use chrono::Utc;

        #[test]
        fn parse_args_exposes_v2_threshold_flags() {
            use clap::Parser;

            let args = Args::parse_from([
                "claude-proxy",
                "--target",
                "https://api.anthropic.com",
                "--slow-ttft-threshold",
                "3000",
                "--stall-threshold",
                "0.5",
            ]);

            assert_eq!(args.slow_ttft_threshold, 3000);
            assert!((args.stall_threshold - 0.5).abs() < f64::EPSILON);
        }

        fn seed_unanalyzed_request() -> (Arc<Store>, String) {
            let path = std::env::temp_dir().join(format!("proxy-v2-{}.db", uuid::Uuid::new_v4()));
            let store = Arc::new(Store::new(&path).unwrap());
            let request_id = "req-analyzer-1".to_string();

            let request = RequestRecord {
                id: request_id.clone(),
                session_id: Some("session-1".to_string()),
                timestamp: Utc::now(),
                method: "POST".to_string(),
                path: "/v1/messages".to_string(),
                model: "claude-opus-4-1".to_string(),
                stream: true,
                status_code: Some(200),
                status_kind: RequestStatusKind::Success,
                ttft_ms: Some(4200.0),
                duration_ms: Some(6500.0),
                input_tokens: Some(120),
                output_tokens: Some(300),
                cache_read_tokens: None,
                cache_creation_tokens: None,
                thinking_tokens: None,
                request_size_bytes: 1024,
                response_size_bytes: 2048,
                stall_count: 0,
                stall_details_json: "[]".to_string(),
                error_summary: None,
                stop_reason: Some("end_turn".to_string()),
                content_block_types_json: "[]".to_string(),
                anomalies_json: "[]".to_string(),
                analyzed: false,
            };

            store.add_request(&request).unwrap();
            (store, request_id)
        }

        #[tokio::test]
        async fn analyzer_worker_marks_requests_as_analyzed() {
            let (store, request_id) = seed_unanalyzed_request();
            run_analyzer_tick(store.clone()).await.unwrap();
            let req = store.get_request(&request_id).unwrap().unwrap();
            assert!(req.analyzed);
        }
    }
}
