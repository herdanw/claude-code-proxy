use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

fn wildcard_match(pattern: &str, value: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        value.starts_with(prefix)
    } else {
        value == pattern
    }
}

pub fn should_auto_tune(sample_count: u64) -> bool {
    sample_count >= 50 && sample_count % 50 == 0
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
}
