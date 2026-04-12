use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadPolicy {
    MetadataOnly,
    Redacted,
    Full,
}

impl FromStr for PayloadPolicy {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "metadata" | "metadata_only" => Ok(Self::MetadataOnly),
            "redacted" => Ok(Self::Redacted),
            "full" => Ok(Self::Full),
            other => Err(format!("Unsupported payload policy: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_payload_policy_from_str() {
        use std::str::FromStr;

        assert_eq!(
            PayloadPolicy::from_str("metadata").unwrap(),
            PayloadPolicy::MetadataOnly
        );
        assert_eq!(
            PayloadPolicy::from_str("redacted").unwrap(),
            PayloadPolicy::Redacted
        );
        assert_eq!(
            PayloadPolicy::from_str("full").unwrap(),
            PayloadPolicy::Full
        );
        assert!(PayloadPolicy::from_str("unknown").is_err());
    }
}
