use bytes::Bytes;
use futures_util::{StreamExt, stream};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::convert::Infallible;
use warp::{
    Reply,
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    hyper::Body,
    path::FullPath,
    reject::Rejection,
    reply::{self, Response},
};

use crate::{
    app::AppState,
    backend::{ChatCompletionsRequest, ChatStreamOptions, ProxyError, StreamingResponse},
    execution,
    http::logging,
    routing::{self, RoutingDecision},
};

pub async fn universal_request_handler(
    method: Method,
    path: FullPath,
    headers: HeaderMap,
    body: Bytes,
    state: AppState,
) -> Result<Response, Rejection> {
    let path_str = path.as_str();
    logging::log_request(&method, path_str, &headers);

    match (method.as_str(), path_str) {
        ("GET", "/health") => Ok(reply::json(&json!({
            "status": "ok",
            "service": "codex-openai-proxy"
        }))
        .into_response()),
        ("GET", "/models") | ("GET", "/v1/models") => {
            let response = state.model_registry.list_response();
            Ok(reply::json(&response).into_response())
        }
        ("POST", "/chat/completions") | ("POST", "/v1/chat/completions") => {
            handle_chat_completions(path_str.to_string(), headers, body, state).await
        }
        ("POST", "/messages") | ("POST", "/v1/messages") => {
            handle_anthropic_messages(path_str.to_string(), headers, body, state).await
        }
        _ => Ok(reply::with_status("Not found", StatusCode::NOT_FOUND).into_response()),
    }
}

async fn handle_chat_completions(
    path: String,
    headers: HeaderMap,
    body: Bytes,
    state: AppState,
) -> Result<Response, Rejection> {
    let chat_req: ChatCompletionsRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => {
            warn!("event=request.invalid_json error={error}");
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid JSON request body",
                "invalid_request_error",
                "invalid_json",
                None,
            ));
        }
    };

    logging::log_chat_request_details(&path, &headers, &body, &chat_req);
    let decision =
        routing::policy::decide(&chat_req, &headers, &state.model_registry, &state.routing);
    info!(
        "event=routing.decision alias={} backend_model={} thinking={} task={} reasons={:?}",
        decision.selected_alias,
        decision.backend_model,
        decision.thinking_level.as_str(),
        decision.task_kind.as_str(),
        decision
            .reason_codes
            .iter()
            .map(|reason| reason.as_str())
            .collect::<Vec<_>>()
    );

    if chat_req.stream.unwrap_or(false) {
        let mut routed_request = chat_req;
        routed_request.model = decision.backend_model.clone();

        return match state
            .proxy
            .proxy_streaming_request(
                routed_request,
                &state.execution,
                Some(&decision.thinking_level),
            )
            .await
        {
            Ok(stream) => Ok(streaming_response(stream, &decision)),
            Err(error) => {
                error!("event=proxy.streaming_error error={error:#}");
                Ok(proxy_error_response(&error, Some(&decision)))
            }
        };
    }

    let mut routed_request = chat_req;
    routed_request.model = decision.backend_model.clone();

    match execution::execute_chat_completion(
        &state.proxy,
        &state.model_registry,
        &state.execution,
        routed_request,
        decision.clone(),
    )
    .await
    {
        Ok(outcome) => {
            let reply = reply::json(&outcome.response);
            let reply = reply::with_header(reply, "content-type", "application/json");
            let reply = reply::with_header(reply, "access-control-allow-origin", "*");
            Ok(with_routing_headers(
                reply.into_response(),
                &outcome.final_decision,
                outcome.escalated,
                outcome.escalation_reason.as_ref(),
            ))
        }
        Err(error) => {
            error!("event=proxy.error error={error:#}");
            Ok(proxy_error_response(&error, Some(&decision)))
        }
    }
}

