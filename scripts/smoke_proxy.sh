#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${1:-http://127.0.0.1:8080}"

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required for smoke_proxy.sh" >&2
  exit 1
fi

echo "== health =="
curl -fsS "${BASE_URL}/health"
printf '\n\n'

echo "== models =="
curl -fsS "${BASE_URL}/v1/models"
printf '\n\n'

echo "== non-streaming chat =="
curl -fsS "${BASE_URL}/v1/chat/completions" \
  -H 'content-type: application/json' \
  -d '{"model":"auto","messages":[{"role":"user","content":"say hello briefly"}]}'
printf '\n\n'

echo "== streaming chat =="
curl -fsS -N "${BASE_URL}/v1/chat/completions" \
  -H 'content-type: application/json' \
  -d '{"model":"medium","stream":true,"messages":[{"role":"user","content":"say hello briefly"}]}'
printf '\n\n'

echo "== anthropic non-streaming messages =="
curl -fsS "${BASE_URL}/v1/messages" \
  -H 'content-type: application/json' \
  -d '{"model":"claude-code-default","max_tokens":256,"messages":[{"role":"user","content":"say hello briefly"}]}'
printf '\n\n'

echo "== anthropic streaming messages =="
curl -fsS -N "${BASE_URL}/v1/messages" \
  -H 'content-type: application/json' \
  -d '{"model":"claude-code-default","stream":true,"max_tokens":256,"messages":[{"role":"user","content":"say hello briefly"}]}'
printf '\n\n'

echo "== invalid json =="
curl -sS -i "${BASE_URL}/v1/chat/completions" \
  -H 'content-type: application/json' \
  -d '{invalid json'
printf '\n'

echo
echo "== tool loop step 1: tool call =="
TOOL_REQUEST='{
  "model":"medium",
  "messages":[
    {"role":"user","content":"Call the weather tool for Makassar."}
  ],
  "tools":[
    {
      "type":"function",
      "function":{
        "name":"get_weather",
        "description":"Get the weather for a city",
        "parameters":{
          "type":"object",
          "properties":{"city":{"type":"string"}},
          "required":["city"]
        }
      }
    }
  ]
}'

TOOL_RESPONSE="$(curl -fsS "${BASE_URL}/v1/chat/completions" \
  -H 'content-type: application/json' \
  -d "${TOOL_REQUEST}")"
printf '%s\n\n' "${TOOL_RESPONSE}"

TOOL_CALL_ID="$(printf '%s' "${TOOL_RESPONSE}" | jq -r '.choices[0].message.tool_calls[0].id')"
TOOL_NAME="$(printf '%s' "${TOOL_RESPONSE}" | jq -r '.choices[0].message.tool_calls[0].function.name')"
TOOL_ARGS="$(printf '%s' "${TOOL_RESPONSE}" | jq -c '.choices[0].message.tool_calls[0].function.arguments | fromjson')"
FINISH_REASON="$(printf '%s' "${TOOL_RESPONSE}" | jq -r '.choices[0].finish_reason')"

if [[ "${FINISH_REASON}" != "tool_calls" ]]; then
  echo "expected finish_reason=tool_calls, got ${FINISH_REASON}" >&2
  exit 1
fi

if [[ -z "${TOOL_CALL_ID}" || "${TOOL_CALL_ID}" == "null" ]]; then
  echo "missing tool call id in tool loop response" >&2
  exit 1
fi

echo "== tool loop step 2: tool result follow-up =="
TOOL_OUTPUT='{"city":"Makassar","temp_c":30,"condition":"sunny"}'
FOLLOW_UP_REQUEST="$(jq -n \
  --arg tool_call_id "${TOOL_CALL_ID}" \
  --arg tool_name "${TOOL_NAME}" \
  --argjson tool_args "${TOOL_ARGS}" \
  --arg tool_output "${TOOL_OUTPUT}" \
  '{
    model: "medium",
    messages: [
      {role: "user", content: "Call the weather tool for Makassar."},
      {
        role: "assistant",
        content: null,
        tool_calls: [
          {
            id: $tool_call_id,
            type: "function",
            function: {
              name: $tool_name,
              arguments: ($tool_args | tojson)
            }
          }
        ]
      },
      {
        role: "tool",
        tool_call_id: $tool_call_id,
        content: $tool_output
      }
    ]
  }')"

FOLLOW_UP_RESPONSE="$(curl -fsS "${BASE_URL}/v1/chat/completions" \
  -H 'content-type: application/json' \
  -d "${FOLLOW_UP_REQUEST}")"
printf '%s\n' "${FOLLOW_UP_RESPONSE}"

echo
echo "== multi-tool non-streaming =="
MULTI_TOOL_REQUEST='{
  "model":"medium",
  "parallel_tool_calls":true,
  "messages":[
    {
      "role":"user",
      "content":"Use the weather tool to get the weather for Makassar and Jakarta. Call the tool for both cities before answering."
    }
  ],
  "tools":[
    {
      "type":"function",
      "function":{
        "name":"get_weather",
        "description":"Get the weather for a city",
        "parameters":{
          "type":"object",
          "properties":{"city":{"type":"string"}},
          "required":["city"]
        }
      }
    }
  ]
}'

MULTI_TOOL_RESPONSE="$(curl -fsS "${BASE_URL}/v1/chat/completions" \
  -H 'content-type: application/json' \
  -d "${MULTI_TOOL_REQUEST}")"
printf '%s\n\n' "${MULTI_TOOL_RESPONSE}"

MULTI_TOOL_COUNT="$(printf '%s' "${MULTI_TOOL_RESPONSE}" | jq -r '.choices[0].message.tool_calls | length')"
if [[ "${MULTI_TOOL_COUNT}" -lt 2 ]]; then
  echo "expected at least 2 tool calls, got ${MULTI_TOOL_COUNT}" >&2
  exit 1
fi

echo "== multi-tool streaming =="
curl -fsS -N "${BASE_URL}/v1/chat/completions" \
  -H 'content-type: application/json' \
  -d "$(jq -c '. + {stream: true}' <<<"${MULTI_TOOL_REQUEST}")"
printf '\n'
