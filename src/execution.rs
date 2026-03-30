use crate::{
    backend::{ChatCompletionsRequest, ChatCompletionsResponse, ProxyResult, ProxyServer},
    config::ExecutionConfig,
    models::ModelRegistry,
    routing::{
        EscalationReason, OverrideSource, RoutingDecision, RoutingReason, decision::ThinkingLevel,
    },
};

#[derive(Debug)]
pub struct ExecutionOutcome {
    pub response: ChatCompletionsResponse,
    pub final_decision: RoutingDecision,
    pub escalated: bool,
    pub escalation_reason: Option<EscalationReason>,
}

pub async fn execute_chat_completion(
    proxy: &ProxyServer,
    model_registry: &ModelRegistry,
    execution: &ExecutionConfig,
    request: ChatCompletionsRequest,
    decision: RoutingDecision,
) -> ProxyResult<ExecutionOutcome> {
    let response = proxy
        .proxy_request(request.clone(), execution, Some(&decision.thinking_level))
        .await?;

    if !should_escalate(&response, &decision, execution) {
        return Ok(ExecutionOutcome {
            response,
            final_decision: decision,
            escalated: false,
            escalation_reason: None,
        });
    }

    let Some(next_alias) = next_escalation_alias(&decision.selected_alias) else {
        return Ok(ExecutionOutcome {
            response,
            final_decision: decision,
            escalated: false,
            escalation_reason: None,
        });
    };
    let Some(next_backend) = model_registry.backend_for_alias(next_alias) else {
        return Ok(ExecutionOutcome {
            response,
            final_decision: decision,
            escalated: false,
            escalation_reason: None,
        });
    };

    let mut escalated_request = request;
    escalated_request.model = next_backend.to_string();

    let mut escalated_decision = decision;
    escalated_decision.selected_alias = next_alias.to_string();
    escalated_decision.backend_model = next_backend.to_string();
    escalated_decision.thinking_level = escalated_thinking(&escalated_decision.thinking_level);
    escalated_decision
        .reason_codes
        .push(RoutingReason::EscalatedAfterWeakResponse);
    escalated_decision.override_source = OverrideSource::ExecutionManager;

    let escalated_response = proxy
        .proxy_request(
            escalated_request,
            execution,
            Some(&escalated_decision.thinking_level),
        )
        .await?;

    Ok(ExecutionOutcome {
        response: escalated_response,
        final_decision: escalated_decision,
        escalated: true,
        escalation_reason: Some(EscalationReason::WeakInitialResponse),
    })
}

fn should_escalate(
    response: &ChatCompletionsResponse,
    decision: &RoutingDecision,
    execution: &ExecutionConfig,
) -> bool {
    if !execution.enable_non_streaming_escalation {
        return false;
    }

    if decision.override_source != OverrideSource::Policy {
        return false;
    }

    if next_escalation_alias(&decision.selected_alias).is_none() {
        return false;
    }

    let Some(choice) = response.choices.first() else {
        return true;
    };

    if choice
        .message
        .tool_calls
        .as_ref()
        .is_some_and(|tool_calls| !tool_calls.is_empty())
    {
        return false;
    }

    let content = choice.message.content.as_deref().unwrap_or_default().trim();
    if content.is_empty() {
        return true;
    }

    let lowercase = content.to_lowercase();
    if lowercase.contains("development mode while chatgpt backend integration is being finalized") {
        return false;
    }

    if contains_weak_response_signal(&lowercase) {
        return true;
    }

    decision.selected_alias == "small"
        && content.chars().count() < execution.escalation_min_content_chars / 2
}

fn contains_weak_response_signal(content: &str) -> bool {
    [
        "could you provide more specific details",
        "please describe what you'd like",
        "what would you like to work on",
        "i can help with your request",
        "i'm ready to help with your coding task",
    ]
    .iter()
    .any(|needle| content.contains(needle))
}

fn escalated_thinking(level: &ThinkingLevel) -> ThinkingLevel {
    match level {
        ThinkingLevel::Low => ThinkingLevel::Medium,
        ThinkingLevel::Medium => ThinkingLevel::High,
        ThinkingLevel::High | ThinkingLevel::ExtraHigh => ThinkingLevel::ExtraHigh,
    }
}

