use warp::http::HeaderMap;

use crate::{backend::ChatCompletionsRequest, config::RoutingPolicyConfig, models::ModelRegistry};

use super::{
    analyzer::{RequestFeatures, analyze},
    decision::{OverrideSource, RoutingDecision, RoutingReason, TaskKind, ThinkingLevel},
};

pub fn decide(
    chat_req: &ChatCompletionsRequest,
    headers: &HeaderMap,
    model_registry: &ModelRegistry,
    routing_config: &RoutingPolicyConfig,
) -> RoutingDecision {
    let features = analyze(chat_req);
    let thinking_override = parse_thinking_override(headers);
    let routing_mode = parse_routing_mode(headers, chat_req.model.as_str());

    if model_registry.is_virtual_alias(chat_req.model.as_str()) {
        return decide_virtual_alias(
            &features,
            chat_req.model.as_str(),
            routing_mode,
            thinking_override,
            model_registry,
            routing_config,
        );
    }

    if model_registry.knows_model(chat_req.model.as_str()) {
        return RoutingDecision {
            selected_alias: chat_req.model.clone(),
            backend_model: chat_req.model.clone(),
            thinking_level: thinking_override
                .unwrap_or_else(|| default_thinking(&features, routing_config, "medium")),
            task_kind: features.task_kind.clone(),
            reason_codes: vec![RoutingReason::ExplicitBackendModel],
            override_source: OverrideSource::ClientModel,
        };
    }

    decide_virtual_alias(
        &features,
        "auto",
        routing_mode,
        thinking_override,
        model_registry,
        routing_config,
    )
}

fn decide_virtual_alias(
    features: &RequestFeatures,
    requested_alias: &str,
    routing_mode: RoutingMode,
    thinking_override: Option<ThinkingLevel>,
    model_registry: &ModelRegistry,
    routing_config: &RoutingPolicyConfig,
) -> RoutingDecision {
    let mut reason_codes = Vec::new();
    let selected_alias = if matches!(requested_alias, "small" | "medium" | "large") {
        push_reason(&mut reason_codes, RoutingReason::ExplicitAlias);
        requested_alias.to_string()
    } else {
        select_policy_alias(features, routing_mode, routing_config, &mut reason_codes).to_string()
    };

    let backend_model = model_registry
        .backend_for_alias(&selected_alias)
        .unwrap_or("gpt-5.4")
        .to_string();
    let thinking_level = thinking_override
        .unwrap_or_else(|| default_thinking(features, routing_config, &selected_alias));

    RoutingDecision {
        selected_alias,
        backend_model,
        thinking_level,
        task_kind: features.task_kind.clone(),
        reason_codes,
        override_source: if requested_alias == "auto" || requested_alias == "balanced" {
            OverrideSource::Policy
        } else {
            OverrideSource::ClientAlias
        },
    }
}

fn select_policy_alias(
    features: &RequestFeatures,
    routing_mode: RoutingMode,
    routing_config: &RoutingPolicyConfig,
    reason_codes: &mut Vec<RoutingReason>,
) -> &'static str {
    if routing_mode == RoutingMode::Quality {
        push_reason(reason_codes, RoutingReason::RoutingModeQuality);
        return "large";
    }

    if routing_mode == RoutingMode::Economy {
        push_reason(reason_codes, RoutingReason::RoutingModeEconomy);
        return if requires_large(features, routing_config, reason_codes) {
            "medium"
        } else {
            "small"
        };
    }

    if requires_large(features, routing_config, reason_codes) {
        "large"
    } else if requires_medium(features, routing_config, reason_codes) {
        "medium"
    } else {
        "small"
    }
}

fn requires_medium(
    features: &RequestFeatures,
    routing_config: &RoutingPolicyConfig,
    reason_codes: &mut Vec<RoutingReason>,
) -> bool {
    let coding_task = matches!(
        features.task_kind,
        TaskKind::CodeEditLocal
            | TaskKind::Review
            | TaskKind::ToolWorkflow
            | TaskKind::DebugComplex
    );
    if coding_task {
        push_reason(reason_codes, RoutingReason::MediumTaskKind);
    }

    coding_task
        || features.code_block_count > 0
        || features.file_reference_count > 0
        || features.message_count > 1
        || (features.estimated_chars > routing_config.debug_medium_chars
            && matches!(features.task_kind, TaskKind::DebugSimple))
}

