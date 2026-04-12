use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub profiles: HashMap<String, serde_json::Value>,
    pub model_mappings: HashMap<String, String>,
}

pub fn resolve_behavior_class(config: &ModelConfig, model_name: &str) -> Option<String> {
    if let Some(class) = config.model_mappings.get(model_name) {
        return Some(class.clone());
    }

    config
        .model_mappings
        .iter()
        .filter_map(|(pattern, class)| {
            let prefix = pattern.strip_suffix('*')?;
            if model_name.starts_with(prefix) {
                Some((prefix.len(), class))
            } else {
                None
            }
        })
        .max_by_key(|(prefix_len, _)| *prefix_len)
        .map(|(_, class)| class.clone())
}

/// Load a model config JSON file from disk.
pub fn load_model_config(path: &Path) -> Result<ModelConfig, String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    let config: ModelConfig = serde_json::from_str(&contents)
        .map_err(|e| format!("Failed to parse {}: {e}", path.display()))?;
    Ok(config)
}

pub fn should_auto_tune(sample_count: u64) -> bool {
    sample_count >= 50 && sample_count.is_multiple_of(50)
}

pub fn fingerprint_parameter_names() -> Vec<&'static str> {
    vec![
        "avg_ttft_ms", "median_ttft_ms", "p95_ttft_ms", "p99_ttft_ms",
        "min_ttft_ms", "max_ttft_ms", "ttft_stddev_ms", "avg_duration_ms",
        "tokens_per_second", "avg_inter_chunk_ms", "chunk_timing_variance",
        "ttft_vs_context_correlation", "thinking_frequency", "avg_thinking_tokens",
        "median_thinking_tokens", "thinking_token_ratio", "thinking_depth_by_complexity",
        "redacted_thinking_frequency", "thinking_before_tool_rate", "thinking_per_turn_variance",
        "max_thinking_tokens", "effort_response_correlation", "avg_input_tokens",
        "avg_output_tokens", "median_output_tokens", "p95_output_tokens",
        "output_input_ratio", "max_tokens_hit_rate", "cache_creation_rate",
        "cache_hit_rate", "avg_cache_read_tokens", "cache_miss_after_hit_rate",
        "total_tokens_per_request", "output_token_consistency", "token_efficiency",
        "context_window_utilization", "tool_call_rate", "tools_per_turn",
        "max_tools_per_turn", "multi_tool_rate", "unique_tool_diversity",
        "tool_preference_distribution", "tool_chain_depth", "max_tool_chain_depth",
        "tool_success_rate", "tool_retry_rate", "tool_adaptation_rate",
        "tool_input_avg_size", "tool_call_position", "text_before_tool_ratio",
        "tool_use_after_thinking", "deferred_tool_usage", "avg_content_blocks",
        "max_content_blocks", "text_block_count_avg", "avg_text_block_length",
        "block_type_distribution", "stop_reason_distribution", "end_turn_rate",
        "code_in_response_rate", "markdown_usage_rate", "response_structure_variance",
        "multi_text_block_rate", "interleaved_thinking_rate", "citations_frequency",
        "connector_text_frequency", "stall_rate", "avg_stall_duration_ms",
        "max_stall_duration_ms", "stalls_per_request", "stall_position_distribution",
        "stream_completion_rate", "interrupted_stream_rate", "ping_frequency",
        "avg_chunks_per_response", "bytes_per_chunk_avg", "first_content_event_ms",
        "stream_warmup_pattern", "error_rate", "server_error_rate",
        "rate_limit_rate", "overload_rate", "client_error_rate",
        "timeout_rate", "connection_error_rate", "error_type_distribution",
        "refusal_rate", "error_recovery_rate", "consecutive_error_max",
        "error_time_clustering", "avg_requests_per_session", "session_duration_avg_ms",
        "inter_request_gap_avg_ms", "inter_request_gap_variance", "context_growth_rate",
        "conversation_depth_avg", "session_error_clustering", "session_tool_evolution",
        "session_ttft_trend", "session_token_trend", "system_prompt_frequency",
        "system_prompt_avg_size", "avg_message_count", "tools_provided_avg",
        "tool_choice_distribution", "temperature_distribution", "max_tokens_setting_avg",
        "image_input_rate", "document_input_rate", "request_body_avg_bytes",
        "effort_param_usage", "effort_thinking_correlation", "effort_output_correlation",
        "effort_ttft_correlation", "speed_mode_usage", "speed_mode_ttft_impact",
        "speed_mode_quality_impact", "task_budget_usage", "cache_control_usage_rate",
        "cache_scope_global_rate", "cache_ttl_1h_rate", "cache_edit_usage_rate",
        "cache_cost_savings_ratio", "cache_stability", "cache_warmup_requests",
        "cache_invalidation_pattern", "beta_features_count", "beta_feature_set",
        "custom_headers_present", "anthropic_version", "provider_type",
        "auth_method", "request_id_tracking", "response_request_id",
        "unknown_sse_event_types", "unknown_content_block_types", "unknown_request_fields",
        "unknown_header_patterns", "unknown_stop_reasons", "unknown_delta_types",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_behavior_class_uses_wildcard_mapping() {
        let config = ModelConfig {
            profiles: std::collections::HashMap::new(),
            model_mappings: std::collections::HashMap::from([(
                "claude-opus-4-*".to_string(),
                "opus".to_string(),
            )]),
        };

        let class = resolve_behavior_class(&config, "claude-opus-4-20260301");
        assert_eq!(class.as_deref(), Some("opus"));
    }

    #[test]
    fn resolve_behavior_class_prefers_exact_match_over_wildcard() {
        let config = ModelConfig {
            profiles: std::collections::HashMap::new(),
            model_mappings: std::collections::HashMap::from([
                ("claude-opus-4-*".to_string(), "wildcard-opus".to_string()),
                (
                    "claude-opus-4-20260301".to_string(),
                    "exact-opus".to_string(),
                ),
            ]),
        };

        let class = resolve_behavior_class(&config, "claude-opus-4-20260301");
        assert_eq!(class.as_deref(), Some("exact-opus"));
    }

    #[test]
    fn resolve_behavior_class_prefers_most_specific_wildcard_prefix() {
        let config = ModelConfig {
            profiles: std::collections::HashMap::new(),
            model_mappings: std::collections::HashMap::from([
                ("claude-*".to_string(), "generic".to_string()),
                ("claude-opus-*".to_string(), "opus-generic".to_string()),
                (
                    "claude-opus-4-*".to_string(),
                    "opus-v4-specific".to_string(),
                ),
            ]),
        };

        let class = resolve_behavior_class(&config, "claude-opus-4-20260301");
        assert_eq!(class.as_deref(), Some("opus-v4-specific"));
    }

    #[test]
    fn resolve_behavior_class_returns_none_when_no_match_exists() {
        let config = ModelConfig {
            profiles: std::collections::HashMap::new(),
            model_mappings: std::collections::HashMap::from([
                ("claude-opus-4-*".to_string(), "opus".to_string()),
                ("gpt-4-*".to_string(), "gpt".to_string()),
            ]),
        };

        let class = resolve_behavior_class(&config, "llama-3-70b");
        assert_eq!(class, None);
    }

    #[test]
    fn should_auto_tune_boundary_values() {
        assert!(!should_auto_tune(49));
        assert!(should_auto_tune(50));
        assert!(!should_auto_tune(51));
        assert!(should_auto_tune(100));
    }

    #[test]
    fn load_model_config_parses_valid_json() {
        let json = r#"{
            "model_mappings": { "claude-opus-4-*": "opus" },
            "profiles": { "opus": { "avg_ttft_ms": 3500 } }
        }"#;
        let tmp = std::env::temp_dir().join(format!("test-config-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&tmp, json).unwrap();
        let config = load_model_config(&tmp).unwrap();
        assert_eq!(config.model_mappings.get("claude-opus-4-*").unwrap(), "opus");
        assert_eq!(config.profiles.get("opus").unwrap()["avg_ttft_ms"], 3500.0);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn load_model_config_returns_error_for_missing_file() {
        let result = load_model_config(std::path::Path::new("/nonexistent/config.json"));
        assert!(result.is_err());
    }

    #[test]
    fn fingerprint_parameter_names_returns_140_entries() {
        let names = fingerprint_parameter_names();
        assert_eq!(names.len(), 140);
        assert!(names.contains(&"avg_ttft_ms"));
        assert!(names.contains(&"unknown_delta_types"));
    }
}
