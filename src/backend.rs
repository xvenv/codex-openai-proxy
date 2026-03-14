use anyhow::{Context, Result};
use log::{debug, warn};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, convert::Infallible, pin::Pin};
use thiserror::Error;
use uuid::Uuid;

use crate::config::ExecutionConfig;
use crate::routing::decision::ThinkingLevel;
use bytes::Bytes;
use futures_util::{stream, Stream, StreamExt};

#[derive(Clone, Deserialize, Debug)]
pub struct ChatCompletionsRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<i32>,
    pub stream: Option<bool>,
    pub stream_options: Option<ChatStreamOptions>,
    pub tools: Option<Vec<Value>>,
    pub parallel_tool_calls: Option<bool>,
    pub tool_choice: Option<Value>,
}

#[derive(Clone, Deserialize, Debug)]
pub struct ChatStreamOptions {
    #[serde(default)]
    pub include_usage: bool,
}

#[derive(Clone, Deserialize, Debug)]
pub struct ChatMessage {
    pub role: String,
    pub content: Value,
    #[serde(default)]
    pub tool_calls: Option<Vec<ChatToolCall>>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

#[derive(Clone, Serialize, Debug)]
pub struct ChatCompletionsResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
}

#[derive(Clone, Serialize, Debug)]
pub struct Choice {
    pub index: i32,
    pub message: ChatResponseMessage,
    pub finish_reason: Option<String>,
}

#[derive(Clone, Deserialize, Serialize, Debug)]
pub struct ChatToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: ChatFunctionCall,
}

#[derive(Clone, Deserialize, Serialize, Debug)]
pub struct ChatFunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Serialize, Debug)]
pub struct ChatResponseMessage {
    pub role: String,
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ChatToolCall>>,
}

#[derive(Clone, Serialize, Debug)]
pub struct Usage {
    pub prompt_tokens: i32,
    pub completion_tokens: i32,
    pub total_tokens: i32,
}

#[derive(Serialize, Debug)]
struct ResponsesApiRequest {
    model: String,
    instructions: String,
    input: Vec<ResponseItem>,
    tools: Vec<Value>,
    tool_choice: Value,
    parallel_tool_calls: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<Value>,
    store: bool,
    stream: bool,
    include: Vec<String>,
}

#[derive(Deserialize, Debug)]
struct ResponsesApiResponse {
    id: Option<String>,
    output: Option<Vec<ResponseOutputItem>>,
    usage: Option<ResponsesUsage>,
}

#[derive(Deserialize, Debug)]
struct ResponseOutputItem {
    #[allow(dead_code)]
    #[serde(rename = "type")]
    item_type: Option<String>,
    role: Option<String>,
    content: Option<Vec<ResponseContentItem>>,
    call_id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize, Debug)]
struct ResponseContentItem {
    #[serde(rename = "type")]
    content_type: Option<String>,
    text: Option<String>,
}

#[derive(Clone, Deserialize, Debug)]
struct ResponsesUsage {
    input_tokens: i32,
    output_tokens: i32,
    total_tokens: i32,
}

#[derive(Serialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseItem {
    Message {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        role: String,
        content: Vec<ContentItem>,
    },
    FunctionCall {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
}

#[derive(Serialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentItem {
    InputText { text: String },
    OutputText { text: String },
}

#[derive(Deserialize, Debug, Clone)]
struct AuthData {
    #[serde(rename = "OPENAI_API_KEY")]
    env_api_key: Option<String>,
    api_key: Option<String>,
    access_token: Option<String>,
    account_id: Option<String>,
    tokens: Option<TokenData>,
}

