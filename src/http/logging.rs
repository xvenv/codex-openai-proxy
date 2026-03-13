use bytes::Bytes;
use log::{debug, info};
use warp::http::{HeaderMap, Method};

use crate::backend::{ChatCompletionsRequest, ChatMessage};

pub fn log_request(method: &Method, path: &str, headers: &HeaderMap) {
    let user_agent = headers
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("none")
        .to_lowercase()
        .replace(' ', "_");

    info!(
        "event=http.request method={} path={} header_count={} user_agent={}",
        method,
        path,
        headers.len(),
        user_agent
    );

    for (name, value) in headers {
        let header_name = name.as_str().to_lowercase();
        let header_value =
            sanitize_header_value(&header_name, value.to_str().unwrap_or("[invalid]"));
        debug!(
            "event=http.request_header method={} path={} name={} value={}",
            method, path, header_name, header_value
        );
    }
}

pub fn log_chat_request_details(
    path: &str,
    headers: &HeaderMap,
    body: &Bytes,
    chat_req: &ChatCompletionsRequest,
) {
    info!(
        "event=http.chat_request path={} model={} body_bytes={} message_count={} stream={}",
        path,
        chat_req.model,
        body.len(),
        chat_req.messages.len(),
        chat_req.stream.unwrap_or(false)
    );

    for (index, message) in chat_req.messages.iter().enumerate() {
        debug!(
            "event=http.chat_message path={} index={} role={} preview={}",
            path,
            index,
            message.role,
            preview_message_content(message)
        );
    }

    if let Ok(body_str) = std::str::from_utf8(body) {
        debug!(
            "event=http.chat_body path={} body_preview={}",
            path,
            truncate_for_log(body_str, 1000)
        );
    }

    for (name, value) in headers {
        if let Ok(value_str) = value.to_str() {
            let header_name = name.as_str().to_lowercase();
            if header_name.starts_with("x-forwarded") || header_name == "host" {
                continue;
            }

            debug!(
                "event=http.chat_header path={} name={} value={}",
                path,
                header_name,
                sanitize_header_value(&header_name, value_str)
            );
        }
    }
}

fn preview_message_content(message: &ChatMessage) -> String {
    match &message.content {
        serde_json::Value::String(text) => truncate_for_log(text, 50),
        serde_json::Value::Null => "[null]".to_string(),
        serde_json::Value::Array(items) => format!("[array with {} items]", items.len()),
        other => format!("[{}]", truncate_for_log(&other.to_string(), 50)),
    }
}

fn sanitize_header_value(header_name: &str, value: &str) -> String {
    if header_name == "authorization" {
        let preview = &value[..std::cmp::min(20, value.len())];
        format!("{preview}***")
    } else {
        value.replace(' ', "_")
    }
}

fn truncate_for_log(value: &str, max_chars: usize) -> String {
    let truncated = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        format!("{truncated}...[truncated]")
    } else {
        truncated
    }
}