async fn handle_anthropic_messages(
    path: String,
    headers: HeaderMap,
    body: Bytes,
    state: AppState,
) -> Result<Response, Rejection> {
    let anthropic_req: AnthropicMessagesRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => {
            warn!("event=request.invalid_json protocol=anthropic error={error}");
            return Ok(anthropic_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "invalid JSON request body",
                None,
            ));
        }
    };
    info!(
        "event=request.accepted protocol=anthropic stream={} has_metadata={} stop_sequence_count={} service_tier={} has_thinking={}",
        anthropic_req.stream.unwrap_or(false),
        anthropic_req.metadata.is_some(),
        anthropic_req
            .stop_sequences
            .as_ref()
            .map_or(0usize, std::vec::Vec::len),
        anthropic_req.service_tier.as_deref().unwrap_or("none"),
        anthropic_req.thinking.is_some()
    );

    let chat_req = match anthropic_to_chat_request(&anthropic_req, &state.anthropic_mapping) {
        Ok(request) => request,
        Err(message) => {
            return Ok(anthropic_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                &message,
                None,
            ));
        }
    };

    logging::log_chat_request_details(&path, &headers, &body, &chat_req);
    let decision =
        routing::policy::decide(&chat_req, &headers, &state.model_registry, &state.routing);
    let decision = apply_anthropic_thinking_preferences(decision, &headers, &anthropic_req);
    info!(
        "event=routing.decision protocol=anthropic alias={} backend_model={} thinking={} task={} reasons={:?}",
        decision.selected_alias,
        decision.backend_model,
        decision.thinking_level.as_str(),
        decision.task_kind.as_str(),
        decision
            .reason_codes
            .iter()
            .map(|reason| reason.as_str())
            .collect::<Vec<_>>()
    );

    let mut routed_request = chat_req;
    routed_request.model = decision.backend_model.clone();
    routed_request.stream = anthropic_req.stream;
    if anthropic_req.stream.unwrap_or(false) {
        routed_request.stream_options = Some(ChatStreamOptions {
            include_usage: true,
        });
    }

    if anthropic_req.stream.unwrap_or(false) {
        return match state
            .proxy
            .proxy_streaming_request(
                routed_request,
                &state.execution,
                Some(&decision.thinking_level),
            )
            .await
        {
            Ok(stream) => Ok(anthropic_streaming_response(
                stream,
                &decision,
                anthropic_req.model.clone(),
            )),
            Err(error) => {
                error!("event=proxy.streaming_error protocol=anthropic error={error:#}");
                Ok(anthropic_proxy_error_response(&error, Some(&decision)))
            }
        };
    }

    match execution::execute_chat_completion(
        &state.proxy,
        &state.model_registry,
        &state.execution,
        routed_request,
        decision.clone(),
    )
    .await
    {
        Ok(outcome) => {
            let response = anthropic_from_chat_response(&anthropic_req.model, &outcome.response);
            let reply = reply::json(&response);
            let reply = reply::with_header(reply, "content-type", "application/json");
            let reply = reply::with_header(reply, "access-control-allow-origin", "*");
            Ok(with_routing_headers(
                reply.into_response(),
                &outcome.final_decision,
                outcome.escalated,
                outcome.escalation_reason.as_ref(),
            ))
        }
        Err(error) => {
            error!("event=proxy.error protocol=anthropic error={error:#}");
            Ok(anthropic_proxy_error_response(&error, Some(&decision)))
        }
    }
}