#[cfg(test)]
impl AuthData {
    fn empty() -> Self {
        Self {
            env_api_key: None,
            api_key: None,
            access_token: None,
            account_id: None,
            tokens: None,
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
struct TokenData {
    access_token: String,
    account_id: String,
    #[allow(dead_code)]
    refresh_token: Option<String>,
}

#[derive(Deserialize, Debug)]
struct ResponsesApiEvent {
    #[serde(rename = "type")]
    event_type: Option<String>,
    delta: Option<String>,
    item: Option<Value>,
    response: Option<Value>,
    item_id: Option<String>,
}

#[derive(Clone)]
pub struct ProxyServer {
    client: Client,
    auth_data: AuthData,
    #[cfg(test)]
    test_stub_message: Option<String>,
    #[cfg(test)]
    test_stub_response: Option<ChatCompletionsResponse>,
}

pub type StreamingResponse =
    Pin<Box<dyn Stream<Item = std::result::Result<Bytes, Infallible>> + Send>>;

pub type ProxyResult<T> = std::result::Result<T, ProxyError>;

#[derive(Debug, Error)]
#[error("{message}")]
pub struct ProxyError {
    status_code: u16,
    error_type: String,
    code: String,
    message: String,
}

impl ProxyError {
    fn new(
        status_code: u16,
        error_type: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            status_code,
            error_type: error_type.into(),
            code: code.into(),
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::new(500, "proxy_error", "internal_error", message)
    }

    fn from_backend(status: reqwest::StatusCode, body: &str) -> Self {
        if let Ok(value) = serde_json::from_str::<Value>(body) {
            if let Some(error) = value.get("error") {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("backend request failed")
                    .to_string();
                let error_type = error
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| default_error_type(status))
                    .to_string();
                let code = error
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| default_error_code(status))
                    .to_string();

                return Self::new(status.as_u16(), error_type, code, message);
            }
        }

        let body_preview: String = body.chars().take(300).collect();
        let message = if body_preview.is_empty() {
            format!("backend request failed with status {}", status.as_u16())
        } else {
            format!(
                "backend request failed with status {}: {}",
                status.as_u16(),
                body_preview
            )
        };

        Self::new(
            status.as_u16(),
            default_error_type(status),
            default_error_code(status),
            message,
        )
    }

    pub const fn status_code(&self) -> u16 {
        self.status_code
    }

    pub fn error_type(&self) -> &str {
        &self.error_type
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl ProxyServer {
    pub async fn new(auth_path: &str) -> Result<Self> {
        let auth_content = tokio::fs::read_to_string(auth_path)
            .await
            .context("Failed to read auth.json")?;

        let auth_data: AuthData =
            serde_json::from_str(&auth_content).context("Failed to parse auth.json")?;

        let client = Client::builder()
            .user_agent(
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
            )
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            client,
            auth_data,
            #[cfg(test)]
            test_stub_message: None,
            #[cfg(test)]
            test_stub_response: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        let client = Client::builder()
            .build()
            .expect("test HTTP client should build");

        Self {
            client,
            auth_data: AuthData::empty(),
            test_stub_message: None,
            test_stub_response: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_tests_with_stub_message(message: &str) -> Self {
        let mut server = Self::for_tests();
        server.test_stub_message = Some(message.to_string());
        server
    }

    #[cfg(test)]
    pub(crate) fn for_tests_with_stub_response(response: ChatCompletionsResponse) -> Self {
        let mut server = Self::for_tests();
        server.test_stub_response = Some(response);
        server
    }

    fn convert_chat_to_responses(
        &self,
        chat_req: ChatCompletionsRequest,
        thinking_level: Option<&ThinkingLevel>,
    ) -> ResponsesApiRequest {
        let ChatCompletionsRequest {
            model,
            messages,
            temperature,
            max_tokens,
            stream: _,
            stream_options,
            tools,
            parallel_tool_calls,
            tool_choice,
        } = chat_req;
        let mut input = Vec::new();
        let mut system_instructions = Vec::new();

        for msg in messages {
            if msg.role == "system" {
                let content = flatten_content(&msg.content);
                if !content.is_empty() {
                    system_instructions.push(content);
                }
                continue;
            }

            if msg.role == "assistant" {
                if let Some(tool_calls) = msg.tool_calls.as_ref() {
                    let content = flatten_content(&msg.content);
                    if !content.is_empty() {
                        input.push(ResponseItem::Message {
                            id: None,
                            role: "assistant".to_string(),
                            content: vec![ContentItem::OutputText { text: content }],
                        });
                    }

                    for tool_call in tool_calls {
                        input.push(ResponseItem::FunctionCall {
                            id: None,
                            call_id: tool_call.id.clone(),
                            name: tool_call.function.name.clone(),
                            arguments: tool_call.function.arguments.clone(),
                        });
                    }
                    continue;
                }
            }

            if msg.role == "tool" {
                if let Some(tool_call_id) = msg.tool_call_id.as_ref() {
                    input.push(ResponseItem::FunctionCallOutput {
                        call_id: tool_call_id.clone(),
                        output: flatten_content(&msg.content),
                    });
                    continue;
                }
            }

            let role = msg.role;
            let content = flatten_content(&msg.content);
            input.push(ResponseItem::Message {
                id: None,
                content: vec![message_content_item(&role, content)],
                role,
            });
        }

        let instructions = if system_instructions.is_empty() {
            "You are a helpful AI assistant. Provide clear, accurate, and concise responses to user questions and requests."
                .to_string()
        } else {
            system_instructions.join("\n\n")
        };

        ResponsesApiRequest {
            model,
            instructions,
            input,
            tools: normalize_tools(tools.as_deref().unwrap_or(&[])),
            tool_choice: normalize_tool_choice(tool_choice),
            parallel_tool_calls: parallel_tool_calls.unwrap_or(false),
            temperature,
            max_output_tokens: max_tokens,
            reasoning: thinking_level.map(|level| json!({ "effort": level.backend_effort() })),
            store: false,
            // The Codex backend currently requires SSE responses even when the caller wants a
            // non-streaming Chat Completions response. The proxy consumes the SSE stream and
            // materializes a single JSON completion for non-streaming clients.
            stream: true,
            include: response_includes(stream_options.as_ref()),
        }
    }

    pub async fn proxy_request(
        &self,
        chat_req: ChatCompletionsRequest,
        execution: &ExecutionConfig,
        thinking_level: Option<&ThinkingLevel>,
    ) -> ProxyResult<ChatCompletionsResponse> {
        debug!(
            "event=backend.request model={} stream={:?}",
            chat_req.model,
            chat_req.stream
        );

        if execution.prefer_real_backend {
            match self
                .proxy_request_original(chat_req.clone(), thinking_level)
                .await
            {
                Ok(response) => return Ok(response),
                Err(error) => {
                    warn!("event=backend.request_failed error={error:#}");
                    if !execution.fallback_to_stub {
                        return Err(error);
                    }
                }
            }
        }

        #[cfg(test)]
        if let Some(response) = self.test_stub_response.clone() {
            return Ok(response);
        }

        Ok(self.stub_response(&chat_req.model))
    }

    fn stub_response(&self, model: &str) -> ChatCompletionsResponse {
        ChatCompletionsResponse {
            id: format!("chatcmpl-{}", Uuid::new_v4()),
            object: "chat.completion".to_string(),
            created: chrono::Utc::now().timestamp(),
            model: model.to_string(),
            choices: vec![Choice {
                index: 0,
                message: ChatResponseMessage {
                    role: "assistant".to_string(),
                    content: Some(self.stub_message().to_string()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: Some(Usage {
                prompt_tokens: 50,
                completion_tokens: 30,
                total_tokens: 80,
            }),
        }
    }

    #[allow(dead_code)]
    pub async fn proxy_request_original(
        &self,
        chat_req: ChatCompletionsRequest,
        thinking_level: Option<&ThinkingLevel>,
    ) -> ProxyResult<ChatCompletionsResponse> {
        let responses_req = self.convert_chat_to_responses(chat_req, thinking_level);
        let (response, responses_req) = self.send_backend_request(responses_req).await?;

        let response_text = response.text().await.map_err(|error| {
            ProxyError::internal(format!("failed to read backend response: {error}"))
        })?;
        let parsed_response = if response_text.trim_start().starts_with('{') {
            let response_json: ResponsesApiResponse = serde_json::from_str(&response_text)
                .map_err(|error| {
                    ProxyError::internal(format!("failed to parse backend JSON response: {error}"))
                })?;
            collect_json_response_content(&response_json)
        } else {
            collect_response_content(&response_text)
        };
        if parsed_response.content.is_empty() && parsed_response.tool_calls.is_none() {
            return Err(ProxyError::internal(
                "backend response did not contain assistant text content or tool calls",
            ));
        }
        let response_content = parsed_response.content;

        Ok(ChatCompletionsResponse {
            id: parsed_response
                .id
                .unwrap_or_else(|| format!("chatcmpl-{}", Uuid::new_v4())),
            object: "chat.completion".to_string(),
            created: chrono::Utc::now().timestamp(),
            model: responses_req.model.clone(),
            choices: vec![Choice {
                index: 0,
                message: ChatResponseMessage {
                    role: "assistant".to_string(),
                    content: (!response_content.is_empty()).then_some(response_content),
                    tool_calls: parsed_response.tool_calls,
                },
                finish_reason: Some(parsed_response.finish_reason),
            }],
            usage: parsed_response.usage.or(Some(Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            })),
        })
    }

    pub async fn proxy_streaming_request(
        &self,
        chat_req: ChatCompletionsRequest,
        execution: &ExecutionConfig,
        thinking_level: Option<&ThinkingLevel>,
    ) -> ProxyResult<StreamingResponse> {
        debug!(
            "event=backend.streaming_request model={} stream={:?}",
            chat_req.model,
            chat_req.stream
        );

        let include_usage = chat_req
            .stream_options
            .as_ref()
            .is_some_and(|options| options.include_usage);

        if !execution.prefer_real_backend {
            return Ok(self.stub_streaming_response(&chat_req.model, include_usage));
        }

        let responses_req = self.convert_chat_to_responses(chat_req, thinking_level);
        let fallback_model = responses_req.model.clone();
        let (response, responses_req) = match self.send_backend_request(responses_req).await {
            Ok(result) => result,
            Err(error) => {
                if execution.fallback_to_stub {
                    return Ok(self.stub_streaming_response(&fallback_model, include_usage));
                }
                return Err(error);
            }
        };

        let mut upstream = response.bytes_stream();
        let (sender, receiver) = tokio::sync::mpsc::channel::<Bytes>(32);
        let backend_model = responses_req.model.clone();

        tokio::spawn(async move {
            let mut translator = BackendSseTranslator::new(backend_model, include_usage);

            while let Some(item) = upstream.next().await {
                match item {
                    Ok(chunk) => {
                        for translated in translator.push_chunk(&chunk) {
                            if sender.send(translated).await.is_err() {
                                return;
                            }
                        }
                    }
                    Err(error) => {
                        warn!("event=backend.streaming_read_failed error={error:#}");
                        for translated in translator.finish_with_comment("backend_stream_error") {
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

        Ok(Box::pin(stream))
    }

    fn stub_streaming_response(&self, model: &str, include_usage: bool) -> StreamingResponse {
        let chunk_id = format!("chatcmpl-{}", Uuid::new_v4());
        let created = chrono::Utc::now().timestamp();
        let content = self.stub_message();
        let mut chunks = vec![
            openai_data_chunk(json!({
                "id": chunk_id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": { "role": "assistant" },
                    "finish_reason": null
                }]
            })),
            openai_data_chunk(json!({
                "id": chunk_id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": { "content": content },
                    "finish_reason": null
                }]
            })),
            openai_data_chunk(json!({
                "id": chunk_id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": "stop"
                }]
            })),
        ];

        if include_usage {
            chunks.push(usage_chunk(
                &chunk_id,
                created,
                model,
                &Usage {
                    prompt_tokens: 50,
                    completion_tokens: 30,
                    total_tokens: 80,
                },
            ));
        }
        chunks.push(done_chunk());

        Box::pin(stream::iter(
            chunks.into_iter().map(Ok::<Bytes, Infallible>),
        ))
    }

    fn stub_message(&self) -> &str {
        #[cfg(test)]
        if let Some(message) = self.test_stub_message.as_deref() {
            return message;
        }

        "I can help you with coding tasks! The proxy connection is working well. What would you \
         like assistance with? (Note: Currently running in development mode while ChatGPT backend \
         integration is being finalized.)"
    }

    async fn send_backend_request(
        &self,
        mut responses_req: ResponsesApiRequest,
    ) -> ProxyResult<(reqwest::Response, ResponsesApiRequest)> {
        let mut retried_without_max_output_tokens = false;

        loop {
            debug!(
                "event=backend.call model={} max_output_tokens={:?}",
                responses_req.model,
                responses_req.max_output_tokens
            );

            let response = self
                .build_backend_request(&responses_req)
                .json(&responses_req)
                .send()
                .await
                .map_err(|error| {
                    ProxyError::internal(format!("failed to send request to backend: {error}"))
                })?;

            if response.status().is_success() {
                return Ok((response, responses_req));
            }

            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let error = ProxyError::from_backend(status, &body);

            if !retried_without_max_output_tokens
                && responses_req.max_output_tokens.is_some()
                && should_retry_without_max_output_tokens(&error)
            {
                warn!("event=backend.retry_without_max_output_tokens");
                responses_req.max_output_tokens = None;
                retried_without_max_output_tokens = true;
                continue;
            }

            return Err(error);
        }
    }

    fn build_backend_request(
        &self,
        responses_req: &ResponsesApiRequest,
    ) -> reqwest::RequestBuilder {
        let mut request_builder = self
            .client
            .post("https://chatgpt.com/backend-api/codex/responses")
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .header("Accept-Language", "en-US,en;q=0.9")
            .header("Accept-Encoding", "gzip, deflate, br")
            .header("Referer", "https://chatgpt.com/")
            .header("Origin", "https://chatgpt.com")
            .header("Sec-Fetch-Dest", "empty")
            .header("Sec-Fetch-Mode", "cors")
            .header("Sec-Fetch-Site", "same-origin")
            .header("Cache-Control", "no-cache")
            .header("Pragma", "no-cache")
            .header("DNT", "1")
            .header("OpenAI-Beta", "responses=experimental")
            .header("originator", "codex_cli_rs");

        if let Some(tokens) = &self.auth_data.tokens {
            request_builder =
                request_builder.header("Authorization", format!("Bearer {}", tokens.access_token));
            request_builder =
                request_builder.header("chatgpt-account-id", tokens.account_id.as_str());
        } else if let (Some(access_token), Some(account_id)) =
            (&self.auth_data.access_token, &self.auth_data.account_id)
        {
            request_builder =
                request_builder.header("Authorization", format!("Bearer {access_token}"));
            request_builder = request_builder.header("chatgpt-account-id", account_id.as_str());
        } else if let Some(api_key) = self
            .auth_data
            .api_key
            .as_ref()
            .or(self.auth_data.env_api_key.as_ref())
        {
            request_builder = request_builder.header("Authorization", format!("Bearer {api_key}"));
        }

        let session_id = Uuid::new_v4();
        request_builder
            .header("session_id", session_id.to_string())
            .header("x-codex-target-model", responses_req.model.as_str())
    }
}

fn message_content_item(role: &str, text: String) -> ContentItem {
    match role {
        "assistant" => ContentItem::OutputText { text },
        _ => ContentItem::InputText { text },
    }
}

struct BackendSseTranslator {
    buffer: Vec<u8>,
    chunk_id: String,
    backend_model: String,
    created: i64,
    include_usage: bool,
    final_usage: Option<Usage>,
    usage_sent: bool,
    finish_reason: String,
    tool_call_indices: HashMap<String, usize>,
    role_sent: bool,
    stop_sent: bool,
    done_sent: bool,
}

impl BackendSseTranslator {
    fn new(backend_model: String, include_usage: bool) -> Self {
        Self {
            buffer: Vec::new(),
            chunk_id: format!("chatcmpl-{}", Uuid::new_v4()),
            backend_model,
            created: chrono::Utc::now().timestamp(),
            include_usage,
            final_usage: None,
            usage_sent: false,
            finish_reason: "stop".to_string(),
            tool_call_indices: HashMap::new(),
            role_sent: false,
            stop_sent: false,
            done_sent: false,
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

        if !self.stop_sent {
            outputs.push(self.stop_chunk());
            self.stop_sent = true;
        }
        self.push_usage_chunk(&mut outputs);
        if !self.done_sent {
            outputs.push(done_chunk());
            self.done_sent = true;
        }

        outputs
    }

    fn finish_with_comment(&mut self, comment: &str) -> Vec<Bytes> {
        let mut outputs = Vec::new();

        if !comment.is_empty() {
            outputs.push(Bytes::from(format!(": {comment}\n\n")));
        }

        outputs.extend(self.finish());
        outputs
    }

    fn translate_frame(&mut self, frame: &str) -> Vec<Bytes> {
        let Some(data) = extract_sse_data(frame) else {
            return Vec::new();
        };

        if data == "[DONE]" {
            let mut outputs = Vec::new();
            if !self.stop_sent {
                outputs.push(self.stop_chunk());
                self.stop_sent = true;
            }
            self.push_usage_chunk(&mut outputs);
            if !self.done_sent {
                outputs.push(done_chunk());
                self.done_sent = true;
            }
            return outputs;
        }

        let Ok(event) = serde_json::from_str::<ResponsesApiEvent>(&data) else {
            return Vec::new();
        };

        if let Some(response) = &event.response {
            self.update_metadata(response);
        }

        let mut outputs = Vec::new();
        match event.event_type.as_deref() {
            Some("response.output_text.delta") => {
                if let Some(delta) = event.delta.as_deref() {
                    if !delta.is_empty() {
                        if !self.role_sent {
                            outputs.push(self.role_chunk());
                            self.role_sent = true;
                        }
                        outputs.push(self.content_chunk(delta));
                    }
                }
            }
            Some("response.output_item.added") => {
                if event
                    .item
                    .as_ref()
                    .and_then(|item| item.get("role"))
                    .and_then(Value::as_str)
                    == Some("assistant")
                    && !self.role_sent
                {
                    outputs.push(self.role_chunk());
                    self.role_sent = true;
                }

                if event
                    .item
                    .as_ref()
                    .and_then(|item| item.get("type"))
                    .and_then(Value::as_str)
                    == Some("function_call")
                {
                    outputs.extend(self.tool_call_start_chunks(event.item.as_ref()));
                }
            }
            Some("response.function_call_arguments.delta") => {
                outputs.extend(self.tool_call_argument_delta(&event));
            }
            Some("response.completed") => {
                if !self.stop_sent {
                    outputs.push(self.stop_chunk());
                    self.stop_sent = true;
                }
                self.push_usage_chunk(&mut outputs);
                if !self.done_sent {
                    outputs.push(done_chunk());
                    self.done_sent = true;
                }
            }
            _ => {}
        }

        outputs
    }

    fn update_metadata(&mut self, response: &Value) {
        if let Some(id) = response.get("id").and_then(Value::as_str) {
            self.chunk_id = id.to_string();
        }
        if let Some(model) = response.get("model").and_then(Value::as_str) {
            self.backend_model = model.to_string();
        }
        if let Some(created_at) = response.get("created_at").and_then(Value::as_i64) {
            self.created = created_at;
        }
        if let Some(usage) = response.get("usage").and_then(extract_usage_from_value) {
            self.final_usage = Some(usage);
        }
        if response
            .get("output")
            .and_then(Value::as_array)
            .is_some_and(|output| {
                output
                    .iter()
                    .any(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
            })
        {
            self.finish_reason = "tool_calls".to_string();
        }
    }

    fn push_usage_chunk(&mut self, outputs: &mut Vec<Bytes>) {
        if !self.include_usage || self.usage_sent {
            return;
        }

        if let Some(usage) = self.final_usage.as_ref() {
            outputs.push(usage_chunk(
                &self.chunk_id,
                self.created,
                &self.backend_model,
                usage,
            ));
            self.usage_sent = true;
        }
    }

    fn role_chunk(&self) -> Bytes {
        openai_data_chunk(json!({
            "id": self.chunk_id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.backend_model,
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant" },
                "finish_reason": null
            }]
        }))
    }

    fn content_chunk(&self, content: &str) -> Bytes {
        openai_data_chunk(json!({
            "id": self.chunk_id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.backend_model,
            "choices": [{
                "index": 0,
                "delta": { "content": content },
                "finish_reason": null
            }]
        }))
    }

    fn stop_chunk(&self) -> Bytes {
        openai_data_chunk(json!({
            "id": self.chunk_id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.backend_model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": self.finish_reason
            }]
        }))
    }

    fn tool_call_start_chunks(&mut self, item: Option<&Value>) -> Vec<Bytes> {
        let Some(item) = item else {
            return Vec::new();
        };
        let Some(item_id) = item.get("id").and_then(Value::as_str) else {
            return Vec::new();
        };
        let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
            return Vec::new();
        };
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            return Vec::new();
        };

        let next_index = self.tool_call_indices.len();
        let index = *self
            .tool_call_indices
            .entry(item_id.to_string())
            .or_insert(next_index);
        self.finish_reason = "tool_calls".to_string();

        vec![openai_data_chunk(json!({
            "id": self.chunk_id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.backend_model,
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": index,
                        "id": call_id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": ""
                        }
                    }]
                },
                "finish_reason": null
            }]
        }))]
    }

    fn tool_call_argument_delta(&self, event: &ResponsesApiEvent) -> Vec<Bytes> {
        let Some(item_id) = event.item_id.as_deref() else {
            return Vec::new();
        };
        let Some(index) = self.tool_call_indices.get(item_id) else {
            return Vec::new();
        };
        let Some(delta) = event.delta.as_deref() else {
            return Vec::new();
        };

        vec![openai_data_chunk(json!({
            "id": self.chunk_id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.backend_model,
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": index,
                        "function": {
                            "arguments": delta
                        }
                    }]
                },
                "finish_reason": null
            }]
        }))]
    }
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
    let mut lines = frame.lines();
    let mut data_lines = Vec::new();

    for line in lines.by_ref() {
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

fn openai_data_chunk(payload: Value) -> Bytes {
    Bytes::from(format!("data: {payload}\n\n"))
}

fn usage_chunk(id: &str, created: i64, model: &str, usage: &Usage) -> Bytes {
    openai_data_chunk(json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [],
        "usage": usage,
    }))
}

