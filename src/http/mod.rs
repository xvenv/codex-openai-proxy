pub mod handlers;
pub mod logging;

use warp::Filter;

use crate::app::AppState;

pub fn routes(
    state: AppState,
) -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
    let state_filter = warp::any().map(move || state.clone());

    let cors = warp::cors()
        .allow_any_origin()
        .allow_headers(vec![
            "authorization",
            "proxy-authorization",
            "content-type",
            "accept",
            "accept-encoding",
            "x-api-key",
            "anthropic-version",
            "anthropic-beta",
            "x-codex-thinking",
            "x-codex-routing-mode",
            "x-stainless-arch",
            "x-stainless-lang",
            "x-stainless-os",
            "x-stainless-package-version",
            "x-stainless-retry-count",
            "x-stainless-runtime",
            "x-stainless-runtime-version",
            "x-stainless-timeout",
        ])
        .allow_methods(vec!["GET", "POST", "PUT", "DELETE", "OPTIONS"]);

    warp::any()
        .and(warp::method())
        .and(warp::path::full())
        .and(warp::header::headers_cloned())
        .and(warp::body::bytes())
        .and(state_filter)
        .and_then(handlers::universal_request_handler)
        .with(cors)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::Value;
    use warp::{http::StatusCode, test::request};

    use crate::{
        app::AppState,
        backend::{
            ChatCompletionsResponse, ChatFunctionCall, ChatResponseMessage, ChatToolCall, Choice,
            ProxyServer, Usage,
        },
        config::{ExecutionConfig, RoutingPolicyConfig},
        models::ModelRegistry,
    };

    #[tokio::test]
    async fn models_endpoint_returns_model_list() {
        let api = super::routes(AppState::for_tests());

        let response = request().method("GET").path("/v1/models").reply(&api).await;

        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(response.body()).expect("models response should be JSON");
        assert_eq!(body["object"], "list");
        let ids = body["data"]
            .as_array()
            .expect("data should be an array")
            .iter()
            .filter_map(|entry| entry.get("id").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert!(ids.contains(&"auto"));
        assert!(ids.contains(&"small"));
        assert!(ids.contains(&"medium"));
        assert!(ids.contains(&"large"));
    }

    #[tokio::test]
    async fn invalid_json_returns_openai_style_error() {
        let api = super::routes(AppState::for_tests());

        let response = request()
            .method("POST")
            .path("/v1/chat/completions")
            .header("content-type", "application/json")
            .body("{invalid json")
            .reply(&api)
            .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value =
            serde_json::from_slice(response.body()).expect("error response should be JSON");
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["code"], "invalid_json");
    }

    #[tokio::test]
    async fn anthropic_invalid_json_returns_anthropic_style_error() {
        let api = super::routes(AppState::for_tests());

        let response = request()
            .method("POST")
            .path("/v1/messages")
            .header("content-type", "application/json")
            .body("{invalid json")
            .reply(&api)
            .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value =
            serde_json::from_slice(response.body()).expect("error response should be JSON");
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["type"], "invalid_request_error");
    }

    #[tokio::test]
    async fn non_streaming_completion_returns_openai_compatible_json() {
        let api = super::routes(AppState::for_tests());

        let response = request()
            .method("POST")
            .path("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(r#"{"model":"auto","messages":[{"role":"user","content":"say hello briefly"}]}"#)
            .reply(&api)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-model")
                .and_then(|value| value.to_str().ok()),
            Some("gpt-5.1-codex-mini")
        );

        let body: Value =
            serde_json::from_slice(response.body()).expect("completion response should be JSON");
        assert_eq!(body["object"], "chat.completion");
        assert_eq!(body["choices"][0]["message"]["role"], "assistant");
        assert!(body["usage"]["total_tokens"].as_i64().is_some());
    }

    #[tokio::test]
    async fn anthropic_non_streaming_message_returns_anthropic_shape() {
        let api = super::routes(AppState::for_tests());

        let response = request()
            .method("POST")
            .path("/v1/messages")
            .header("content-type", "application/json")
            .body(
                r#"{"model":"claude-code-default","max_tokens":256,"messages":[{"role":"user","content":"say hello briefly"}]}"#,
            )
            .reply(&api)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-model")
                .and_then(|value| value.to_str().ok()),
            Some("gpt-5.3-codex")
        );

        let body: Value =
            serde_json::from_slice(response.body()).expect("anthropic response should be JSON");
        assert_eq!(body["type"], "message");
        assert_eq!(body["role"], "assistant");
        assert_eq!(body["model"], "claude-code-default");
        assert_eq!(body["stop_reason"], "end_turn");
        assert_eq!(body["content"][0]["type"], "text");
    }

    #[tokio::test]
    async fn anthropic_sonnet_family_falls_back_to_medium_mapping() {
        let api = super::routes(AppState::for_tests());

        let response = request()
            .method("POST")
            .path("/v1/messages")
            .header("content-type", "application/json")
            .body(
                r#"{"model":"claude-sonnet-4-5","max_tokens":256,"messages":[{"role":"user","content":"say hello briefly"}]}"#,
            )
            .reply(&api)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-model")
                .and_then(|value| value.to_str().ok()),
            Some("gpt-5.3-codex")
        );
        assert_eq!(
            response
                .headers()
                .get("x-codex-thinking")
                .and_then(|value| value.to_str().ok()),
            Some("medium")
        );
    }

    #[tokio::test]
    async fn anthropic_custom_exact_mapping_overrides_family_fallback() {
        let api = super::routes(test_state_with_anthropic_mapping(HashMap::from([(
            "claude-opus-4-1".to_string(),
            "medium".to_string(),
        )])));

        let response = request()
            .method("POST")
            .path("/v1/messages")
            .header("content-type", "application/json")
            .body(
                r#"{"model":"claude-opus-4-1","max_tokens":256,"messages":[{"role":"user","content":"say hello briefly"}]}"#,
            )
            .reply(&api)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-model")
                .and_then(|value| value.to_str().ok()),
            Some("gpt-5.3-codex")
        );
    }

    #[tokio::test]
    async fn anthropic_opus_family_raises_thinking_floor_to_high() {
        let api = super::routes(AppState::for_tests());

        let response = request()
            .method("POST")
            .path("/v1/messages")
            .header("content-type", "application/json")
            .body(
                r#"{"model":"claude-opus-4-1","max_tokens":256,"messages":[{"role":"user","content":"say hello briefly"}]}"#,
            )
            .reply(&api)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-thinking")
                .and_then(|value| value.to_str().ok()),
            Some("high")
        );
    }

    #[tokio::test]
    async fn anthropic_explicit_thinking_override_beats_family_floor() {
        let api = super::routes(AppState::for_tests());

        let response = request()
            .method("POST")
            .path("/v1/messages")
            .header("content-type", "application/json")
            .header("x-codex-thinking", "low")
            .body(
                r#"{"model":"claude-opus-4-1","max_tokens":256,"messages":[{"role":"user","content":"say hello briefly"}]}"#,
            )
            .reply(&api)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-thinking")
                .and_then(|value| value.to_str().ok()),
            Some("low")
        );
    }

    #[tokio::test]
    async fn anthropic_request_thinking_enabled_maps_budget_to_medium() {
        let api = super::routes(AppState::for_tests());

        let response = request()
            .method("POST")
            .path("/v1/messages")
            .header("content-type", "application/json")
            .body(
                r#"{"model":"claude-code-default","max_tokens":256,"thinking":{"type":"enabled","budget_tokens":2000},"messages":[{"role":"user","content":"say hello briefly"}]}"#,
            )
            .reply(&api)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-thinking")
                .and_then(|value| value.to_str().ok()),
            Some("medium")
        );
    }

    #[tokio::test]
    async fn anthropic_request_thinking_enabled_maps_large_budget_to_extra_high() {
        let api = super::routes(AppState::for_tests());

        let response = request()
            .method("POST")
            .path("/v1/messages")
            .header("content-type", "application/json")
            .body(
                r#"{"model":"claude-code-default","max_tokens":256,"thinking":{"type":"enabled","budget_tokens":20000},"messages":[{"role":"user","content":"say hello briefly"}]}"#,
            )
            .reply(&api)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-thinking")
                .and_then(|value| value.to_str().ok()),
            Some("extra_high")
        );
    }

    #[tokio::test]
    async fn anthropic_request_thinking_disabled_can_lower_family_floor() {
        let api = super::routes(AppState::for_tests());

        let response = request()
            .method("POST")
            .path("/v1/messages")
            .header("content-type", "application/json")
            .body(
                r#"{"model":"claude-opus-4-1","max_tokens":256,"thinking":{"type":"disabled"},"messages":[{"role":"user","content":"say hello briefly"}]}"#,
            )
            .reply(&api)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-thinking")
                .and_then(|value| value.to_str().ok()),
            Some("low")
        );
    }

    #[tokio::test]
    async fn anthropic_request_accepts_common_optional_fields() {
        let api = super::routes(AppState::for_tests());

        let response = request()
            .method("POST")
            .path("/v1/messages")
            .header("content-type", "application/json")
            .header("anthropic-version", "2023-06-01")
            .body(
                r#"{
                  "model":"claude-code-default",
                  "max_tokens":256,
                  "service_tier":"auto",
                  "metadata":{"user_id":"user-123"},
                  "stop_sequences":["<END>"],
                  "thinking":{"type":"enabled","budget_tokens":2000},
                  "system":[{"type":"text","text":"You are a coding assistant."}],
                  "messages":[{"role":"user","content":"say hello briefly"}]
                }"#,
            )
            .reply(&api)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-model")
                .and_then(|value| value.to_str().ok()),
            Some("gpt-5.3-codex")
        );
        assert_eq!(
            response
                .headers()
                .get("x-codex-thinking")
                .and_then(|value| value.to_str().ok()),
            Some("medium")
        );
    }

    #[tokio::test]
    async fn streaming_completion_returns_chat_completion_chunks() {
        let api = super::routes(AppState::for_tests());

        let response = request()
            .method("POST")
            .path("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(
                r#"{"model":"medium","stream":true,"stream_options":{"include_usage":true},"messages":[{"role":"user","content":"say hello briefly"}]}"#,
            )
            .reply(&api)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        assert_eq!(
            response
                .headers()
                .get("x-codex-model")
                .and_then(|value| value.to_str().ok()),
            Some("gpt-5.3-codex")
        );
        assert_eq!(
            response
                .headers()
                .get("x-codex-escalated")
                .and_then(|value| value.to_str().ok()),
            Some("false")
        );

        let body =
            String::from_utf8(response.body().to_vec()).expect("stream body should be utf-8");
        assert!(body.contains("\"object\":\"chat.completion.chunk\""));
        assert!(body.contains("\"role\":\"assistant\""));
        assert!(body.contains("\"finish_reason\":\"stop\""));
        assert!(body.contains("\"usage\":"));
        assert!(body.contains("data: [DONE]"));
    }

    #[tokio::test]
    async fn anthropic_streaming_messages_return_anthropic_sse_events() {
        let api = super::routes(AppState::for_tests());

        let response = request()
            .method("POST")
            .path("/v1/messages")
            .header("content-type", "application/json")
            .body(
                r#"{"model":"claude-code-default","stream":true,"max_tokens":256,"messages":[{"role":"user","content":"say hello briefly"}]}"#,
            )
            .reply(&api)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        assert_eq!(
            response
                .headers()
                .get("x-codex-model")
                .and_then(|value| value.to_str().ok()),
            Some("gpt-5.3-codex")
        );

        let body =
            String::from_utf8(response.body().to_vec()).expect("stream body should be utf-8");
        assert!(body.contains("event: message_start"));
        assert!(body.contains("event: content_block_start"));
        assert!(body.contains("event: content_block_delta"));
        assert!(body.contains("event: content_block_stop"));
        assert!(body.contains("event: message_delta"));
        assert!(body.contains("event: message_stop"));
        assert!(!body.contains("data: [DONE]"));
    }

    #[tokio::test]
    async fn non_streaming_completion_exposes_escalation_headers() {
        let api = super::routes(test_state_with_proxy(
            ExecutionConfig {
                prefer_real_backend: false,
                fallback_to_stub: true,
                enable_non_streaming_escalation: true,
                escalation_min_content_chars: 10_000,
            },
            ProxyServer::for_tests_with_stub_message("ok"),
        ));

        let response = request()
            .method("POST")
            .path("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(r#"{"model":"auto","messages":[{"role":"user","content":"say hello briefly"}]}"#)
            .reply(&api)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-escalated")
                .and_then(|value| value.to_str().ok()),
            Some("true")
        );
        assert_eq!(
            response
                .headers()
                .get("x-codex-escalation-reason")
                .and_then(|value| value.to_str().ok()),
            Some("weak_initial_response")
        );
        assert_eq!(
            response
                .headers()
                .get("x-codex-model")
                .and_then(|value| value.to_str().ok()),
            Some("gpt-5.3-codex")
        );

        let body: Value =
            serde_json::from_slice(response.body()).expect("completion response should be JSON");
        assert_eq!(body["model"], "gpt-5.3-codex");
    }

    #[tokio::test]
    async fn non_streaming_tool_call_response_uses_openai_shape() {
        let api = super::routes(test_state_with_proxy(
            ExecutionConfig {
                prefer_real_backend: false,
                fallback_to_stub: true,
                enable_non_streaming_escalation: true,
                escalation_min_content_chars: 10_000,
            },
            ProxyServer::for_tests_with_stub_response(ChatCompletionsResponse {
                id: "chatcmpl-tool".to_string(),
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
                    prompt_tokens: 12,
                    completion_tokens: 5,
                    total_tokens: 17,
                }),
            }),
        ));

        let response = request()
            .method("POST")
            .path("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(
                r#"{"model":"medium","messages":[{"role":"user","content":"Call the weather tool for Makassar."}],"tools":[{"type":"function","function":{"name":"get_weather","parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}}}]}"#,
            )
            .reply(&api)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-escalated")
                .and_then(|value| value.to_str().ok()),
            Some("false")
        );

        let body: Value =
            serde_json::from_slice(response.body()).expect("completion response should be JSON");
        assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
        assert!(body["choices"][0]["message"]["content"].is_null());
        assert_eq!(
            body["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "get_weather"
        );
    }

    #[tokio::test]
    async fn anthropic_tool_use_response_uses_anthropic_shape() {
        let api = super::routes(test_state_with_proxy(
            ExecutionConfig {
                prefer_real_backend: false,
                fallback_to_stub: true,
                enable_non_streaming_escalation: false,
                escalation_min_content_chars: 160,
            },
            ProxyServer::for_tests_with_stub_response(ChatCompletionsResponse {
                id: "msg-tool".to_string(),
                object: "chat.completion".to_string(),
                created: 0,
                model: "gpt-5.3-codex".to_string(),
                choices: vec![Choice {
                    index: 0,
                    message: ChatResponseMessage {
                        role: "assistant".to_string(),
                        content: Some("Checking now.".to_string()),
                        tool_calls: Some(vec![ChatToolCall {
                            id: "toolu_123".to_string(),
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
                    prompt_tokens: 12,
                    completion_tokens: 9,
                    total_tokens: 21,
                }),
            }),
        ));

        let response = request()
            .method("POST")
            .path("/v1/messages")
            .header("content-type", "application/json")
            .body(
                r#"{"model":"claude-code-default","max_tokens":256,"messages":[{"role":"user","content":"Check the weather in Makassar"}],"tools":[{"name":"get_weather","description":"Get weather","input_schema":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}}]}"#,
            )
            .reply(&api)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(response.body()).expect("anthropic response should be JSON");
        assert_eq!(body["type"], "message");
        assert_eq!(body["stop_reason"], "tool_use");
        assert_eq!(body["content"][0]["type"], "text");
        assert_eq!(body["content"][1]["type"], "tool_use");
        assert_eq!(body["content"][1]["name"], "get_weather");
    }

    #[tokio::test]
    async fn non_streaming_mixed_text_and_tool_calls_preserves_both_fields() {
        let api = super::routes(test_state_with_proxy(
            ExecutionConfig {
                prefer_real_backend: false,
                fallback_to_stub: true,
                enable_non_streaming_escalation: true,
                escalation_min_content_chars: 10_000,
            },
            ProxyServer::for_tests_with_stub_response(ChatCompletionsResponse {
                id: "chatcmpl-mixed-tool".to_string(),
                object: "chat.completion".to_string(),
                created: 0,
                model: "gpt-5.3-codex".to_string(),
                choices: vec![Choice {
                    index: 0,
                    message: ChatResponseMessage {
                        role: "assistant".to_string(),
                        content: Some("Checking now.".to_string()),
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
                    prompt_tokens: 16,
                    completion_tokens: 9,
                    total_tokens: 25,
                }),
            }),
        ));

        let response = request()
            .method("POST")
            .path("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(
                r#"{"model":"medium","messages":[{"role":"user","content":"Before calling the weather tool, say you are checking now."}],"tools":[{"type":"function","function":{"name":"get_weather","parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}}}]}"#,
            )
            .reply(&api)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(response.body()).expect("completion response should be JSON");

        assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(body["choices"][0]["message"]["content"], "Checking now.");
        assert_eq!(
            body["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "get_weather"
        );
    }

    #[tokio::test]
    async fn accepts_tool_result_follow_up_requests() {
        let api = super::routes(test_state_with_proxy(
            ExecutionConfig {
                prefer_real_backend: false,
                fallback_to_stub: true,
                enable_non_streaming_escalation: false,
                escalation_min_content_chars: 160,
            },
            ProxyServer::for_tests_with_stub_response(ChatCompletionsResponse {
                id: "chatcmpl-followup".to_string(),
                object: "chat.completion".to_string(),
                created: 0,
                model: "gpt-5.3-codex".to_string(),
                choices: vec![Choice {
                    index: 0,
                    message: ChatResponseMessage {
                        role: "assistant".to_string(),
                        content: Some("The weather in Makassar is 30C.".to_string()),
                        tool_calls: None,
                    },
                    finish_reason: Some("stop".to_string()),
                }],
                usage: Some(Usage {
                    prompt_tokens: 24,
                    completion_tokens: 8,
                    total_tokens: 32,
                }),
            }),
        ));

        let response = request()
            .method("POST")
            .path("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(
                r#"{"model":"medium","messages":[{"role":"user","content":"Check the weather in Makassar."},{"role":"assistant","content":null,"tool_calls":[{"id":"call_123","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Makassar\"}"}}]},{"role":"tool","tool_call_id":"call_123","content":"{\"city\":\"Makassar\",\"temp_c\":30}"}]}"#,
            )
            .reply(&api)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-model")
                .and_then(|value| value.to_str().ok()),
            Some("gpt-5.3-codex")
        );

        let body: Value =
            serde_json::from_slice(response.body()).expect("completion response should be JSON");
        assert_eq!(body["choices"][0]["message"]["role"], "assistant");
        assert_eq!(
            body["choices"][0]["message"]["content"],
            "The weather in Makassar is 30C."
        );
        assert_eq!(body["choices"][0]["finish_reason"], "stop");
    }

    #[tokio::test]
    async fn anthropic_tool_result_follow_up_is_accepted() {
        let api = super::routes(test_state_with_proxy(
            ExecutionConfig {
                prefer_real_backend: false,
                fallback_to_stub: true,
                enable_non_streaming_escalation: false,
                escalation_min_content_chars: 160,
            },
            ProxyServer::for_tests_with_stub_response(ChatCompletionsResponse {
                id: "msg-followup".to_string(),
                object: "chat.completion".to_string(),
                created: 0,
                model: "gpt-5.3-codex".to_string(),
                choices: vec![Choice {
                    index: 0,
                    message: ChatResponseMessage {
                        role: "assistant".to_string(),
                        content: Some("The weather in Makassar is 30C.".to_string()),
                        tool_calls: None,
                    },
                    finish_reason: Some("stop".to_string()),
                }],
                usage: Some(Usage {
                    prompt_tokens: 24,
                    completion_tokens: 8,
                    total_tokens: 32,
                }),
            }),
        ));

        let response = request()
            .method("POST")
            .path("/v1/messages")
            .header("content-type", "application/json")
            .body(
                r#"{"model":"claude-code-default","max_tokens":256,"messages":[{"role":"user","content":[{"type":"text","text":"Check the weather in Makassar."}]},{"role":"assistant","content":[{"type":"tool_use","id":"toolu_123","name":"get_weather","input":{"city":"Makassar"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_123","content":"{\"city\":\"Makassar\",\"temp_c\":30}"}]}]}"#,
            )
            .reply(&api)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(response.body()).expect("anthropic response should be JSON");
        assert_eq!(body["type"], "message");
        assert_eq!(body["stop_reason"], "end_turn");
        assert_eq!(body["content"][0]["type"], "text");
        assert_eq!(
            body["content"][0]["text"],
            "The weather in Makassar is 30C."
        );
    }

    fn test_state_with_proxy(execution: ExecutionConfig, proxy: ProxyServer) -> AppState {
        AppState {
            proxy,
            model_registry: ModelRegistry::default(),
            routing: RoutingPolicyConfig::default(),
            execution,
            anthropic_mapping: HashMap::from([
                ("claude-code-fast".to_string(), "small".to_string()),
                ("claude-code-default".to_string(), "medium".to_string()),
                ("claude-code-max".to_string(), "large".to_string()),
            ]),
        }
    }

    fn test_state_with_anthropic_mapping(anthropic_mapping: HashMap<String, String>) -> AppState {
        AppState {
            proxy: ProxyServer::for_tests(),
            model_registry: ModelRegistry::default(),
            routing: RoutingPolicyConfig::default(),
            execution: ExecutionConfig {
                prefer_real_backend: false,
                fallback_to_stub: true,
                enable_non_streaming_escalation: false,
                escalation_min_content_chars: 160,
            },
            anthropic_mapping,
        }
    }
}
