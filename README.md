# Codex OpenAI Proxy

OpenAI-compatible and Anthropic-compatible gateway for the Codex/ChatGPT backend.

It accepts OpenAI Chat Completions requests and Anthropic Messages requests, routes them through a policy-aware model selector, forwards them to the Codex backend, and translates the responses back into protocol-compatible JSON or SSE.

## Current Status

This repo is already usable for serious local testing.

Verified today:

- non-streaming `/v1/chat/completions`
- streaming `/v1/chat/completions`
- non-streaming `/v1/messages`
- streaming `/v1/messages`
- `/v1/models`
- tool-call responses
- tool-result follow-up requests
- multi-tool responses
- official OpenAI Python SDK compatibility
- Claude Code compatibility against the Anthropic-compatible `/v1/messages` path

The latest implementation status is tracked in [docs/status/2026-03-13-project-summary-and-remaining-tasks.md](docs/status/2026-03-13-project-summary-and-remaining-tasks.md).

## What It Does

- accepts OpenAI Chat Completions requests
- accepts Anthropic Messages requests
- uses Codex auth from `~/.codex/auth.json`
- supports virtual aliases `auto`, `balanced`, `small`, `medium`, and `large`
- maps routing decisions onto Codex-supported backend models
- supports reasoning levels `low`, `medium`, `high`, and `extra_high`
- supports non-streaming and streaming SSE responses
- supports `tools`, `tool_choice`, `parallel_tool_calls`, and tool-result follow-up messages
- returns OpenAI-style and Anthropic-style error payloads
- exposes routing/debug headers such as `x-codex-model` and `x-codex-thinking`

## Model Routing

Current alias mapping:

- `small` -> `gpt-5.1-codex-mini`
- `medium` -> `gpt-5.3-codex`
- `large` -> `gpt-5.4`

Default `balanced` behavior is roughly:

- light requests -> `small + low`
- normal coding work -> `medium + medium`
- complex coding and tool workflows -> `medium + high`
- architecture or deep investigation -> `large + high` or `large + extra_high`

Important behavior:

- explicit client model choices are respected
- auto-escalation is for policy-driven routes, not for explicit client overrides

## API Surface

Implemented endpoints:

- `GET /health`
- `GET /models`
- `GET /v1/models`
- `POST /chat/completions`
- `POST /v1/chat/completions`
- `POST /messages`
- `POST /v1/messages`

Supported request fields include:

- `model`
- `messages`
- `temperature`
- `max_tokens`
- `stream`
- `stream_options.include_usage`
- `tools`
- `tool_choice`
- `parallel_tool_calls`
- Anthropic `thinking`

Anthropic model families currently map like this:

- `claude-haiku-*` -> `small + low`
- `claude-sonnet-*` -> `medium + medium`
- `claude-opus-*` -> `large + high`
- `claude-code-fast/default/max` -> `small/medium/large`

Anthropic `thinking` request handling:

- `{"type":"disabled"}` -> `low`
- `{"type":"enabled","budget_tokens":2000}` -> `medium`
- `{"type":"enabled","budget_tokens":4000}` -> `high`
- `{"type":"enabled","budget_tokens":16000}` -> `extra_high`

The proxy keeps Anthropic-style labels externally, but maps `extra_high` to the Codex backend effort value it expects.

## Auth

The proxy reads Codex authentication from `~/.codex/auth.json` by default.

It accepts both top-level and nested token shapes. The important values are:

- access token
- account id
- optional API key fallback

## Quick Start

Build and run:

```bash
cargo build --release
./target/release/codex-openai-proxy --port 8080 --auth-path ~/.codex/auth.json
```

Health check:

```bash
curl http://127.0.0.1:8080/health
```

Basic chat test:

```bash
curl http://127.0.0.1:8080/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{
    "model": "auto",
    "messages": [
      {"role": "user", "content": "say hello briefly"}
    ]
  }'
```

Basic Anthropic Messages test:

```bash
curl http://127.0.0.1:8080/v1/messages \
  -H 'content-type: application/json' \
  -d '{
    "model": "claude-code-default",
    "max_tokens": 256,
    "messages": [
      {"role": "user", "content": "say hello briefly"}
    ]
  }'
```

## OpenAI-Compatible Client Setup

Use any OpenAI-compatible client with:

- base URL: `http://127.0.0.1:8080/v1`
- API key: any placeholder value
- model: one of `auto`, `balanced`, `small`, `medium`, `large`, or a concrete backend model

Example:

- base URL: `http://127.0.0.1:8080/v1`
- API key: `test-key`
- model: `medium`

If your client requires HTTPS, put the proxy behind your own tunnel or reverse proxy.

## Anthropic-Compatible Client Setup

Use any Anthropic-compatible client with:

- base URL: `http://127.0.0.1:8080`
- API key: any placeholder value
- endpoint: `/v1/messages`
- model: one of `claude-code-fast`, `claude-code-default`, `claude-code-max`, or a Claude family model such as `claude-sonnet-4-5`

Example:

- base URL: `http://127.0.0.1:8080`
- API key: `test-key`
- model: `claude-code-default`

Verified client:

- Claude Code works against this proxy with `ANTHROPIC_BASE_URL=http://127.0.0.1:8080`

Notes from live validation:

- Anthropic `system` content is translated into backend `instructions`
- assistant history is encoded as backend `output_text`, not `input_text`
- empty object tool schemas are normalized to include `properties: {}`

## Smoke Tests

Manual curl-based smoke:

```bash
scripts/smoke_proxy.sh http://127.0.0.1:8080
```

Official OpenAI Python SDK smoke:

```bash
scripts/smoke_openai_sdk.sh http://127.0.0.1:8080
```

That SDK smoke currently verifies:

- non-streaming chat
- streaming chat
- multi-tool tool-call flow
- tool-result follow-up flow

The curl smoke also verifies:

- OpenAI-compatible non-streaming and streaming
- Anthropic-compatible non-streaming and streaming
- tool loop and multi-tool behavior

## Debugging

Run with logs:

```bash
RUST_LOG=info cargo run -- --port 8080
```

More verbose:

```bash
RUST_LOG=debug cargo run -- --port 8080
```

Useful response headers:

- `x-codex-route`
- `x-codex-model`
- `x-codex-thinking`
- `x-codex-task-kind`
- `x-codex-override-source`
- `x-codex-escalated`
- `x-codex-escalation-reason`

## Development

Main modules:

- `src/main.rs`: bootstrap only
- `src/app.rs`: app state and startup wiring
- `src/config.rs`: config loading
- `src/models.rs`: model registry
- `src/routing/`: analyzer, policy, and decision types
- `src/execution.rs`: non-streaming execution and escalation
- `src/backend.rs`: backend adapter and SSE translation
- `src/http/`: routes, handlers, and contract tests

Common commands:

```bash
cargo fmt
cargo test
RUST_LOG=debug cargo run -- --port 8080
```

## Optional Follow-Up

The core gateway milestone is complete.

Possible follow-up work:

- observe a live backend case that emits mixed `assistant text + tool_calls`
- expand compatibility only if a concrete client requires additional fields
- refine observability further if you want shadow-mode or richer analytics

See [docs/status/2026-03-13-project-summary-and-remaining-tasks.md](docs/status/2026-03-13-project-summary-and-remaining-tasks.md) for the latest status snapshot.