fn done_chunk() -> Bytes {
    Bytes::from("data: [DONE]\n\n")
}

fn response_includes(stream_options: Option<&ChatStreamOptions>) -> Vec<String> {
    let _ = stream_options;
    Vec::new()
}

fn normalize_tools(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            let Some(object) = tool.as_object() else {
                return tool.clone();
            };

            if object.get("type").and_then(Value::as_str) != Some("function") {
                return tool.clone();
            }

            let Some(function) = object.get("function").and_then(Value::as_object) else {
                return tool.clone();
            };

            let mut normalized = serde_json::Map::new();
            normalized.insert("type".to_string(), json!("function"));

            if let Some(name) = function.get("name") {
                normalized.insert("name".to_string(), name.clone());
            }
            if let Some(description) = function.get("description") {
                normalized.insert("description".to_string(), description.clone());
            }
            if let Some(parameters) = function.get("parameters") {
                normalized.insert(
                    "parameters".to_string(),
                    normalize_json_schema(parameters),
                );
            }
            if let Some(strict) = function.get("strict") {
                normalized.insert("strict".to_string(), strict.clone());
            }

            Value::Object(normalized)
        })
        .collect()
}

fn normalize_json_schema(schema: &Value) -> Value {
    match schema {
        Value::Object(object) => {
            let mut normalized = object.clone();

            if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                normalized.insert(
                    "properties".to_string(),
                    Value::Object(
                        properties
                            .iter()
                            .map(|(key, value)| (key.clone(), normalize_json_schema(value)))
                            .collect(),
                    ),
                );
            } else if object.get("type").and_then(Value::as_str) == Some("object") {
                normalized.insert("properties".to_string(), json!({}));
            }

            if let Some(items) = object.get("items") {
                normalized.insert("items".to_string(), normalize_json_schema(items));
            }

            for key in ["additionalProperties", "not", "if", "then", "else"] {
                if let Some(value) = object.get(key) {
                    normalized.insert(key.to_string(), normalize_json_schema(value));
                }
            }

            for key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
                if let Some(values) = object.get(key).and_then(Value::as_array) {
                    normalized.insert(
                        key.to_string(),
                        Value::Array(values.iter().map(normalize_json_schema).collect()),
                    );
                }
            }

            Value::Object(normalized)
        }
        Value::Array(values) => Value::Array(values.iter().map(normalize_json_schema).collect()),
        _ => schema.clone(),
    }
}

