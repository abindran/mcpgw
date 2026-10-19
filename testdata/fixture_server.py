#!/usr/bin/env python3
"""Minimal legacy (2025-11-25) MCP stdio server used as a test backend.

Implements only what the gateway tests need. It answers `server/discover`
with JSON-RPC -32601 (method not found), exactly like a real pre-2026 server,
so it exercises the gateway's Auto-lifecycle fallback to `initialize`.

Tools:
  add(a, b)      -> text sum
  echo(text)     -> text, prefixed with $FIXTURE_PREFIX if set
  crash()        -> process exits immediately (lifecycle tests)
  sleep(ms)      -> sleeps, then returns "slept"
Env:
  FIXTURE_HANG_PING=1   never answer `ping` (health-check tests)
"""
import json
import os
import sys
import time

PREFIX = os.environ.get("FIXTURE_PREFIX", "")
HANG_PING = os.environ.get("FIXTURE_HANG_PING") == "1"

TOOLS = [
    {
        "name": "add",
        "description": "Add two integers",
        "inputSchema": {
            "type": "object",
            "properties": {"a": {"type": "integer"}, "b": {"type": "integer"}},
            "required": ["a", "b"],
        },
    },
    {
        "name": "echo",
        "description": "Echo text back",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        },
    },
    {"name": "crash", "description": "Exit the process", "inputSchema": {"type": "object"}},
    {
        "name": "sleep",
        "description": "Sleep for ms milliseconds",
        "inputSchema": {"type": "object", "properties": {"ms": {"type": "integer"}}},
    },
]


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def text(t):
    return {"content": [{"type": "text", "text": t}], "isError": False}


def handle(method, params):
    if method == "initialize":
        return {
            "protocolVersion": "2025-11-25",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fixture", "version": "0.0.0"},
        }
    if method == "ping":
        return {}
    if method == "tools/list":
        return {"tools": TOOLS}
    if method == "tools/call":
        name = params.get("name")
        args = params.get("arguments") or {}
        if name == "add":
            return text(str(int(args["a"]) + int(args["b"])))
        if name == "echo":
            return text(PREFIX + str(args.get("text", "")))
        if name == "crash":
            sys.exit(3)
        if name == "sleep":
            time.sleep(int(args.get("ms", 0)) / 1000.0)
            return text("slept")
        raise LookupError(f"tool not found: {name}")
    raise NotImplementedError(method)


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        msg = json.loads(line)
        if "id" not in msg or "method" not in msg:
            continue  # notification or response
        mid, method = msg["id"], msg["method"]
        if method == "ping" and HANG_PING:
            continue
        try:
            send({"jsonrpc": "2.0", "id": mid, "result": handle(method, msg.get("params") or {})})
        except NotImplementedError:
            send({"jsonrpc": "2.0", "id": mid,
                  "error": {"code": -32601, "message": f"Method not found: {method}"}})
        except LookupError as e:
            send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32602, "message": str(e)}})


if __name__ == "__main__":
    main()