fn proxy_error_response(error: &ProxyError, decision: Option<&RoutingDecision>) -> Response {
    let status =
        StatusCode::from_u16(error.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    error_response(
        status,
        error.message(),
        error.error_type(),
        error.code(),
        decision,
    )
}

fn anthropic_proxy_error_response(
    error: &ProxyError,
    decision: Option<&RoutingDecision>,
) -> Response {
    let status =
        StatusCode::from_u16(error.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    anthropic_error_response(status, error.error_type(), error.message(), decision)
}

fn error_response(
    status: StatusCode,
    message: &str,
    error_type: &str,
    code: &str,
    decision: Option<&RoutingDecision>,
) -> Response {
    let reply = reply::with_status(
        reply::json(&json!({
            "error": {
                "message": message,
                "type": error_type,
                "code": code
            }
        })),
        status,
    );
    let reply = reply::with_header(reply, "content-type", "application/json");
    let reply = reply::with_header(reply, "access-control-allow-origin", "*");
    let response = reply.into_response();

    if let Some(decision) = decision {
        with_routing_headers(response, decision, false, None)
    } else {
        response
    }
}

fn anthropic_error_response(
    status: StatusCode,
    error_type: &str,
    message: &str,
    decision: Option<&RoutingDecision>,
) -> Response {
    let reply = reply::with_status(
        reply::json(&json!({
            "type": "error",
            "error": {
                "type": error_type,
                "message": message
            }
        })),
        status,
    );
    let reply = reply::with_header(reply, "content-type", "application/json");
    let reply = reply::with_header(reply, "access-control-allow-origin", "*");
    let response = reply.into_response();

    if let Some(decision) = decision {
        with_routing_headers(response, decision, false, None)
    } else {
        response
    }
}

fn streaming_response(stream: StreamingResponse, decision: &RoutingDecision) -> Response {
    let mut response = Response::new(Body::wrap_stream(stream));
    let headers = response.headers_mut();
    headers.insert(
        "content-type",
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert("cache-control", HeaderValue::from_static("no-cache"));
    headers.insert("connection", HeaderValue::from_static("keep-alive"));
    headers.insert("access-control-allow-origin", HeaderValue::from_static("*"));
    with_routing_headers(response, decision, false, None)
}

fn anthropic_streaming_response(
    stream: StreamingResponse,
    decision: &RoutingDecision,
    requested_model: String,
) -> Response {
    let translated = translate_openai_to_anthropic_stream(stream, requested_model);
    let mut response = Response::new(Body::wrap_stream(translated));
    let headers = response.headers_mut();
    headers.insert(
        "content-type",
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert("cache-control", HeaderValue::from_static("no-cache"));
    headers.insert("connection", HeaderValue::from_static("keep-alive"));
    headers.insert("access-control-allow-origin", HeaderValue::from_static("*"));
    with_routing_headers(response, decision, false, None)
}

#[derive(Debug, Deserialize)]
struct AnthropicMessagesRequest {
    model: String,
    messages: Vec<AnthropicInputMessage>,
    max_tokens: i32,
    #[serde(default)]
    system: Option<serde_json::Value>,
    #[serde(default)]
    #[allow(dead_code)]
    temperature: Option<f32>,
    #[serde(default)]
    stream: Option<bool>,
    #[serde(default)]
    tools: Option<Vec<AnthropicTool>>,
    #[serde(default)]
    tool_choice: Option<serde_json::Value>,
    #[serde(default)]
    thinking: Option<AnthropicThinkingConfig>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
    #[serde(default)]
    stop_sequences: Option<Vec<String>>,
    #[serde(default)]
    service_tier: Option<String>,
    #[serde(default)]
    response_format: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct AnthropicInputMessage {
    role: String,
    content: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct AnthropicTool {
    name: String,
    #[serde(default)]
    description: Option<String>,
    input_schema: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct AnthropicThinkingConfig {
    #[serde(rename = "type")]
    thinking_type: String,
    #[serde(default)]
    budget_tokens: Option<i32>,
}

#[derive(Debug, Serialize)]
struct AnthropicMessagesResponse {
    id: String,
    #[serde(rename = "type")]
    response_type: &'static str,
    role: &'static str,
    content: Vec<AnthropicContentBlock>,
    model: String,
    stop_reason: Option<String>,
    stop_sequence: Option<String>,
    usage: AnthropicUsage,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

#[derive(Clone, Debug, Default, Serialize)]
struct AnthropicUsage {
    input_tokens: i32,
    output_tokens: i32,
}

struct AnthropicStreamingTranslator {
    buffer: Vec<u8>,
    requested_model: String,
    message_id: Option<String>,
    current_block: AnthropicStreamBlock,
    next_block_index: usize,
    stop_reason: Option<String>,
    usage: AnthropicUsage,
    message_started: bool,
    message_stopped: bool,
}

#[derive(Clone, Debug)]
enum AnthropicStreamBlock {
    None,
    Text { index: usize },
    ToolUse { index: usize },
}

impl AnthropicStreamingTranslator {
    fn new(requested_model: String) -> Self {
        Self {
            buffer: Vec::new(),
            requested_model,
            message_id: None,
            current_block: AnthropicStreamBlock::None,
            next_block_index: 0,
            stop_reason: None,
            usage: AnthropicUsage::default(),
            message_started: false,
            message_stopped: false,
        }
    }

    fn push_chunk(&mut self, chunk: &[u8]) -> Vec<Bytes> {
        self.buffer.extend_from_slice(chunk);
        let mut outputs = Vec::new();
        let mut consumed = 0usize;

        while let Some((frame_end, delimiter_len)) = find_frame_end(&self.buffer[consumed..]) {
            let frame_bytes = &self.buffer[consumed..consumed + frame_end];
            let frame = String::from_utf8_lossy(frame_bytes).into_owned();
            outputs.extend(self.translate_frame(frame.trim_matches('\r')));
            consumed += frame_end + delimiter_len;
        }

        if consumed > 0 {
            self.buffer.drain(..consumed);
        }

        outputs
    }

    fn finish(&mut self) -> Vec<Bytes> {
        let mut outputs = Vec::new();

        if !self.buffer.is_empty() {
            let frame = String::from_utf8_lossy(&self.buffer).into_owned();
            outputs.extend(self.translate_frame(frame.trim_matches('\r')));
            self.buffer.clear();
        }

        outputs.extend(self.flush_message());
        outputs
    }

    fn translate_frame(&mut self, frame: &str) -> Vec<Bytes> {
        let Some(data) = extract_sse_data(frame) else {
            return Vec::new();
        };

        if data == "[DONE]" {
            return self.flush_message();
        }

        let Ok(payload) = serde_json::from_str::<Value>(&data) else {
            return Vec::new();
        };

        let mut outputs = Vec::new();

        if let Some(id) = payload.get("id").and_then(Value::as_str) {
            self.message_id = Some(id.to_string());
        }

        if let Some(usage) = payload.get("usage") {
            if let Some(prompt_tokens) = usage.get("prompt_tokens").and_then(Value::as_i64) {
                self.usage.input_tokens = prompt_tokens as i32;
            }
            if let Some(completion_tokens) = usage.get("completion_tokens").and_then(Value::as_i64)
            {
                self.usage.output_tokens = completion_tokens as i32;
            }
        }

        let Some(choices) = payload.get("choices").and_then(Value::as_array) else {
            return outputs;
        };

        if choices.is_empty() {
            return outputs;
        }

        let Some(choice) = choices.first() else {
            return outputs;
        };

        let delta = choice.get("delta").and_then(Value::as_object);
        if let Some(delta) = delta {
            if delta.get("role").and_then(Value::as_str) == Some("assistant")
                && !self.message_started
            {
                outputs.push(self.message_start_event());
                self.message_started = true;
            }

            if let Some(content) = delta.get("content").and_then(Value::as_str) {
                if !content.is_empty() {
                    outputs.extend(self.ensure_text_block());
                    if let AnthropicStreamBlock::Text { index } = self.current_block {
                        outputs.push(anthropic_sse_event(
                            "content_block_delta",
                            json!({
                                "type": "content_block_delta",
                                "index": index,
                                "delta": {
                                    "type": "text_delta",
                                    "text": content
                                }
                            }),
                        ));
                    }
                }
            }

            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for tool_call in tool_calls {
                    outputs.extend(self.translate_tool_call_delta(tool_call));
                }
            }
        }

        if let Some(finish_reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.stop_reason = Some(map_anthropic_stop_reason(Some(finish_reason)));
        }

        outputs
    }

    fn ensure_text_block(&mut self) -> Vec<Bytes> {
        match self.current_block {
            AnthropicStreamBlock::Text { .. } => Vec::new(),
            _ => {
                let mut outputs = self.close_current_block();
                let index = self.next_block_index;
                self.next_block_index += 1;
                self.current_block = AnthropicStreamBlock::Text { index };
                outputs.push(anthropic_sse_event(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "text",
                            "text": ""
                        }
                    }),
                ));
                outputs
            }
        }
    }

    fn translate_tool_call_delta(&mut self, tool_call: &Value) -> Vec<Bytes> {
        let mut outputs = Vec::new();
        if !self.message_started {
            outputs.push(self.message_start_event());
            self.message_started = true;
        }
        let has_start = tool_call.get("id").and_then(Value::as_str).is_some()
            || tool_call
                .get("function")
                .and_then(Value::as_object)
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .is_some();

        if has_start {
            outputs.extend(self.close_current_block());
            let index = self.next_block_index;
            self.next_block_index += 1;
            self.current_block = AnthropicStreamBlock::ToolUse { index };
            outputs.push(anthropic_sse_event(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": {
                        "type": "tool_use",
                        "id": tool_call.get("id").and_then(Value::as_str).unwrap_or_default(),
                        "name": tool_call
                            .get("function")
                            .and_then(Value::as_object)
                            .and_then(|function| function.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                        "input": {}
                    }
                }),
            ));
        }

        if let Some(arguments) = tool_call
            .get("function")
            .and_then(Value::as_object)
            .and_then(|function| function.get("arguments"))
            .and_then(Value::as_str)
        {
            if !arguments.is_empty() {
                if let AnthropicStreamBlock::ToolUse { index } = self.current_block {
                    outputs.push(anthropic_sse_event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": {
                                "type": "input_json_delta",
                                "partial_json": arguments
                            }
                        }),
                    ));
                }
            }
        }

        outputs
    }

    fn close_current_block(&mut self) -> Vec<Bytes> {
        match self.current_block {
            AnthropicStreamBlock::None => Vec::new(),
            AnthropicStreamBlock::Text { index } | AnthropicStreamBlock::ToolUse { index } => {
                self.current_block = AnthropicStreamBlock::None;
                vec![anthropic_sse_event(
                    "content_block_stop",
                    json!({
                        "type": "content_block_stop",
                        "index": index
                    }),
                )]
            }
        }
    }

    fn flush_message(&mut self) -> Vec<Bytes> {
        if self.message_stopped {
            return Vec::new();
        }

        let mut outputs = Vec::new();

        if !self.message_started {
            outputs.push(self.message_start_event());
            self.message_started = true;
        }

        outputs.extend(self.close_current_block());
        outputs.push(anthropic_sse_event(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": self.stop_reason.clone().unwrap_or_else(|| "end_turn".to_string()),
                    "stop_sequence": null
                },
                "usage": {
                    "input_tokens": self.usage.input_tokens,
                    "output_tokens": self.usage.output_tokens
                }
            }),
        ));
        outputs.push(anthropic_sse_event(
            "message_stop",
            json!({
                "type": "message_stop"
            }),
        ));
        self.message_stopped = true;
        outputs
    }

    fn message_start_event(&self) -> Bytes {
        anthropic_sse_event(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": self.message_id.clone().unwrap_or_else(|| "msg_unknown".to_string()),
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": self.requested_model.clone(),
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": self.usage.input_tokens,
                        "output_tokens": self.usage.output_tokens
                    }
                }
            }),
        )
    }
}

