#!/usr/bin/env python3
"""Tiny stdio MCP smoke test for the single SKB binary (`SKB.exe mcp`)."""

import json
import subprocess
import sys

if len(sys.argv) != 2:
    raise SystemExit("usage: python scripts/mcp_smoke.py /path/to/SKB[.exe]")

exe = sys.argv[1]
proc = subprocess.Popen(
    [exe, "mcp"],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
)
assert proc.stdin is not None
assert proc.stdout is not None

messages = [
    {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "skb-smoke", "version": "1"},
        },
    },
    {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
]

for message in messages:
    proc.stdin.write(json.dumps(message) + "\n")
proc.stdin.flush()

for expected_id in (1, 2):
    line = proc.stdout.readline()
    if not line:
        stderr = proc.stderr.read() if proc.stderr else ""
        proc.kill()
        raise SystemExit(f"MCP closed before response {expected_id}: {stderr}")
    response = json.loads(line)
    if response.get("id") != expected_id or "error" in response:
        proc.kill()
        raise SystemExit(f"bad MCP response: {response}")
    print(json.dumps(response, ensure_ascii=False))

proc.terminate()
print("MCP smoke passed")
