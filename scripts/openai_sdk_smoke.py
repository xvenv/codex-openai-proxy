#!/usr/bin/env python3
import argparse
import json
import sys
from typing import Any

from openai import OpenAI


def build_client(base_url: str) -> OpenAI:
    return OpenAI(base_url=f"{base_url.rstrip('/')}/v1", api_key="test-key")


def assert_condition(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(message)


def run_non_streaming(client: OpenAI) -> None:
    print("== openai sdk: non-streaming chat ==")
    response = client.chat.completions.create(
        model="medium",
        messages=[{"role": "user", "content": "say hello briefly"}],
    )
    print(response.model_dump_json(indent=2))

    choice = response.choices[0]
    assert_condition(choice.finish_reason == "stop", "expected non-streaming finish_reason=stop")
    assert_condition(
        bool(choice.message.content and choice.message.content.strip()),
        "expected non-streaming assistant content",
    )


def run_streaming(client: OpenAI) -> None:
    print("\n== openai sdk: streaming chat ==")
    chunks: list[Any] = []
    with client.chat.completions.stream(
        model="medium",
        messages=[{"role": "user", "content": "say hello briefly"}],
    ) as stream:
        for event in stream:
            if event.type == "chunk":
                chunk = event.chunk
                chunks.append(chunk)
                print(chunk.model_dump_json(indent=2))

    assert_condition(chunks, "expected streaming chunks from sdk")
    assert_condition(
        any(
            choice.delta and choice.delta.content
            for chunk in chunks
            for choice in chunk.choices
        ),
        "expected at least one streaming content delta",
    )
    assert_condition(
        any(choice.finish_reason == "stop" for chunk in chunks for choice in chunk.choices),
        "expected final streaming stop chunk",
    )


def run_tool_loop(client: OpenAI) -> None:
    print("\n== openai sdk: tool loop ==")
    tools = [
        {
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get the weather for a city",
                "parameters": {
                    "type": "object",
                    "properties": {"city": {"type": "string"}},
                    "required": ["city"],
                },
            },
        }
    ]

    initial = client.chat.completions.create(
        model="medium",
        parallel_tool_calls=True,
        messages=[
            {
                "role": "user",
                "content": "Use the weather tool to get the weather for Makassar and Jakarta. Call the tool for both cities before answering.",
            }
        ],
        tools=tools,
    )
    print(initial.model_dump_json(indent=2))

    first_choice = initial.choices[0]
    tool_calls = first_choice.message.tool_calls or []
    assert_condition(first_choice.finish_reason == "tool_calls", "expected finish_reason=tool_calls")
    assert_condition(len(tool_calls) >= 2, "expected at least two tool calls from sdk request")

    follow_up_messages: list[dict[str, Any]] = [
        {
            "role": "user",
            "content": "Use the weather tool to get the weather for Makassar and Jakarta. Call the tool for both cities before answering.",
        },
        {
            "role": "assistant",
            "content": None,
            "tool_calls": [
                {
                    "id": tool_call.id,
                    "type": "function",
                    "function": {
                        "name": tool_call.function.name,
                        "arguments": tool_call.function.arguments,
                    },
                }
                for tool_call in tool_calls
            ],
        },
    ]

    canned_outputs = {
        "Makassar": {"city": "Makassar", "temp_c": 30, "condition": "sunny"},
        "Jakarta": {"city": "Jakarta", "temp_c": 31, "condition": "cloudy"},
    }
    for tool_call in tool_calls:
        arguments = json.loads(tool_call.function.arguments)
        city = arguments["city"]
        follow_up_messages.append(
            {
                "role": "tool",
                "tool_call_id": tool_call.id,
                "content": json.dumps(canned_outputs.get(city, {"city": city, "temp_c": 0})),
            }
        )

    final_response = client.chat.completions.create(
        model="medium",
        messages=follow_up_messages,
    )
    print(final_response.model_dump_json(indent=2))

    final_choice = final_response.choices[0]
    assert_condition(final_choice.finish_reason == "stop", "expected final tool-loop finish_reason=stop")
    final_content = final_choice.message.content or ""
    assert_condition("Makassar" in final_content, "expected final answer to mention Makassar")
    assert_condition("Jakarta" in final_content, "expected final answer to mention Jakarta")


def main() -> int:
    parser = argparse.ArgumentParser(description="Smoke test the proxy using the official OpenAI Python SDK.")
    parser.add_argument(
        "--base-url",
        default="http://127.0.0.1:8080",
        help="Proxy base URL without /v1 suffix",
    )
    args = parser.parse_args()

    client = build_client(args.base_url)
    run_non_streaming(client)
    run_streaming(client)
    run_tool_loop(client)
    print("\nopenai sdk smoke test passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