fn translate_openai_to_anthropic_stream(
    mut stream: StreamingResponse,
    requested_model: String,
) -> StreamingResponse {
    let (sender, receiver) = tokio::sync::mpsc::channel::<Bytes>(32);

    tokio::spawn(async move {
        let mut translator = AnthropicStreamingTranslator::new(requested_model);

        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    for translated in translator.push_chunk(&chunk) {
                        if sender.send(translated).await.is_err() {
                            return;
                        }
                    }
                }
                Err(_) => {
                    for translated in translator.finish() {
                        if sender.send(translated).await.is_err() {
                            return;
                        }
                    }
                    return;
                }
            }
        }

        for translated in translator.finish() {
            if sender.send(translated).await.is_err() {
                return;
            }
        }
    });

    let stream = stream::unfold(receiver, |mut receiver| async {
        receiver
            .recv()
            .await
            .map(|chunk| (Ok::<Bytes, Infallible>(chunk), receiver))
    });

    Box::pin(stream)
}

fn anthropic_sse_event(event: &str, payload: Value) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {payload}\n\n"))
}

fn find_frame_end(buffer: &[u8]) -> Option<(usize, usize)> {
    let mut index = 0usize;
    while index + 1 < buffer.len() {
        if buffer[index] == b'\n' && buffer[index + 1] == b'\n' {
            return Some((index, 2));
        }
        if index + 3 < buffer.len()
            && buffer[index] == b'\r'
            && buffer[index + 1] == b'\n'
            && buffer[index + 2] == b'\r'
            && buffer[index + 3] == b'\n'
        {
            return Some((index, 4));
        }
        index += 1;
    }
    None
}