fn next_escalation_alias(current_alias: &str) -> Option<&'static str> {
    match current_alias {
        "small" => Some("medium"),
        "medium" => Some("large"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        backend::{
            ChatCompletionsResponse, ChatFunctionCall, ChatResponseMessage, ChatToolCall, Choice,
            Usage,
        },
        config::ExecutionConfig,
        routing::{
            OverrideSource, RoutingDecision, RoutingReason,
            decision::{TaskKind, ThinkingLevel},
        },
    };

    use super::next_escalation_alias;
    use super::should_escalate;

    #[test]
    fn escalates_small_weak_responses() {
        let response = response_with_content("Could you provide more specific details?");
        let decision = small_decision();
        let execution = ExecutionConfig::default();

        assert!(should_escalate(&response, &decision, &execution));
    }

    #[test]
    fn does_not_escalate_large_responses() {
        let response = response_with_content(
            "Here is a detailed fix with concrete steps, tradeoffs, and the exact code changes \
             to apply across the relevant modules.",
        );
        let decision = small_decision();
        let execution = ExecutionConfig::default();

        assert!(!should_escalate(&response, &decision, &execution));
    }

    #[test]
    fn escalation_ladder_is_small_medium_large() {
        assert_eq!(next_escalation_alias("small"), Some("medium"));
        assert_eq!(next_escalation_alias("medium"), Some("large"));
        assert_eq!(next_escalation_alias("large"), None);
    }

    #[test]
    fn does_not_escalate_medium_brief_but_valid_responses() {
        let response = response_with_content("Hello!");
        let decision = medium_decision();
        let execution = ExecutionConfig::default();

        assert!(!should_escalate(&response, &decision, &execution));
    }

    #[test]
    fn still_escalates_medium_weak_responses() {
        let response = response_with_content("Could you provide more specific details?");
        let decision = policy_medium_decision();
        let execution = ExecutionConfig::default();

        assert!(should_escalate(&response, &decision, &execution));
    }

    #[test]
    fn does_not_escalate_explicit_alias_even_if_response_is_weak() {
        let response = response_with_content("Could you provide more specific details?");
        let decision = medium_decision();
        let execution = ExecutionConfig::default();

        assert!(!should_escalate(&response, &decision, &execution));
    }

    #[test]
    fn does_not_escalate_valid_tool_call_responses() {
        let response = ChatCompletionsResponse {
            id: "chatcmpl-test".to_string(),
            object: "chat.completion".to_string(),
            created: 0,
            model: "gpt-5.3-codex".to_string(),
            choices: vec![Choice {
                index: 0,
                message: ChatResponseMessage {
                    role: "assistant".to_string(),
                    content: None,
                    tool_calls: Some(vec![ChatToolCall {
                        id: "call_123".to_string(),
                        call_type: "function".to_string(),
                        function: ChatFunctionCall {
                            name: "get_weather".to_string(),
                            arguments: "{\"city\":\"Makassar\"}".to_string(),
                        },
                    }]),
                },
                finish_reason: Some("tool_calls".to_string()),
            }],
            usage: Some(Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            }),
        };
        let decision = small_decision();
        let execution = ExecutionConfig::default();

        assert!(!should_escalate(&response, &decision, &execution));
    }

    fn response_with_content(content: &str) -> ChatCompletionsResponse {
        ChatCompletionsResponse {
            id: "chatcmpl-test".to_string(),
            object: "chat.completion".to_string(),
            created: 0,
            model: "gpt-4".to_string(),
            choices: vec![Choice {
                index: 0,
                message: ChatResponseMessage {
                    role: "assistant".to_string(),
                    content: Some(content.to_string()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: Some(Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            }),
        }
    }

    fn small_decision() -> RoutingDecision {
        RoutingDecision {
            selected_alias: "small".to_string(),
            backend_model: "gpt-5.1-codex-mini".to_string(),
            thinking_level: ThinkingLevel::Low,
            task_kind: TaskKind::Chat,
            reason_codes: vec![RoutingReason::ExplicitAlias],
            override_source: OverrideSource::Policy,
        }
    }

    fn medium_decision() -> RoutingDecision {
        RoutingDecision {
            selected_alias: "medium".to_string(),
            backend_model: "gpt-5.3-codex".to_string(),
            thinking_level: ThinkingLevel::Medium,
            task_kind: TaskKind::Chat,
            reason_codes: vec![RoutingReason::ExplicitAlias],
            override_source: OverrideSource::ClientAlias,
        }
    }

    fn policy_medium_decision() -> RoutingDecision {
        RoutingDecision {
            selected_alias: "medium".to_string(),
            backend_model: "gpt-5.3-codex".to_string(),
            thinking_level: ThinkingLevel::Medium,
            task_kind: TaskKind::Chat,
            reason_codes: vec![RoutingReason::MediumTaskKind],
            override_source: OverrideSource::Policy,
        }
    }
}
