#!/usr/bin/env python3
"""Read-only MCP stdio smoke test for mcp-cura.

Usage:
    python3 scripts/smoke_mcp.py [path-to-binary]

The server must be able to start. Nothing destructive is called.
"""

from __future__ import annotations

import argparse
import json
import queue
import subprocess
import sys
import threading
import time

PROTOCOL_VERSION = "2025-11-25"


def result_text(message: dict) -> str:
    parts = []
    for block in message.get("result", {}).get("content") or []:
        if block.get("type") == "text":
            parts.append(block.get("text", ""))
    return "\n".join(parts)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "binary",
        nargs="?",
        default="target/debug/mcp-cura",
        help="path to the mcp-cura binary",
    )
    args = parser.parse_args()

    requests = [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "mcp-cura-smoke", "version": "0.1.0"},
            },
        },
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
        {"jsonrpc": "2.0", "id": 3, "method": "resources/list"},
        {"jsonrpc": "2.0", "id": 4, "method": "prompts/list"},
        {
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {"name": "cura_policy_info", "arguments": {}},
        },
    ]

    try:
        proc = subprocess.Popen(
            [args.binary],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
    except FileNotFoundError:
        print(f"FAIL binary not found: {args.binary}")
        return 1

    assert proc.stdin and proc.stdout and proc.stderr

    lines: queue.Queue[str] = queue.Queue()

    def read_stdout() -> None:
        assert proc.stdout is not None
        for line in proc.stdout:
            lines.put(line)

    threading.Thread(target=read_stdout, daemon=True).start()

    for request in requests:
        proc.stdin.write(json.dumps(request) + "\n")
        proc.stdin.flush()

    seen: dict[int, dict] = {}
    deadline = time.time() + 45
    while len(seen) < len(requests):
        remaining = deadline - time.time()
        if remaining <= 0:
            break
        try:
            line = lines.get(timeout=remaining)
        except queue.Empty:
            break
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            print("NON-JSON STDOUT:", line[:200])
            continue
        if "id" in message:
            seen[message["id"]] = message

    proc.stdin.close()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)
    stderr = proc.stderr.read()

    failures: list[str] = []

    def check(condition: bool, label: str) -> None:
        print(("PASS " if condition else "FAIL ") + label)
        if not condition:
            failures.append(label)

    init = seen.get(1, {})
    check(
        init.get("result", {}).get("protocolVersion") == PROTOCOL_VERSION,
        "initialize",
    )
    check(
        init.get("result", {}).get("serverInfo", {}).get("name") == "mcp-cura",
        "server identity",
    )

    tools = seen.get(2, {}).get("result", {}).get("tools", [])
    tool_names = {tool.get("name") for tool in tools}
    check("cura_server_info" in tool_names, "tools/list contains cura_server_info")
    check("cura_policy_info" in tool_names, "tools/list contains cura_policy_info")
    check(len(tools) >= 32, f"tools/list count ({len(tools)})")

    resources = seen.get(3, {}).get("result", {}).get("resources", [])
    check(
        any(resource.get("uri") == "cura://policy" for resource in resources),
        "resources/list",
    )

    prompts = seen.get(4, {}).get("result", {}).get("prompts", [])
    check(len(prompts) == 3, f"prompts/list ({len(prompts)})")

    policy_text = result_text(seen.get(5, {}))
    check('"policy"' in policy_text, "policy_info result")

    if failures:
        print("\nSTDERR:\n" + stderr[-4000:])
        print(f"\n{len(failures)} check(s) failed.")
        return 1

    print(f"MCP smoke test passed ({len(seen)} responses).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
