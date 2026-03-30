use anyhow::{Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::models::{ModelRegistry, ModelRegistryEntry};

const DEFAULT_CONFIG_PATH: &str = "~/.config/codex-proxy/config.json";
const LEGACY_CONFIG_PATH: &str = "~/.codex-proxy/config.json";

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Port to listen on
    #[arg(short, long)]
    pub port: Option<u16>,

    /// Path to Codex auth.json file
    #[arg(long)]
    pub auth_path: Option<String>,

    /// Path to proxy config file
    #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
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
        let defaults = FileConfig::default();

        let port = args
            .port
            .or(file_config.port)
            .unwrap_or(defaults.port.unwrap_or(8080));
        let auth_path = args
            .auth_path
            .or(file_config.auth_path)
            .unwrap_or_else(|| defaults.default_auth_path());

        Ok(Self {
            port,
            auth_path: expand_home_path(&auth_path),
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

#[derive(Clone, Debug, Deserialize, Serialize)]
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

#[derive(Clone, Debug, Deserialize, Serialize)]
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

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
struct FileConfig {
    port: Option<u16>,
    auth_path: Option<String>,
    default_client_model: Option<String>,
    models: Vec<ModelRegistryEntry>,
    routing: RoutingPolicyConfig,
    execution: ExecutionConfig,
    anthropic_mapping: HashMap<String, String>,
}

impl FileConfig {
    fn default_auth_path(&self) -> String {
        self.auth_path
            .clone()
            .unwrap_or_else(|| "~/.config/codex-proxy/auth.json".to_string())
    }
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            port: Some(8080),
            auth_path: Some("~/.config/codex-proxy/auth.json".to_string()),
            default_client_model: Some("auto".to_string()),
            models: vec![
                ModelRegistryEntry {
                    id: "auto".to_string(),
                    owned_by: "proxy".to_string(),
                    backend_target: None,
                },
                ModelRegistryEntry {
                    id: "balanced".to_string(),
                    owned_by: "proxy".to_string(),
                    backend_target: None,
                },
                ModelRegistryEntry {
                    id: "small".to_string(),
                    owned_by: "proxy".to_string(),
                    backend_target: Some("gpt-5.1-codex-mini".to_string()),
                },
                ModelRegistryEntry {
                    id: "medium".to_string(),
                    owned_by: "proxy".to_string(),
                    backend_target: Some("gpt-5.3-codex".to_string()),
                },
                ModelRegistryEntry {
                    id: "large".to_string(),
                    owned_by: "proxy".to_string(),
                    backend_target: Some("gpt-5.4".to_string()),
                },
                ModelRegistryEntry {
                    id: "gpt-5.1-codex-mini".to_string(),
                    owned_by: "openai".to_string(),
                    backend_target: None,
                },
                ModelRegistryEntry {
                    id: "gpt-5.3-codex".to_string(),
                    owned_by: "openai".to_string(),
                    backend_target: None,
                },
                ModelRegistryEntry {
                    id: "gpt-5.4".to_string(),
                    owned_by: "openai".to_string(),
                    backend_target: None,
                },
            ],
            routing: RoutingPolicyConfig::default(),
            execution: ExecutionConfig::default(),
            anthropic_mapping: HashMap::from([
                ("claude-code-fast".to_string(), "small".to_string()),
                ("claude-code-default".to_string(), "medium".to_string()),
                ("claude-code-max".to_string(), "large".to_string()),
                ("claude-haiku".to_string(), "small".to_string()),
                ("claude-sonnet".to_string(), "medium".to_string()),
                ("claude-opus".to_string(), "large".to_string()),
            ]),
        }
    }
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
    ensure_file_config(config_path)?;

    let config_text =
        std::fs::read_to_string(config_path).with_context(|| format!("Failed to read {path}"))?;
    serde_json::from_str(&config_text).with_context(|| format!("Failed to parse {path}"))
}

fn ensure_file_config(path: &std::path::Path) -> Result<()> {
    if !path.exists() {
        if path == std::path::Path::new(&expand_home_path(DEFAULT_CONFIG_PATH)) {
            let legacy_path_string = expand_home_path(LEGACY_CONFIG_PATH);
            let legacy_path = std::path::Path::new(&legacy_path_string);
            if legacy_path.exists() {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).with_context(|| {
                        format!("Failed to create config directory {}", parent.display())
                    })?;
                }
                std::fs::copy(legacy_path, path).with_context(|| {
                    format!(
                        "Failed to migrate legacy config from {} to {}",
                        legacy_path.display(),
                        path.display()
                    )
                })?;
                return Ok(());
            }
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create config directory {}", parent.display())
            })?;
        }
        let default_config = serde_json::to_string_pretty(&FileConfig::default())
            .context("Failed to serialize default config")?;
        std::fs::write(path, format!("{default_config}\n"))
            .with_context(|| format!("Failed to write default config {}", path.display()))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use clap::Parser;

    use super::{AppConfig, Args, RoutingPolicyConfig, load_file_config};

    #[test]
    fn missing_config_uses_defaults() {
        let path = unique_test_path("missing");
        let config = load_file_config(path.to_str().unwrap()).expect("config should load");

        assert_eq!(config.port, Some(8080));
        assert_eq!(config.auth_path.as_deref(), Some("~/.config/codex-proxy/auth.json"));
        assert!(!config.models.is_empty());
        assert_eq!(config.routing.small_max_messages, 4);
        assert!(path.exists());
        fs::remove_file(path).ok();
    }

    #[test]
    fn parses_port_and_auth_path_from_file() {
        let path = unique_test_path("runtime");
        fs::write(
            &path,
            r#"{
              "port": 9090,
              "auth_path": "~/custom-auth.json"
            }"#,
        )
        .expect("config file should be written");

        let config = load_file_config(path.to_str().unwrap()).expect("config should parse");
        assert_eq!(config.port, Some(9090));
        assert_eq!(config.auth_path.as_deref(), Some("~/custom-auth.json"));

        fs::remove_file(path).ok();
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

    #[test]
    fn cli_overrides_file_runtime_values() {
        let path = unique_test_path("override");
        fs::write(
            &path,
            r#"{
              "port": 9090,
              "auth_path": "~/from-file.json"
            }"#,
        )
        .expect("config file should be written");

        let args = Args::parse_from([
            "codex-openai-proxy",
            "--config-path",
            path.to_str().expect("path should be utf8"),
            "--port",
            "9191",
            "--auth-path",
            "~/from-cli.json",
        ]);

        let config = AppConfig::from_args(args).expect("app config should build");
        assert_eq!(config.port, 9191);
        assert!(config.auth_path.ends_with("/from-cli.json"));

        fs::remove_file(path).ok();
    }

    #[test]
    fn creates_default_config_file_when_missing() {
        let path = unique_test_path("autocreate");
        load_file_config(path.to_str().unwrap()).expect("config should load");

        let written = fs::read_to_string(&path).expect("config file should exist");
        assert!(written.contains("\"port\": 8080"));
        assert!(written.contains("\"auth_path\": \"~/.config/codex-proxy/auth.json\""));

        fs::remove_file(path).ok();
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