fn extract_sse_data(frame: &str) -> Option<String> {
    let mut data_lines = Vec::new();

    for line in frame.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            data_lines.push(data);
        } else if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start());
        }
    }

    if data_lines.is_empty() {
        None
    } else {
        Some(data_lines.join("\n"))
    }
}

fn anthropic_to_chat_request(
    request: &AnthropicMessagesRequest,
    anthropic_mapping: &HashMap<String, String>,
) -> Result<ChatCompletionsRequest, String> {
    let mut messages = Vec::new();

    if let Some(system) = request.system.as_ref() {
        let system_text = flatten_anthropic_text(system)?;
        if !system_text.is_empty() {
            messages.push(crate::backend::ChatMessage {
                role: "system".to_string(),
                content: serde_json::Value::String(system_text),
                tool_calls: None,
                tool_call_id: None,
            });
        }
    }

    for message in &request.messages {
        messages.extend(anthropic_message_to_chat_messages(message)?);
    }

    Ok(ChatCompletionsRequest {
        model: resolve_anthropic_model(&request.model, anthropic_mapping),
        messages,
        temperature: None,
        max_tokens: Some(request.max_tokens),
        stream: Some(false),
        stream_options: None,
        tools: request
            .tools
            .as_ref()
            .map(|tools| convert_anthropic_tools(tools.as_slice())),
        parallel_tool_calls: Some(true),
        tool_choice: normalize_anthropic_tool_choice(request.tool_choice.as_ref()),
        response_format: request.response_format.clone(),
    })
}