fn requires_large(
    features: &RequestFeatures,
    routing_config: &RoutingPolicyConfig,
    reason_codes: &mut Vec<RoutingReason>,
) -> bool {
    let complex_task = matches!(
        features.task_kind,
        TaskKind::Review
            | TaskKind::Design
            | TaskKind::Migration
            | TaskKind::ToolWorkflow
            | TaskKind::DebugComplex
    );
    if complex_task {
        push_reason(reason_codes, RoutingReason::ComplexTaskKind);
    }
    if features.message_count > routing_config.small_max_messages {
        push_reason(reason_codes, RoutingReason::LongConversation);
    }
    if features.estimated_chars > routing_config.small_max_chars
        && (features.code_block_count > 0
            || features.file_reference_count > 0
            || matches!(
                features.task_kind,
                TaskKind::CodeEditLocal | TaskKind::DebugSimple
            ))
    {
        push_reason(reason_codes, RoutingReason::ExpandedLocalContext);
    }
    if features.estimated_chars > routing_config.large_min_chars {
        push_reason(reason_codes, RoutingReason::LargeContext);
    }
    if features.has_tools {
        push_reason(reason_codes, RoutingReason::ToolsPresent);
    }
    if features.file_reference_count >= routing_config.multi_file_threshold {
        push_reason(reason_codes, RoutingReason::MultiFileContext);
    }
    if features.code_block_count > routing_config.max_code_blocks_for_small {
        push_reason(reason_codes, RoutingReason::MultipleCodeBlocks);
    }

    complex_task
        || features.message_count > routing_config.small_max_messages
        || (features.estimated_chars > routing_config.small_max_chars
            && (features.code_block_count > 0
                || features.file_reference_count > 0
                || matches!(
                    features.task_kind,
                    TaskKind::CodeEditLocal | TaskKind::DebugSimple
                )))
        || features.estimated_chars > routing_config.large_min_chars
        || features.has_tools
        || features.file_reference_count >= routing_config.multi_file_threshold
        || features.code_block_count > routing_config.max_code_blocks_for_small
}

fn default_thinking(
    features: &RequestFeatures,
    routing_config: &RoutingPolicyConfig,
    selected_alias: &str,
) -> ThinkingLevel {
    match (selected_alias, &features.task_kind) {
        ("large", TaskKind::Design | TaskKind::Migration)
            if features.estimated_chars > routing_config.large_min_chars
                || features.has_tools
                || features.file_reference_count >= routing_config.multi_file_threshold =>
        {
            ThinkingLevel::ExtraHigh
        }
        ("large", TaskKind::Design | TaskKind::Migration | TaskKind::DebugComplex) => {
            ThinkingLevel::High
        }
        ("large", TaskKind::Review | TaskKind::ToolWorkflow | TaskKind::CodeEditLocal) => {
            ThinkingLevel::High
        }
        ("medium", TaskKind::DebugComplex) => ThinkingLevel::High,
        ("medium", _) => ThinkingLevel::Medium,
        ("small", TaskKind::DebugSimple)
            if features.estimated_chars > routing_config.debug_medium_chars =>
        {
            ThinkingLevel::Medium
        }
        _ => ThinkingLevel::Low,
    }
}

fn push_reason(reason_codes: &mut Vec<RoutingReason>, reason: RoutingReason) {
    if !reason_codes.contains(&reason) {
        reason_codes.push(reason);
    }
}

fn parse_thinking_override(headers: &HeaderMap) -> Option<ThinkingLevel> {
    match header_value(headers, "x-codex-thinking").as_deref() {
        Some("low") => Some(ThinkingLevel::Low),
        Some("medium") => Some(ThinkingLevel::Medium),
        Some("high") => Some(ThinkingLevel::High),
        Some("extra_high" | "extra-high") => Some(ThinkingLevel::ExtraHigh),
        _ => None,
    }
}

