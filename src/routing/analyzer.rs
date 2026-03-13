use crate::backend::{ChatCompletionsRequest, ChatMessage};

use super::decision::TaskKind;

#[derive(Clone, Debug)]
pub struct RequestFeatures {
    pub message_count: usize,
    pub estimated_chars: usize,
    pub has_tools: bool,
    pub code_block_count: usize,
    pub file_reference_count: usize,
    pub task_kind: TaskKind,
}

pub fn analyze(chat_req: &ChatCompletionsRequest) -> RequestFeatures {
    let flattened_messages: Vec<String> = chat_req.messages.iter().map(flatten_message).collect();
    let joined = flattened_messages.join("\n");
    let lower = joined.to_lowercase();
    let has_tools = chat_req
        .tools
        .as_ref()
        .is_some_and(|tools| !tools.is_empty());
    let code_block_count = lower.matches("```").count();
    let file_reference_count = count_file_references(&lower);

    let contains_migration_keywords =
        contains_any(&lower, &["migration", "migrate", "upgrade", "downgrade"]);
    let contains_architecture_keywords = contains_any(
        &lower,
        &[
            "architecture",
            "design",
            "tradeoff",
            "refactor",
            "plan",
            "strategy",
        ],
    );
    let contains_review_keywords =
        contains_any(&lower, &["review", "audit", "feedback", "best practice"]);
    let contains_debug_keywords = contains_any(
        &lower,
        &[
            "debug",
            "bug",
            "error",
            "traceback",
            "failing",
            "broken",
            "fix",
        ],
    );
    let contains_transform_keywords = contains_any(
        &lower,
        &["rewrite", "summarize", "translate", "rephrase", "format"],
    );

    let task_kind = if has_tools {
        TaskKind::ToolWorkflow
    } else if contains_migration_keywords {
        TaskKind::Migration
    } else if contains_architecture_keywords {
        TaskKind::Design
    } else if contains_review_keywords {
        TaskKind::Review
    } else if contains_debug_keywords {
        if flattened_messages.len() > 4 || joined.len() > 2_000 || file_reference_count > 1 {
            TaskKind::DebugComplex
        } else {
            TaskKind::DebugSimple
        }
    } else if contains_transform_keywords {
        TaskKind::Transform
    } else if code_block_count > 0 || file_reference_count > 0 {
        TaskKind::CodeEditLocal
    } else {
        TaskKind::Chat
    };

    RequestFeatures {
        message_count: chat_req.messages.len(),
        estimated_chars: joined.len(),
        has_tools,
        code_block_count,
        file_reference_count,
        task_kind,
    }
}

fn flatten_message(message: &ChatMessage) -> String {
    match &message.content {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                item.as_object()
                    .and_then(|object| object.get("text"))
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned)
                    .or_else(|| item.as_str().map(ToOwned::to_owned))
            })
            .collect::<Vec<_>>()
            .join(" "),
        other => other.to_string(),
    }
}

fn count_file_references(text: &str) -> usize {
    text.split_whitespace()
        .filter(|token| {
            token.contains("src/")
                || token.ends_with(".rs")
                || token.ends_with(".ts")
                || token.ends_with(".js")
                || token.ends_with(".tsx")
                || token.ends_with(".json")
                || token.ends_with(".toml")
        })
        .count()
}

fn contains_any(text: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|pattern| text.contains(pattern))
}
