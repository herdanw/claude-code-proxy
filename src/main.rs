mod proxy;
mod stats;
mod dashboard;
mod correlation;
mod local_context;
mod correlation_engine;
mod explainer;
mod session_admin;
mod settings_admin;

use clap::Parser;
use correlation::PayloadPolicy;
use correlation_engine::{correlate_request, CorrelationConfig};
use explainer::{explain_request, ExplanationContext};
use stats::{RequestCorrelation, SourceKind};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(name = "claude-proxy", about = "Ultra-fast API logging proxy for Claude Code")]
struct Args {
    /// Target API URL to proxy to
    #[arg(short, long)]
    target: String,

    /// Proxy listen port (Claude Code connects here)
    #[arg(short, long, default_value_t = 9090)]
    port: u16,

    /// Dashboard port
    #[arg(short, long, default_value_t = 9091)]
    dashboard_port: u16,

    /// Open dashboard in browser on start
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    open_browser: bool,

    /// Storage directory for proxy data (default: ~/.claude/api-logs/)
    #[arg(long)]
    log_dir: Option<String>,

    /// Max log entries to keep in memory
    #[arg(long, default_value_t = 50000)]
    max_entries: usize,

    /// Claude directory for local correlation sources
    #[arg(long)]
    claude_dir: Option<String>,

    /// Payload policy for projects correlation
    #[arg(long, default_value = "metadata")]
    projects_policy: String,

    /// Payload policy for shell correlation
    #[arg(long, default_value = "metadata")]
    shell_policy: String,

    /// Payload policy for config correlation
    #[arg(long, default_value = "metadata")]
    config_policy: String,

    /// Enable shell correlation source
    #[arg(long, default_value_t = false)]
    enable_shell_correlation: bool,

    /// Enable config correlation source
    #[arg(long, default_value_t = false)]
    enable_config_correlation: bool,

    /// Stall detection threshold in seconds
    #[arg(long, default_value_t = 20.0)]
    stall_threshold: f64,

    /// Slow TTFT threshold in seconds
    #[arg(long, default_value_t = 8.0)]
    slow_ttft_threshold: f64,

    /// Max request/response body size to store (bytes, default 2MB)
    #[arg(long, default_value_t = 2_097_152)]
    max_body_size: usize,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    let target = args.target.trim_end_matches('/').to_string();

    let projects_policy = PayloadPolicy::from_str(&args.projects_policy).unwrap_or_else(|_| {
        panic!(
            "Invalid value for --projects-policy: '{}'. Expected one of: metadata, metadata_only, redacted, full",
            args.projects_policy
        )
    });
    let shell_policy = PayloadPolicy::from_str(&args.shell_policy).unwrap_or_else(|_| {
        panic!(
            "Invalid value for --shell-policy: '{}'. Expected one of: metadata, metadata_only, redacted, full",
            args.shell_policy
        )
    });
    let config_policy = PayloadPolicy::from_str(&args.config_policy).unwrap_or_else(|_| {
        panic!(
            "Invalid value for --config-policy: '{}'. Expected one of: metadata, metadata_only, redacted, full",
            args.config_policy
        )
    });

    // Resolve storage directory
    let log_dir = if let Some(dir) = &args.log_dir {
        std::path::PathBuf::from(dir)
    } else {
        dirs::home_dir()
            .unwrap_or_default()
            .join(".claude")
            .join("api-logs")
    };
    std::fs::create_dir_all(&log_dir).ok();

    let claude_root = if let Some(dir) = &args.claude_dir {
        PathBuf::from(dir)
    } else {
        dirs::home_dir().unwrap_or_default().join(".claude")
    };

    let store = Arc::new(stats::StatsStore::new(
        args.max_entries,
        log_dir.clone(),
        args.stall_threshold,
        args.slow_ttft_threshold,
        args.max_body_size,
        claude_root.clone(),
    ));

    // Load recent persisted entries from SQLite
    store.load_from_db();

    print_banner(
        &target,
        args.port,
        args.dashboard_port,
        &log_dir,
        store.database_path(),
    );

    let dashboard_url = format!("http://127.0.0.1:{}", args.dashboard_port);