fn normalize_tool_choice(tool_choice: Option<Value>) -> Value {
    let Some(tool_choice) = tool_choice else {
        return json!("auto");
    };

    let Some(object) = tool_choice.as_object() else {
        return tool_choice;
    };

    if object.get("type").and_then(Value::as_str) != Some("function") {
        return tool_choice;
    }

    let Some(function) = object.get("function").and_then(Value::as_object) else {
        return tool_choice;
    };

    let mut normalized = serde_json::Map::new();
    normalized.insert("type".to_string(), json!("function"));
    if let Some(name) = function.get("name") {
        normalized.insert("name".to_string(), name.clone());
    }

    Value::Object(normalized)
}

fn flatten_content(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                item.as_object()
                    .and_then(|object| object.get("text"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .or_else(|| item.as_str().map(ToOwned::to_owned))
            })
            .collect::<Vec<_>>()
            .join(" "),
        other => other.to_string(),
    }
}

fn collect_response_content(response_text: &str) -> ParsedBackendResponse {
    let mut parsed = ParsedBackendResponse {
        id: None,
        content: String::new(),
        tool_calls: None,
        usage: None,
        finish_reason: "stop".to_string(),
    };
    let mut saw_text_delta = false;

    for line in response_text.lines() {
        if let Some(json_data) = line.strip_prefix("data: ") {
            if json_data == "[DONE]" {
                break;
            }

            if let Ok(event) = serde_json::from_str::<ResponsesApiEvent>(json_data) {
                if let Some(response) = &event.response {
                    update_parsed_backend_response(&mut parsed, response);
                }

                match event.event_type.as_deref() {
                    Some("response.output_text.delta") => {
                        if let Some(delta) = event.delta {
                            saw_text_delta = true;
                            parsed.content.push_str(&delta);
                        }
                    }
                    Some("response.output_item.done") => {
                        if !saw_text_delta {
                            if let Some(item) = event.item {
                                if let Some(content_items) =
                                    item.get("content").and_then(Value::as_array)
                                {
                                    for content_item in content_items {
                                        if let Some(text) =
                                            content_item.get("text").and_then(Value::as_str)
                                        {
                                            parsed.content.push_str(text);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    parsed
}

struct ParsedBackendResponse {
    id: Option<String>,
    content: String,
    tool_calls: Option<Vec<ChatToolCall>>,
    usage: Option<Usage>,
    finish_reason: String,
}

fn collect_json_response_content(response: &ResponsesApiResponse) -> ParsedBackendResponse {
    let content = response
        .output
        .as_ref()
        .into_iter()
        .flatten()
        .filter(|item| item.role.as_deref() == Some("assistant"))
        .flat_map(|item| item.content.as_ref().into_iter().flatten())
        .filter(|content| {
            matches!(
                content.content_type.as_deref(),
                Some("output_text" | "text")
            )
        })
        .filter_map(|content| content.text.as_deref())
        .collect::<Vec<_>>()
        .join("");

    ParsedBackendResponse {
        id: response.id.clone(),
        content,
        tool_calls: collect_tool_calls(response.output.as_deref()),
        usage: response.usage.as_ref().map(Usage::from),
        finish_reason: response_finish_reason(response.output.as_deref()),
    }
}

fn collect_tool_calls(output: Option<&[ResponseOutputItem]>) -> Option<Vec<ChatToolCall>> {
    let tool_calls = output
        .into_iter()
        .flatten()
        .filter(|item| item.item_type.as_deref() == Some("function_call"))
        .filter_map(|item| {
            Some(ChatToolCall {
                id: item.call_id.clone()?,
                call_type: "function".to_string(),
                function: ChatFunctionCall {
                    name: item.name.clone()?,
                    arguments: item.arguments.clone().unwrap_or_default(),
                },
            })
        })
        .collect::<Vec<_>>();

    if tool_calls.is_empty() {
        None
    } else {
        Some(tool_calls)
    }
}

fn response_finish_reason(output: Option<&[ResponseOutputItem]>) -> String {
    if output
        .into_iter()
        .flatten()
        .any(|item| item.item_type.as_deref() == Some("function_call"))
    {
        "tool_calls".to_string()
    } else {
        "stop".to_string()
    }
}

fn update_parsed_backend_response(parsed: &mut ParsedBackendResponse, response: &Value) {
    if let Some(id) = response.get("id").and_then(Value::as_str) {
        parsed.id = Some(id.to_string());
    }

    if let Some(usage) = response.get("usage").and_then(extract_usage_from_value) {
        parsed.usage = Some(usage);
    }

    if let Some(output) = response
        .get("output")
        .and_then(|output| serde_json::from_value::<Vec<ResponseOutputItem>>(output.clone()).ok())
    {
        parsed.tool_calls = collect_tool_calls(Some(output.as_slice()));
        parsed.finish_reason = response_finish_reason(Some(output.as_slice()));
    }
}

fn extract_usage_from_value(value: &Value) -> Option<Usage> {
    let input_tokens = value.get("input_tokens")?.as_i64()?;
    let output_tokens = value.get("output_tokens")?.as_i64()?;
    let total_tokens = value.get("total_tokens")?.as_i64()?;

    Some(Usage {
        prompt_tokens: i32::try_from(input_tokens).ok()?,
        completion_tokens: i32::try_from(output_tokens).ok()?,
        total_tokens: i32::try_from(total_tokens).ok()?,
    })
}

fn default_error_type(status: reqwest::StatusCode) -> &'static str {
    match status.as_u16() {
        400 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "invalid_request_error",
        409 => "conflict_error",
        429 => "rate_limit_error",
        500..=599 => "api_error",
        _ => "proxy_error",
    }
}

fn default_error_code(status: reqwest::StatusCode) -> &'static str {
    match status.as_u16() {
        400 => "invalid_request",
        401 => "invalid_api_key",
        403 => "permission_denied",
        404 => "not_found",
        409 => "conflict",
        429 => "rate_limit_exceeded",
        500..=599 => "backend_error",
        _ => "request_failed",
    }
}

fn should_retry_without_max_output_tokens(error: &ProxyError) -> bool {
    error.status_code() == 400
        && error
            .message()
            .contains("Unsupported parameter: max_output_tokens")
}

impl From<&ResponsesUsage> for Usage {
    fn from(value: &ResponsesUsage) -> Self {
        Self {
            prompt_tokens: value.input_tokens,
            completion_tokens: value.output_tokens,
            total_tokens: value.total_tokens,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        collect_json_response_content, collect_response_content, default_error_code,
        default_error_type, should_retry_without_max_output_tokens, BackendSseTranslator,
        ChatCompletionsRequest, ChatFunctionCall, ChatMessage, ChatStreamOptions, ChatToolCall,
        ProxyError, ProxyServer, ResponsesApiResponse,
    };

    #[test]
    fn converts_common_openai_request_fields_for_backend() {
        let server = ProxyServer::for_tests();
        let request = ChatCompletionsRequest {
            model: "gpt-5.3-codex".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: json!("hello"),
                tool_calls: None,
                tool_call_id: None,
            }],
            temperature: Some(0.2),
            max_tokens: Some(256),
            stream: Some(true),
            stream_options: Some(ChatStreamOptions {
                include_usage: true,
            }),
            tools: Some(vec![json!({"type":"function","function":{"name":"do_it"}})]),
            parallel_tool_calls: Some(true),
            tool_choice: Some(json!({"type":"function","function":{"name":"do_it"}})),
        };

        let responses_request = server.convert_chat_to_responses(request, None);
        let payload = serde_json::to_value(responses_request).expect("request should serialize");

        let temperature = payload["temperature"]
            .as_f64()
            .expect("temperature should serialize as a number");
        assert!((temperature - 0.2).abs() < 1e-6);
        assert_eq!(payload["max_output_tokens"], 256);
        assert_eq!(payload["parallel_tool_calls"], true);
        assert_eq!(payload["tool_choice"]["type"], "function");
        assert_eq!(payload["tool_choice"]["name"], "do_it");
        assert_eq!(payload["tools"][0]["name"], "do_it");
        assert_eq!(payload["include"], json!([]));
    }

    #[test]
    fn normalizes_object_tool_schema_without_properties() {
        let server = ProxyServer::for_tests();
        let request = ChatCompletionsRequest {
            model: "gpt-5.3-codex".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: json!("hello"),
                tool_calls: None,
                tool_call_id: None,
            }],
            temperature: None,
            max_tokens: None,
            stream: Some(false),
            stream_options: None,
            tools: Some(vec![json!({
                "type": "function",
                "function": {
                    "name": "empty_schema_tool",
                    "parameters": { "type": "object" }
                }
            })]),
            parallel_tool_calls: None,
            tool_choice: None,
        };

        let responses_request = server.convert_chat_to_responses(request, None);
        let payload = serde_json::to_value(responses_request).expect("request should serialize");

        assert_eq!(payload["tools"][0]["parameters"]["type"], "object");
        assert_eq!(payload["tools"][0]["parameters"]["properties"], json!({}));
    }

    #[test]
    fn maps_extra_high_thinking_to_backend_xhigh_effort() {
        let server = ProxyServer::for_tests();
        let request = ChatCompletionsRequest {
            model: "gpt-5.3-codex".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: json!("hello"),
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
        };

        let responses_request = server.convert_chat_to_responses(
            request,
            Some(&crate::routing::decision::ThinkingLevel::ExtraHigh),
        );
        let payload = serde_json::to_value(responses_request).expect("request should serialize");

        assert_eq!(payload["reasoning"]["effort"], "xhigh");
    }

    #[test]
    fn retries_without_max_output_tokens_for_backend_unsupported_parameter_errors() {
        let error = ProxyError::new(
            400,
            "invalid_request_error",
            "invalid_request",
            "backend request failed with status 400: {\"detail\":\"Unsupported parameter: max_output_tokens\"}".to_string(),
        );

        assert!(should_retry_without_max_output_tokens(&error));
    }

    #[test]
    fn converts_chat_completions_tool_messages_for_backend() {
        let server = ProxyServer::for_tests();
        let request = ChatCompletionsRequest {
            model: "gpt-5.3-codex".to_string(),
            messages: vec![
                ChatMessage {
                    role: "assistant".to_string(),
                    content: json!(null),
                    tool_calls: Some(vec![ChatToolCall {
                        id: "call_123".to_string(),
                        call_type: "function".to_string(),
                        function: ChatFunctionCall {
                            name: "get_weather".to_string(),
                            arguments: "{\"city\":\"Makassar\"}".to_string(),
                        },
                    }]),
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "tool".to_string(),
                    content: json!("{\"temp\":30}"),
                    tool_calls: None,
                    tool_call_id: Some("call_123".to_string()),
                },
            ],
            temperature: None,
            max_tokens: None,
            stream: Some(false),
            stream_options: None,
            tools: None,
            parallel_tool_calls: None,
            tool_choice: None,
        };

        let responses_request = server.convert_chat_to_responses(request, None);
        let payload = serde_json::to_value(responses_request).expect("request should serialize");

        assert_eq!(payload["input"][0]["type"], "function_call");
        assert_eq!(payload["input"][0]["call_id"], "call_123");
        assert_eq!(payload["input"][1]["type"], "function_call_output");
        assert_eq!(payload["input"][1]["call_id"], "call_123");
    }

    #[test]
    fn converts_assistant_history_text_into_output_text_blocks() {
        let server = ProxyServer::for_tests();
        let request = ChatCompletionsRequest {
            model: "gpt-5.3-codex".to_string(),
            messages: vec![
                ChatMessage {
                    role: "user".to_string(),
                    content: json!("First question"),
                    tool_calls: None,
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "assistant".to_string(),
                    content: json!("First answer"),
                    tool_calls: None,
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: json!("Follow-up question"),
                    tool_calls: None,
                    tool_call_id: None,
                },
            ],
            temperature: None,
            max_tokens: None,
            stream: Some(false),
            stream_options: None,
            tools: None,
            parallel_tool_calls: None,
            tool_choice: None,
        };

        let responses_request = server.convert_chat_to_responses(request, None);
        let payload = serde_json::to_value(responses_request).expect("request should serialize");

        assert_eq!(payload["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(payload["input"][1]["content"][0]["type"], "output_text");
        assert_eq!(payload["input"][1]["content"][0]["text"], "First answer");
        assert_eq!(payload["input"][2]["content"][0]["type"], "input_text");
    }

    #[test]
    fn converts_system_messages_into_backend_instructions() {
        let server = ProxyServer::for_tests();
        let request = ChatCompletionsRequest {
            model: "gpt-5.3-codex".to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: json!("You are a coding assistant."),
                    tool_calls: None,
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: json!("hello"),
                    tool_calls: None,
                    tool_call_id: None,
                },
            ],
            temperature: None,
            max_tokens: None,
            stream: Some(false),
            stream_options: None,
            tools: None,
            parallel_tool_calls: None,
            tool_choice: None,
        };

        let responses_request = server.convert_chat_to_responses(request, None);
        let payload = serde_json::to_value(responses_request).expect("request should serialize");

        assert_eq!(payload["instructions"], "You are a coding assistant.");
        assert_eq!(payload["input"].as_array().map_or(0, Vec::len), 1);
        assert_eq!(payload["input"][0]["type"], "message");
        assert_eq!(payload["input"][0]["role"], "user");
    }

    #[test]
    fn preserves_tool_loop_message_order_for_backend_input() {
        let server = ProxyServer::for_tests();
        let request = ChatCompletionsRequest {
            model: "gpt-5.3-codex".to_string(),
            messages: vec![
                ChatMessage {
                    role: "user".to_string(),
                    content: json!("Check the weather in Makassar"),
                    tool_calls: None,
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "assistant".to_string(),
                    content: json!(null),
                    tool_calls: Some(vec![ChatToolCall {
                        id: "call_123".to_string(),
                        call_type: "function".to_string(),
                        function: ChatFunctionCall {
                            name: "get_weather".to_string(),
                            arguments: "{\"city\":\"Makassar\"}".to_string(),
                        },
                    }]),
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "tool".to_string(),
                    content: json!("{\"city\":\"Makassar\",\"temp_c\":30}"),
                    tool_calls: None,
                    tool_call_id: Some("call_123".to_string()),
                },
            ],
            temperature: None,
            max_tokens: None,
            stream: Some(false),
            stream_options: None,
            tools: None,
            parallel_tool_calls: None,
            tool_choice: None,
        };

        let responses_request = server.convert_chat_to_responses(request, None);
        let payload = serde_json::to_value(responses_request).expect("request should serialize");

        assert_eq!(payload["input"][0]["type"], "message");
        assert_eq!(payload["input"][0]["role"], "user");
        assert_eq!(
            payload["input"][0]["content"][0]["text"],
            "Check the weather in Makassar"
        );
        assert_eq!(payload["input"][1]["type"], "function_call");
        assert_eq!(payload["input"][1]["call_id"], "call_123");
        assert_eq!(payload["input"][2]["type"], "function_call_output");
        assert_eq!(payload["input"][2]["call_id"], "call_123");
        assert_eq!(
            payload["input"][2]["output"],
            "{\"city\":\"Makassar\",\"temp_c\":30}"
        );
    }

    #[test]
    fn extracts_non_streaming_assistant_text() {
        let response: ResponsesApiResponse = serde_json::from_value(json!({
            "id": "resp_123",
            "usage": {
                "input_tokens": 11,
                "output_tokens": 7,
                "total_tokens": 18
            },
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [
                    { "type": "output_text", "text": "hello " },
                    { "type": "output_text", "text": "world" }
                ]
            }]
        }))
        .expect("json should deserialize");

        let parsed = collect_json_response_content(&response);

        assert_eq!(parsed.id.as_deref(), Some("resp_123"));
        assert_eq!(parsed.content, "hello world");
        assert_eq!(
            parsed.usage.as_ref().map(|usage| usage.total_tokens),
            Some(18)
        );
        assert_eq!(parsed.finish_reason, "stop");
    }

    #[test]
    fn extracts_non_streaming_tool_calls() {
        let response: ResponsesApiResponse = serde_json::from_value(json!({
            "id": "resp_tool",
            "output": [{
                "type": "function_call",
                "call_id": "call_123",
                "name": "get_weather",
                "arguments": "{\"city\":\"Makassar\"}"
            }]
        }))
        .expect("json should deserialize");

        let parsed = collect_json_response_content(&response);

        assert!(parsed.content.is_empty());
        assert_eq!(parsed.finish_reason, "tool_calls");
        assert_eq!(parsed.tool_calls.as_ref().map(Vec::len), Some(1));
        assert_eq!(
            parsed
                .tool_calls
                .as_ref()
                .and_then(|calls| calls.first())
                .map(|call| call.id.as_str()),
            Some("call_123")
        );
    }

    #[test]
    fn extracts_mixed_text_and_multiple_tool_calls() {
        let response: ResponsesApiResponse = serde_json::from_value(json!({
            "id": "resp_mixed",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        { "type": "output_text", "text": "I'll check both cities." }
                    ]
                },
                {
                    "type": "function_call",
                    "call_id": "call_123",
                    "name": "get_weather",
                    "arguments": "{\"city\":\"Makassar\"}"
                },
                {
                    "type": "function_call",
                    "call_id": "call_456",
                    "name": "get_weather",
                    "arguments": "{\"city\":\"Jakarta\"}"
                }
            ]
        }))
        .expect("json should deserialize");

        let parsed = collect_json_response_content(&response);

        assert_eq!(parsed.content, "I'll check both cities.");
        assert_eq!(parsed.finish_reason, "tool_calls");
        assert_eq!(parsed.tool_calls.as_ref().map(Vec::len), Some(2));
        assert_eq!(
            parsed
                .tool_calls
                .as_ref()
                .and_then(|calls| calls.get(1))
                .map(|call| call.id.as_str()),
            Some("call_456")
        );
    }

    #[test]
    fn does_not_duplicate_streaming_text_when_done_event_repeats_content() {
        let stream = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello \"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"world\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"content\":[{\"text\":\"hello world\"}]}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_stream\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2,\"total_tokens\":7}}}\n\n",
            "data: [DONE]\n\n"
        );

        let parsed = collect_response_content(stream);

        assert_eq!(parsed.id.as_deref(), Some("resp_stream"));
        assert_eq!(parsed.content, "hello world");
        assert_eq!(
            parsed.usage.as_ref().map(|usage| usage.total_tokens),
            Some(7)
        );
    }

    #[test]
    fn translates_backend_sse_into_openai_chunks() {
        let mut translator = BackendSseTranslator::new("gpt-5.1-codex-mini".to_string(), true);
        let backend_stream = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_123\",\"created_at\":42,\"model\":\"gpt-5.1-codex-mini\"}}\n\n",
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"role\":\"assistant\"}}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello\"}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"!\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_123\",\"created_at\":42,\"model\":\"gpt-5.1-codex-mini\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2,\"total_tokens\":7}}}\n\n"
        );

        let mut outputs = translator.push_chunk(backend_stream.as_bytes());
        outputs.extend(translator.finish());
        let rendered = outputs
            .into_iter()
            .map(|chunk| String::from_utf8(chunk.to_vec()).expect("bytes should be utf-8"))
            .collect::<String>();

        assert!(rendered.contains("\"object\":\"chat.completion.chunk\""));
        assert!(rendered.contains("\"role\":\"assistant\""));
        assert!(rendered.contains("\"content\":\"Hello\""));
        assert!(rendered.contains("\"content\":\"!\""));
        assert!(rendered.contains("\"finish_reason\":\"stop\""));
        assert!(rendered.contains(
            "\"usage\":{\"completion_tokens\":2,\"prompt_tokens\":5,\"total_tokens\":7}"
        ));
        assert_eq!(rendered.matches("data: [DONE]").count(), 1);
    }

    #[test]
    fn translates_backend_tool_call_stream_into_chat_completion_chunks() {
        let mut translator = BackendSseTranslator::new("gpt-5.3-codex".to_string(), false);
        let backend_stream = concat!(
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_123\",\"name\":\"get_weather\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"{\\\"city\\\":\\\"Makassar\\\"}\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_tool\",\"created_at\":42,\"model\":\"gpt-5.3-codex\",\"output\":[{\"type\":\"function_call\",\"call_id\":\"call_123\",\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\\\"Makassar\\\"}\"}]}}\n\n"
        );

        let mut outputs = translator.push_chunk(backend_stream.as_bytes());
        outputs.extend(translator.finish());
        let rendered = outputs
            .into_iter()
            .map(|chunk| String::from_utf8(chunk.to_vec()).expect("bytes should be utf-8"))
            .collect::<String>();

        assert!(rendered.contains("\"tool_calls\""));
        assert!(rendered.contains("\"id\":\"call_123\""));
        assert!(rendered.contains("\"name\":\"get_weather\""));
        assert!(rendered.contains("\"finish_reason\":\"tool_calls\""));
        assert_eq!(rendered.matches("data: [DONE]").count(), 1);
    }

    #[test]
    fn translates_mixed_text_and_tool_call_stream_into_chat_completion_chunks() {
        let mut translator = BackendSseTranslator::new("gpt-5.3-codex".to_string(), false);
        let backend_stream = concat!(
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"role\":\"assistant\"}}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Checking now. \"}\n\n",
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_123\",\"name\":\"get_weather\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"{\\\"city\\\":\\\"Makassar\\\"}\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_mixed_stream\",\"created_at\":42,\"model\":\"gpt-5.3-codex\",\"output\":[{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Checking now. \"}]},{\"type\":\"function_call\",\"call_id\":\"call_123\",\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\\\"Makassar\\\"}\"}]}}\n\n"
        );

        let mut outputs = translator.push_chunk(backend_stream.as_bytes());
        outputs.extend(translator.finish());
        let rendered = outputs
            .into_iter()
            .map(|chunk| String::from_utf8(chunk.to_vec()).expect("bytes should be utf-8"))
            .collect::<String>();

        assert!(rendered.contains("\"role\":\"assistant\""));
        assert!(rendered.contains("\"content\":\"Checking now. \""));
        assert!(rendered.contains("\"tool_calls\""));
        assert!(rendered.contains("\"name\":\"get_weather\""));
        assert!(rendered.contains("\"finish_reason\":\"tool_calls\""));
        assert_eq!(rendered.matches("data: [DONE]").count(), 1);
    }

    #[test]
    fn translates_multiple_tool_calls_with_partial_argument_deltas() {
        let mut translator = BackendSseTranslator::new("gpt-5.3-codex".to_string(), false);
        let backend_stream = concat!(
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_123\",\"name\":\"get_weather\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"{\\\"city\\\":\\\"Ma\"}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"kassar\\\"}\"}\n\n",
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"fc_2\",\"type\":\"function_call\",\"call_id\":\"call_456\",\"name\":\"get_weather\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_2\",\"delta\":\"{\\\"city\\\":\\\"Jakarta\\\"}\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_multi_tool\",\"created_at\":42,\"model\":\"gpt-5.3-codex\",\"output\":[{\"type\":\"function_call\",\"call_id\":\"call_123\",\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\\\"Makassar\\\"}\"},{\"type\":\"function_call\",\"call_id\":\"call_456\",\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\\\"Jakarta\\\"}\"}]}}\n\n"
        );

        let mut outputs = translator.push_chunk(backend_stream.as_bytes());
        outputs.extend(translator.finish());
        let rendered = outputs
            .into_iter()
            .map(|chunk| String::from_utf8(chunk.to_vec()).expect("bytes should be utf-8"))
            .collect::<String>();

        assert!(rendered.contains("\"id\":\"call_123\""));
        assert!(rendered.contains("\"id\":\"call_456\""));
        assert!(rendered.contains("\"index\":0"));
        assert!(rendered.contains("\"index\":1"));
        assert!(rendered.contains("\"arguments\":\"{\\\"city\\\":\\\"Ma\""));
        assert!(rendered.contains("\"arguments\":\"kassar\\\"}\""));
        assert!(rendered.contains("\"arguments\":\"{\\\"city\\\":\\\"Jakarta\\\"}\""));
        assert!(rendered.contains("\"finish_reason\":\"tool_calls\""));
        assert_eq!(rendered.matches("data: [DONE]").count(), 1);
    }

    #[test]
    fn backend_error_uses_body_error_shape_when_available() {
        let error = ProxyError::from_backend(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"bad input","type":"invalid_request_error","code":"bad_arg"}}"#,
        );

        assert_eq!(error.status_code(), 400);
        assert_eq!(error.error_type(), "invalid_request_error");
        assert_eq!(error.code(), "bad_arg");
        assert_eq!(error.message(), "bad input");
    }

    #[test]
    fn backend_error_falls_back_to_status_defaults() {
        let error = ProxyError::from_backend(reqwest::StatusCode::TOO_MANY_REQUESTS, "busy");

        assert_eq!(
            error.error_type(),
            default_error_type(reqwest::StatusCode::TOO_MANY_REQUESTS)
        );
        assert_eq!(
            error.code(),
            default_error_code(reqwest::StatusCode::TOO_MANY_REQUESTS)
        );
        assert!(error.message().contains("429"));
    }
}
