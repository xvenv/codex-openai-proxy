use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;
use std::collections::HashMap;

use crate::models::{ModelRegistry, ModelRegistryEntry};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Port to listen on
    #[arg(short, long, default_value = "8080")]
    pub port: u16,

    /// Path to Codex auth.json file
    #[arg(long, default_value = "~/.codex/auth.json")]
    pub auth_path: String,

    /// Path to proxy config file
    #[arg(long, default_value = "config/proxy.json")]
    pub config_path: String,
}

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub port: u16,
    pub auth_path: String,
    pub model_registry: ModelRegistry,
    pub routing: RoutingPolicyConfig,
    pub execution: ExecutionConfig,
    pub anthropic_mapping: HashMap<String, String>,
}

impl AppConfig {
    pub fn from_args(args: Args) -> Result<Self> {
        let config_path = expand_home_path(&args.config_path);
        let file_config = load_file_config(&config_path)?;

        Ok(Self {
            port: args.port,
            auth_path: expand_home_path(&args.auth_path),
            model_registry: ModelRegistry::from_config(
                file_config.default_client_model,
                file_config.models,
            ),
            routing: file_config.routing,
            execution: file_config.execution,
            anthropic_mapping: file_config.anthropic_mapping,
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct RoutingPolicyConfig {
    pub small_max_messages: usize,
    pub small_max_chars: usize,
    pub large_min_chars: usize,
    pub multi_file_threshold: usize,
    pub max_code_blocks_for_small: usize,
    pub debug_medium_chars: usize,
}

impl Default for RoutingPolicyConfig {
    fn default() -> Self {
        Self {
            small_max_messages: 4,
            small_max_chars: 2_000,
            large_min_chars: 4_000,
            multi_file_threshold: 2,
            max_code_blocks_for_small: 2,
            debug_medium_chars: 1_200,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ExecutionConfig {
    pub prefer_real_backend: bool,
    pub fallback_to_stub: bool,
    pub enable_non_streaming_escalation: bool,
    pub escalation_min_content_chars: usize,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            prefer_real_backend: true,
            fallback_to_stub: false,
            enable_non_streaming_escalation: true,
            escalation_min_content_chars: 160,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FileConfig {
    default_client_model: Option<String>,
    models: Vec<ModelRegistryEntry>,
    routing: RoutingPolicyConfig,
    execution: ExecutionConfig,
    anthropic_mapping: HashMap<String, String>,
}

fn expand_home_path(path: &str) -> String {
    if let Some(relative) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{relative}");
        }
    }

    path.to_string()
}

fn load_file_config(path: &str) -> Result<FileConfig> {
    let config_path = std::path::Path::new(path);
    if !config_path.exists() {
        return Ok(FileConfig::default());
    }

    let config_text =
        std::fs::read_to_string(config_path).with_context(|| format!("Failed to read {path}"))?;
    serde_json::from_str(&config_text).with_context(|| format!("Failed to parse {path}"))
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::{load_file_config, RoutingPolicyConfig};

    #[test]
    fn missing_config_uses_defaults() {
        let path = unique_test_path("missing");
        let config = load_file_config(path.to_str().unwrap()).expect("config should load");

        assert!(config.models.is_empty());
        assert_eq!(config.routing.small_max_messages, 4);
    }

    #[test]
    fn parses_routing_thresholds_from_file() {
        let path = unique_test_path("custom");
        fs::write(
            &path,
            r#"{
              "routing": {
                "small_max_messages": 2,
                "small_max_chars": 100,
                "large_min_chars": 500,
                "multi_file_threshold": 3,
                "max_code_blocks_for_small": 1,
                "debug_medium_chars": 50
              }
            }"#,
        )
        .expect("config file should be written");

        let config = load_file_config(path.to_str().unwrap()).expect("config should parse");
        assert_eq!(config.routing.small_max_messages, 2);
        assert_eq!(config.routing.large_min_chars, 500);

        fs::remove_file(path).ok();
    }

    #[test]
    fn parses_anthropic_mapping_from_file() {
        let path = unique_test_path("anthropic-mapping");
        fs::write(
            &path,
            r#"{
              "anthropic_mapping": {
                "claude-code-fast": "small",
                "claude-opus-4-1": "large"
              }
            }"#,
        )
        .expect("config file should be written");

        let config = load_file_config(path.to_str().unwrap()).expect("config should parse");
        assert_eq!(
            config
                .anthropic_mapping
                .get("claude-code-fast")
                .map(String::as_str),
            Some("small")
        );
        assert_eq!(
            config
                .anthropic_mapping
                .get("claude-opus-4-1")
                .map(String::as_str),
            Some("large")
        );

        fs::remove_file(path).ok();
    }

    #[test]
    fn routing_policy_defaults_stay_stable() {
        let config = RoutingPolicyConfig::default();
        assert_eq!(config.small_max_chars, 2_000);
        assert_eq!(config.debug_medium_chars, 1_200);
    }

    fn unique_test_path(label: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be valid")
            .as_nanos();
        path.push(format!("codex-openai-proxy-{label}-{nanos}.json"));
        path
    }
}