fn anthropic_message_to_chat_messages(
    message: &AnthropicInputMessage,
) -> Result<Vec<crate::backend::ChatMessage>, String> {
    if let Some(text) = message.content.as_str() {
        return Ok(vec![crate::backend::ChatMessage {
            role: message.role.clone(),
            content: serde_json::Value::String(text.to_string()),
            tool_calls: None,
            tool_call_id: None,
        }]);
    }

    let blocks = message
        .content
        .as_array()
        .ok_or_else(|| "Anthropic content must be a string or array".to_string())?;

    match message.role.as_str() {
        "assistant" => assistant_blocks_to_chat_message(blocks),
        "user" => user_blocks_to_chat_messages(blocks),
        role => Ok(vec![crate::backend::ChatMessage {
            role: role.to_string(),
            content: serde_json::Value::String(flatten_anthropic_text(&message.content)?),
            tool_calls: None,
            tool_call_id: None,
        }]),
    }
}

fn assistant_blocks_to_chat_message(
    blocks: &[serde_json::Value],
) -> Result<Vec<crate::backend::ChatMessage>, String> {
    let mut text_blocks = Vec::new();
    let mut tool_calls = Vec::new();

    for block in blocks {
        let Some(object) = block.as_object() else {
            return Err("Anthropic content blocks must be objects".to_string());
        };
        match object.get("type").and_then(serde_json::Value::as_str) {
            Some("text") => text_blocks.push(block.clone()),
            Some("tool_use") => {
                let id = object
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| "Anthropic tool_use block is missing id".to_string())?;
                let name = object
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| "Anthropic tool_use block is missing name".to_string())?;
                let input = object.get("input").cloned().unwrap_or_else(|| json!({}));
                tool_calls.push(crate::backend::ChatToolCall {
                    id: id.to_string(),
                    call_type: "function".to_string(),
                    function: crate::backend::ChatFunctionCall {
                        name: name.to_string(),
                        arguments: input.to_string(),
                    },
                });
            }
            Some(other) => {
                return Err(format!(
                    "Unsupported Anthropic assistant content block type: {other}"
                ));
            }
            None => return Err("Anthropic content block is missing type".to_string()),
        }
    }

    Ok(vec![crate::backend::ChatMessage {
        role: "assistant".to_string(),
        content: serde_json::Value::Array(text_blocks),
        tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
        tool_call_id: None,
    }])
}

fn user_blocks_to_chat_messages(
    blocks: &[serde_json::Value],
) -> Result<Vec<crate::backend::ChatMessage>, String> {
    let mut messages = Vec::new();
    let mut pending_text = Vec::new();

    for block in blocks {
        let Some(object) = block.as_object() else {
            return Err("Anthropic content blocks must be objects".to_string());
        };
        match object.get("type").and_then(serde_json::Value::as_str) {
            Some("text") => pending_text.push(block.clone()),
            Some("tool_result") => {
                if !pending_text.is_empty() {
                    messages.push(crate::backend::ChatMessage {
                        role: "user".to_string(),
                        content: serde_json::Value::Array(std::mem::take(&mut pending_text)),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }

                let tool_call_id = object
                    .get("tool_use_id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        "Anthropic tool_result block is missing tool_use_id".to_string()
                    })?;
                let content = object
                    .get("content")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                messages.push(crate::backend::ChatMessage {
                    role: "tool".to_string(),
                    content: serde_json::Value::String(flatten_anthropic_text(&content)?),
                    tool_calls: None,
                    tool_call_id: Some(tool_call_id.to_string()),
                });
            }
            Some(other) => {
                return Err(format!(
                    "Unsupported Anthropic user content block type: {other}"
                ));
            }
            None => return Err("Anthropic content block is missing type".to_string()),
        }
    }

    if !pending_text.is_empty() {
        messages.push(crate::backend::ChatMessage {
            role: "user".to_string(),
            content: serde_json::Value::Array(pending_text),
            tool_calls: None,
            tool_call_id: None,
        });
    }

    Ok(messages)
}

fn flatten_anthropic_text(content: &serde_json::Value) -> Result<String, String> {
    match content {
        serde_json::Value::String(text) => Ok(text.clone()),
        serde_json::Value::Null => Ok(String::new()),
        serde_json::Value::Array(items) => Ok(items
            .iter()
            .filter_map(|item| {
                item.as_object()
                    .and_then(|object| object.get("text"))
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned)
                    .or_else(|| item.as_str().map(ToOwned::to_owned))
            })
            .collect::<Vec<_>>()
            .join(" ")),
        other => Ok(other.to_string()),
    }
}