fn parse_routing_mode(headers: &HeaderMap, model: &str) -> RoutingMode {
    let header_mode = header_value(headers, "x-codex-routing-mode");
    match header_mode.as_deref().unwrap_or(model) {
        "economy" => RoutingMode::Economy,
        "quality" => RoutingMode::Quality,
        _ => RoutingMode::Balanced,
    }
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_lowercase())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RoutingMode {
    Economy,
    Balanced,
    Quality,
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use warp::http::{HeaderMap, HeaderValue};

    use crate::{
        backend::{ChatCompletionsRequest, ChatMessage},
        config::RoutingPolicyConfig,
        models::ModelRegistry,
    };

    use super::{RoutingReason, ThinkingLevel, decide};

    #[test]
    fn routes_simple_chat_to_small() {
        let request = ChatCompletionsRequest {
            model: "auto".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: json!("hello there"),
                tool_calls: None,
                tool_call_id: None,
            }],
            temperature: None,
            max_tokens: None,
            stream: Some(false),
            stream_options: None,
            tools: None,
            parallel_tool_calls: None,
            tool_choice: None,
            response_format: None,
        };

        let decision = decide(
            &request,
            &HeaderMap::new(),
            &ModelRegistry::default(),
            &RoutingPolicyConfig::default(),
        );

        assert_eq!(decision.selected_alias, "small");
        assert_eq!(decision.backend_model, "gpt-5.1-codex-mini");
        assert_eq!(decision.thinking_level, ThinkingLevel::Low);
    }

    #[test]
    fn routes_normal_coding_request_to_medium() {
        let request = ChatCompletionsRequest {
            model: "auto".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: json!("update src/main.rs to improve request parsing"),
                tool_calls: None,
                tool_call_id: None,
            }],
            temperature: None,
            max_tokens: None,
            stream: Some(false),
            stream_options: None,
            tools: None,
            parallel_tool_calls: None,
            tool_choice: None,
            response_format: None,
        };

        let decision = decide(
            &request,
            &HeaderMap::new(),
            &ModelRegistry::default(),
            &RoutingPolicyConfig::default(),
        );

        assert_eq!(decision.selected_alias, "medium");
        assert_eq!(decision.backend_model, "gpt-5.3-codex");
        assert_eq!(decision.thinking_level, ThinkingLevel::Medium);
        assert!(
            decision
                .reason_codes
                .contains(&RoutingReason::MediumTaskKind)
        );
    }

    #[test]
    fn routes_design_request_to_large_high() {
        let request = ChatCompletionsRequest {
            model: "auto".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: json!("design an architecture and refactor plan for a multi-file proxy"),
                tool_calls: None,
                tool_call_id: None,
            }],
            temperature: None,
            max_tokens: None,
            stream: Some(false),
            stream_options: None,
            tools: None,
            parallel_tool_calls: None,
            tool_choice: None,
            response_format: None,
        };

        let decision = decide(
            &request,
            &HeaderMap::new(),
            &ModelRegistry::default(),
            &RoutingPolicyConfig::default(),
        );

        assert_eq!(decision.selected_alias, "large");
        assert_eq!(decision.backend_model, "gpt-5.4");
        assert_eq!(decision.thinking_level, ThinkingLevel::High);
        assert!(
            decision
                .reason_codes
                .contains(&RoutingReason::ComplexTaskKind)
        );
    }

    #[test]
    fn honors_custom_large_thresholds() {
        let request = ChatCompletionsRequest {
            model: "auto".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: json!("please explain this long long long prompt"),
                tool_calls: None,
                tool_call_id: None,
            }],
            temperature: None,
            max_tokens: None,
            stream: Some(false),
            stream_options: None,
            tools: None,
            parallel_tool_calls: None,
            tool_choice: None,
            response_format: None,
        };
        let config = RoutingPolicyConfig {
            large_min_chars: 10,
            ..RoutingPolicyConfig::default()
        };

        let decision = decide(
            &request,
            &HeaderMap::new(),
            &ModelRegistry::default(),
            &config,
        );

        assert_eq!(decision.selected_alias, "large");
    }

    #[test]
    fn honors_thinking_header_override() {
        let request = ChatCompletionsRequest {
            model: "auto".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: json!("hello there"),
                tool_calls: None,
                tool_call_id: None,
            }],
            temperature: None,
            max_tokens: None,
            stream: Some(false),
            stream_options: None,
            tools: None,
            parallel_tool_calls: None,
            tool_choice: None,
            response_format: None,
        };
        let mut headers = HeaderMap::new();
        headers.insert("x-codex-thinking", HeaderValue::from_static("high"));

        let decision = decide(
            &request,
            &headers,
            &ModelRegistry::default(),
            &RoutingPolicyConfig::default(),
        );

        assert_eq!(decision.thinking_level, ThinkingLevel::High);
    }

    #[test]
    fn honors_extra_high_thinking_override() {
        let request = ChatCompletionsRequest {
            model: "auto".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: json!("design an auth migration plan"),
                tool_calls: None,
                tool_call_id: None,
            }],
            temperature: None,
            max_tokens: None,
            stream: Some(false),
            stream_options: None,
            tools: None,
            parallel_tool_calls: None,
            tool_choice: None,
            response_format: None,
        };
        let mut headers = HeaderMap::new();
        headers.insert("x-codex-thinking", HeaderValue::from_static("extra_high"));

        let decision = decide(
            &request,
            &headers,
            &ModelRegistry::default(),
            &RoutingPolicyConfig::default(),
        );

        assert_eq!(decision.thinking_level, ThinkingLevel::ExtraHigh);
    }

    #[test]
    fn honors_quality_routing_mode_override() {
        let request = ChatCompletionsRequest {
            model: "auto".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: json!("hello there"),
                tool_calls: None,
                tool_call_id: None,
            }],
            temperature: None,
            max_tokens: None,
            stream: Some(false),
            stream_options: None,
            tools: None,
            parallel_tool_calls: None,
            tool_choice: None,
            response_format: None,
        };
        let mut headers = HeaderMap::new();
        headers.insert("x-codex-routing-mode", HeaderValue::from_static("quality"));

        let decision = decide(
            &request,
            &headers,
            &ModelRegistry::default(),
            &RoutingPolicyConfig::default(),
        );

        assert_eq!(decision.selected_alias, "large");
        assert!(
            decision
                .reason_codes
                .contains(&RoutingReason::RoutingModeQuality)
        );
    }
}