    // Spawn dashboard server
    let store_dash = store.clone();
    let dash_port = args.dashboard_port;
    tokio::spawn(async move {
        if let Err(err) = dashboard::run_dashboard(store_dash, dash_port).await {
            eprintln!("  Dashboard startup failed: {err}");
        }
    });

    // Spawn local projects ingestion worker (phase 1)
    let store_local = store.clone();
    let enable_shell_correlation = args.enable_shell_correlation;
    let enable_config_correlation = args.enable_config_correlation;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
        loop {
            let checkpoint = store_local.get_ingestion_checkpoint(SourceKind::ClaudeProject);
            let mut all_events = local_context::ingest_projects_once_async(
                claude_root.clone(),
                projects_policy,
                checkpoint,
            )
            .await;

            if enable_shell_correlation {
                let shell_checkpoint = store_local.get_ingestion_checkpoint(SourceKind::ShellSnapshot);
                let shell_events = local_context::ingest_shell_snapshots_once(
                    &claude_root,
                    shell_policy,
                    shell_checkpoint.as_deref(),
                );
                all_events.extend(shell_events);
            }

            if enable_config_correlation {
                let config_events = local_context::ingest_config_snapshots_once(
                    &claude_root,
                    config_policy,
                    None,
                );
                all_events.extend(config_events);
            }

            let mut newest_project_event_ms: Option<i64> = None;
            let mut newest_shell_event_ms: Option<i64> = None;

            for event in all_events {
                if !store_local.upsert_local_event(&event) {
                    continue;
                }

                match event.source_kind {
                    SourceKind::ClaudeProject => {
                        newest_project_event_ms = Some(
                            newest_project_event_ms
                                .map_or(event.event_time_ms, |current| current.max(event.event_time_ms)),
                        );
                    }
                    SourceKind::ShellSnapshot => {
                        newest_shell_event_ms = Some(
                            newest_shell_event_ms
                                .map_or(event.event_time_ms, |current| current.max(event.event_time_ms)),
                        );
                    }
                    _ => {}
                }
            }

            if let Some(ms) = newest_project_event_ms {
                store_local.set_ingestion_checkpoint(SourceKind::ClaudeProject, &ms.to_string());
            }
            if let Some(ms) = newest_shell_event_ms {
                store_local.set_ingestion_checkpoint(SourceKind::ShellSnapshot, &ms.to_string());
            }

            interval.tick().await;
        }
    });

    // Spawn correlation worker loop
    let store_corr = store.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
        let config = CorrelationConfig::default();

        loop {
            let recent_requests = store_corr.get_recent_requests_for_correlation(100);
            let recent_events = store_corr.get_recent_local_events_for_correlation(200);

            for request in recent_requests {
                let links = correlate_request(&request, &recent_events, &config)
                    .into_iter()
                    .map(|link| RequestCorrelation {
                        id: link.id,
                        request_id: link.request_id,
                        local_event_id: link.local_event_id,
                        link_type: link.link_type,
                        confidence: link.confidence,
                        reason: link.reason,
                        created_at_ms: chrono::Utc::now().timestamp_millis(),
                    })
                    .collect::<Vec<_>>();

                store_corr.replace_correlations_for_request(&request.id, &links);
            }

            interval.tick().await;
        }
    });

    // Spawn explanation worker loop
    let store_explainer = store.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(12));

        loop {
            let recent_requests = store_explainer.get_recent_requests_for_correlation(100);

            for request in recent_requests {
                if request.anomalies.is_empty() {
                    continue;
                }

                let correlations = store_explainer.get_correlations_for_request(&request.id, 20);
                let local_events = store_explainer.get_local_events_for_request_correlations(&correlations);
                let context = ExplanationContext {
                    correlations,
                    local_events,
                };

                let explanations = explain_request(&request, &context);
                store_explainer.replace_explanations_for_request(&request.id, &explanations);
            }

            interval.tick().await;
        }
    });

    // Open browser
    if args.open_browser {
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        let _ = open::that(&dashboard_url);
    }

    // Run proxy (blocking)
    if let Err(err) = proxy::run_proxy(store, &target, args.port).await {
        eprintln!("  Proxy startup failed: {err}");
        std::process::exit(1);
    }
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
    println!("  {bold}{cyan}  ⚡ Claude Code Proxy — Ultra-Fast API Monitor{reset}");
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