fn convert_anthropic_tools(tools: &[AnthropicTool]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema
                }
            })
        })
        .collect()
}

fn normalize_anthropic_tool_choice(
    tool_choice: Option<&serde_json::Value>,
) -> Option<serde_json::Value> {
    let Some(tool_choice) = tool_choice else {
        return None;
    };
    let Some(object) = tool_choice.as_object() else {
        return Some(tool_choice.clone());
    };
    match object.get("type").and_then(serde_json::Value::as_str) {
        Some("tool") => object
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(|name| {
                json!({
                    "type": "function",
                    "function": { "name": name }
                })
            }),
        Some("any") => Some(json!("required")),
        Some("none") => Some(json!("none")),
        Some("auto") => None,
        _ => Some(tool_choice.clone()),
    }
}

fn anthropic_from_chat_response(
    requested_model: &str,
    response: &crate::backend::ChatCompletionsResponse,
) -> AnthropicMessagesResponse {
    let choice = response
        .choices
        .first()
        .expect("chat completion response should have at least one choice");
    let mut content = Vec::new();

    if let Some(text) = choice.message.content.as_ref() {
        if !text.is_empty() {
            content.push(AnthropicContentBlock::Text { text: text.clone() });
        }
    }
    if let Some(tool_calls) = choice.message.tool_calls.as_ref() {
        content.extend(tool_calls.iter().map(|tool_call| {
            AnthropicContentBlock::ToolUse {
                id: tool_call.id.clone(),
                name: tool_call.function.name.clone(),
                input: serde_json::from_str(&tool_call.function.arguments)
                    .unwrap_or_else(|_| json!({ "raw_arguments": tool_call.function.arguments })),
            }
        }));
    }

    let usage = response
        .usage
        .as_ref()
        .map(|usage| AnthropicUsage {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
        })
        .unwrap_or(AnthropicUsage {
            input_tokens: 0,
            output_tokens: 0,
        });

    AnthropicMessagesResponse {
        id: response.id.clone(),
        response_type: "message",
        role: "assistant",
        content,
        model: requested_model.to_string(),
        stop_reason: Some(map_anthropic_stop_reason(choice.finish_reason.as_deref())),
        stop_sequence: None,
        usage,
    }
}

fn map_anthropic_stop_reason(finish_reason: Option<&str>) -> String {
    match finish_reason {
        Some("tool_calls") => "tool_use".to_string(),
        Some("stop") | None => "end_turn".to_string(),
        Some(other) => other.to_string(),
    }
}

fn resolve_anthropic_model(model: &str, anthropic_mapping: &HashMap<String, String>) -> String {
    if let Some(mapped) = anthropic_mapping.get(model) {
        return mapped.clone();
    }

    let normalized = model.trim().to_ascii_lowercase();
    if let Some(mapped) = anthropic_mapping.get(normalized.as_str()) {
        return mapped.clone();
    }

    if normalized.contains("haiku") {
        return "small".to_string();
    }
    if normalized.contains("sonnet") {
        return "medium".to_string();
    }
    if normalized.contains("opus") {
        return "large".to_string();
    }

    model.to_string()
}

fn apply_anthropic_thinking_preferences(
    mut decision: RoutingDecision,
    headers: &HeaderMap,
    request: &AnthropicMessagesRequest,
) -> RoutingDecision {
    if has_thinking_override(headers) {
        return decision;
    }

    if let Some(explicit) = anthropic_requested_thinking(request.thinking.as_ref()) {
        decision.thinking_level = explicit;
        return decision;
    }

    if let Some(floor) = anthropic_thinking_floor(&request.model) {
        decision.thinking_level = max_thinking_level(decision.thinking_level, floor);
    }

    decision
}

