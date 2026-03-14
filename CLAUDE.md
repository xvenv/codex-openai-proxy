# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Core development commands

- Build: `cargo build` (or `cargo build --release`)
- Run locally: `cargo run`
- Run with verbose logs: `RUST_LOG=debug cargo run`
- Format: `cargo fmt`
- Lint: `cargo clippy --all-targets --all-features`
- Test all: `cargo test`
- Run one test: `cargo test <test_name>`
  - Example: `cargo test routes_simple_chat_to_small`
- Run one module’s tests: `cargo test routing::policy::tests`
- Run smoke tests against a running proxy:
  - `scripts/smoke_proxy.sh http://127.0.0.1:8080`
  - `scripts/smoke_openai_sdk.sh http://127.0.0.1:8080`

## Runtime and config model

- CLI entrypoint is `src/main.rs`; runtime wiring is in `src/app.rs`.
- Config is loaded via `AppConfig::from_args` in `src/config.rs`.
- Default config path is `~/.config/codex-proxy/config.json`.
- If default config is missing, the app auto-creates it (and migrates legacy `~/.codex-proxy/config.json` if present).
- `install.sh` builds a release binary into `~/.local/bin/codex-openai-proxy` and bootstraps user config.

## High-level architecture

The proxy has three major layers:

1. **HTTP protocol layer (`src/http/`)**
   - Single universal handler dispatches all routes.
   - Supports OpenAI-style endpoints (`/v1/chat/completions`) and Anthropic-style endpoints (`/v1/messages`).
   - Performs request/response shape translation, including SSE streaming translation for both protocols.
   - Adds routing/debug headers (`x-codex-*`) to responses.

2. **Routing/execution layer (`src/routing/`, `src/execution.rs`)**
   - `routing::analyzer` classifies request features (task type, complexity signals, tools, file/code context).
   - `routing::policy` maps features + headers (`x-codex-thinking`, `x-codex-routing-mode`) to alias/model/thinking decisions.
   - `execution::execute_chat_completion` handles non-streaming post-routing escalation (small→medium→large) when responses look weak.

3. **Backend adapter layer (`src/backend.rs`)**
   - Converts chat-style requests into ChatGPT/Codex Responses API payloads.
   - Handles auth from Codex auth JSON.
   - Translates backend responses/SSE events back into OpenAI-compatible chunks/messages.
   - Provides stub fallback behavior when configured (`ExecutionConfig`).

## Model and mapping concepts

- Model aliases (`auto`, `balanced`, `small`, `medium`, `large`) and backend targets are managed by `ModelRegistry` in `src/models.rs`.
- Routing policy chooses aliases; registry resolves alias → concrete backend model.
- Anthropic model names are mapped to aliases in config (`anthropic_mapping`) and normalized in `http/handlers.rs`.
- Thinking levels are internalized as `low|medium|high|extra_high` and converted to backend effort values (`extra_high` -> `xhigh`).

## Request lifecycle (important for changes)

For `POST /v1/chat/completions`:
1. Parse request in `http/handlers.rs`.
2. Compute routing decision via `routing::policy::decide`.
3. Replace requested model with routed backend model.
4. Stream path: `ProxyServer::proxy_streaming_request`.
5. Non-stream path: `execution::execute_chat_completion` (may escalate and reissue).

For `POST /v1/messages`:
1. Parse Anthropic payload.
2. Convert to internal chat request (`anthropic_to_chat_request`).
3. Route via same policy engine.
4. Apply Anthropic thinking preferences/floors.
5. Forward to same backend path.
6. Convert back to Anthropic response/SSE event format.

## Testing layout

- This codebase relies mostly on inline unit/integration-style tests under `#[cfg(test)]` in module files (not a separate `tests/` directory).
- Heaviest behavioral coverage is in:
  - `src/http/mod.rs` (end-to-end route behavior, protocol compatibility, streaming/tool flows)
  - `src/routing/policy.rs` (routing policy decisions)
  - `src/config.rs` (config loading/defaulting/migration)
  - `src/execution.rs` (escalation behavior)

## Practical editing guidance

- For protocol compatibility changes, touch `src/http/handlers.rs` first; keep OpenAI/Anthropic shape translation symmetric.
- For routing changes, update both analyzer signals (`src/routing/analyzer.rs`) and policy thresholds/reasons (`src/routing/policy.rs`).
- For backend payload/stream changes, update `src/backend.rs` translators and preserve SSE frame handling semantics.
- When changing routing or protocol translation, run `cargo test` plus both smoke scripts to catch regressions in streaming and tool-call flows.
