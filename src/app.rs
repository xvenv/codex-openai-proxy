use anyhow::Result;
use log::info;
use std::collections::HashMap;

use crate::{
    backend::ProxyServer,
    config::{AppConfig, Args, ExecutionConfig, RoutingPolicyConfig},
    http,
    models::ModelRegistry,
};

#[derive(Clone)]
pub struct AppState {
    pub proxy: ProxyServer,
    pub model_registry: ModelRegistry,
    pub routing: RoutingPolicyConfig,
    pub execution: ExecutionConfig,
    pub anthropic_mapping: HashMap<String, String>,
}

impl AppState {
    pub async fn new(config: &AppConfig) -> Result<Self> {
        Ok(Self {
            proxy: ProxyServer::new(&config.auth_path).await?,
            model_registry: config.model_registry.clone(),
            routing: config.routing.clone(),
            execution: config.execution.clone(),
            anthropic_mapping: config.anthropic_mapping.clone(),
        })
    }

    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self {
            proxy: ProxyServer::for_tests(),
            model_registry: ModelRegistry::default(),
            routing: RoutingPolicyConfig::default(),
            execution: ExecutionConfig {
                prefer_real_backend: false,
                fallback_to_stub: true,
                enable_non_streaming_escalation: false,
                escalation_min_content_chars: 160,
            },
            anthropic_mapping: HashMap::from([
                ("claude-code-fast".to_string(), "small".to_string()),
                ("claude-code-default".to_string(), "medium".to_string()),
                ("claude-code-max".to_string(), "large".to_string()),
            ]),
        }
    }
}

pub async fn run(args: Args) -> Result<()> {
    let _ = env_logger::try_init();

    let config = AppConfig::from_args(args)?;
    info!("event=startup stage=initializing");

    let state = AppState::new(&config).await?;
    info!(
        "event=startup stage=auth_loaded auth_path={}",
        config.auth_path
    );

    let routes = http::routes(state);

    info!(
        "event=startup stage=listening bind=0.0.0.0 port={}",
        config.port
    );
    info!(
        "event=startup health_url=http://localhost:{}/health chat_url=http://localhost:{}/v1/chat/completions base_url=http://localhost:{} default_model={}",
        config.port,
        config.port,
        config.port,
        config.model_registry.default_client_model()
    );

    warp::serve(routes).run(([0, 0, 0, 0], config.port)).await;

    Ok(())
}