fn anthropic_thinking_floor(model: &str) -> Option<routing::decision::ThinkingLevel> {
    let normalized = model.trim().to_ascii_lowercase();

    match normalized.as_str() {
        "claude-code-fast" => Some(routing::decision::ThinkingLevel::Low),
        "claude-code-default" => Some(routing::decision::ThinkingLevel::Medium),
        "claude-code-max" => Some(routing::decision::ThinkingLevel::High),
        _ if normalized.contains("haiku") => Some(routing::decision::ThinkingLevel::Low),
        _ if normalized.contains("sonnet") => Some(routing::decision::ThinkingLevel::Medium),
        _ if normalized.contains("opus") => Some(routing::decision::ThinkingLevel::High),
        _ => None,
    }
}

fn anthropic_requested_thinking(
    thinking: Option<&AnthropicThinkingConfig>,
) -> Option<routing::decision::ThinkingLevel> {
    let thinking = thinking?;
    match thinking.thinking_type.trim().to_ascii_lowercase().as_str() {
        "disabled" => Some(routing::decision::ThinkingLevel::Low),
        "enabled" => Some(map_anthropic_budget_to_thinking(thinking.budget_tokens)),
        _ => None,
    }
}

fn map_anthropic_budget_to_thinking(
    budget_tokens: Option<i32>,
) -> routing::decision::ThinkingLevel {
    match budget_tokens.unwrap_or(4_000) {
        budget if budget >= 16_000 => routing::decision::ThinkingLevel::ExtraHigh,
        budget if budget >= 4_000 => routing::decision::ThinkingLevel::High,
        _ => routing::decision::ThinkingLevel::Medium,
    }
}

fn has_thinking_override(headers: &HeaderMap) -> bool {
    headers.get("x-codex-thinking").is_some()
}

fn max_thinking_level(
    current: routing::decision::ThinkingLevel,
    floor: routing::decision::ThinkingLevel,
) -> routing::decision::ThinkingLevel {
    if thinking_level_rank(&current) >= thinking_level_rank(&floor) {
        current
    } else {
        floor
    }
}

fn thinking_level_rank(level: &routing::decision::ThinkingLevel) -> u8 {
    match level {
        routing::decision::ThinkingLevel::Low => 0,
        routing::decision::ThinkingLevel::Medium => 1,
        routing::decision::ThinkingLevel::High => 2,
        routing::decision::ThinkingLevel::ExtraHigh => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::{AnthropicMessagesRequest, anthropic_to_chat_request};
    use std::collections::HashMap;

    #[test]
    fn anthropic_temperature_is_not_forwarded_to_backend_request() {
        let request: AnthropicMessagesRequest = serde_json::from_str(
            r#"{
                "model": "claude-3-7-sonnet-20250219",
                "messages": [{"role": "user", "content": "hello"}],
                "max_tokens": 128,
                "temperature": 0.7,
                "response_format": {"type": "json_schema", "json_schema": {"name": "structured", "schema": {"type": "object"}}}
            }"#,
        )
        .expect("Anthropic request should deserialize");

        let chat_request = anthropic_to_chat_request(&request, &HashMap::new())
            .expect("Anthropic request should convert");

        assert_eq!(chat_request.temperature, None);
        assert_eq!(
            chat_request.response_format,
            Some(serde_json::json!({"type":"json_schema","json_schema":{"name":"structured","schema":{"type":"object"}}}))
        );
    }
}

fn with_routing_headers(
    mut response: Response,
    decision: &RoutingDecision,
    escalated: bool,
    escalation_reason: Option<&crate::routing::EscalationReason>,
) -> Response {
    let headers = response.headers_mut();
    if let Ok(value) = decision.selected_alias.parse() {
        headers.insert("x-codex-route", value);
    }
    if let Ok(value) = decision.backend_model.parse() {
        headers.insert("x-codex-model", value);
    }
    if let Ok(value) = decision.thinking_level.as_str().parse() {
        headers.insert("x-codex-thinking", value);
    }
    if let Ok(value) = decision.task_kind.as_str().parse() {
        headers.insert("x-codex-task-kind", value);
    }
    if let Ok(value) = decision.override_source.as_str().parse() {
        headers.insert("x-codex-override-source", value);
    }
    if let Ok(value) = if escalated { "true" } else { "false" }.parse() {
        headers.insert("x-codex-escalated", value);
    }
    if let Some(reason) = escalation_reason {
        if let Ok(value) = reason.as_str().parse() {
            headers.insert("x-codex-escalation-reason", value);
        }
    }
    response
}
