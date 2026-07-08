#!/usr/bin/env python3
"""Minimal MCP stdio server for tests: two tools, no dependencies.

Speaks just enough JSON-RPC 2.0 / MCP for a client handshake, tools/list,
and tools/call. Exits when stdin closes.
"""
import json
import sys

TOOLS = [
    {
        "name": "echo",
        "description": "Echo the given text back.",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        },
    },
    {
        "name": "add",
        "description": "Add two integers.",
        "inputSchema": {
            "type": "object",
            "properties": {"a": {"type": "integer"}, "b": {"type": "integer"}},
            "required": ["a", "b"],
        },
    },
]


def reply(id_, result):
    print(json.dumps({"jsonrpc": "2.0", "id": id_, "result": result}), flush=True)


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method, id_ = msg.get("method"), msg.get("id")
    if method == "initialize":
        reply(id_, {
            "protocolVersion": msg["params"]["protocolVersion"],
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "echo-fixture", "version": "0.1.0"},
        })
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        reply(id_, {"tools": TOOLS})
    elif method == "tools/call":
        name = msg["params"]["name"]
        args = msg["params"].get("arguments") or {}
        if name == "echo":
            text = f"echo: {args.get('text', '')}"
        elif name == "add":
            text = str(int(args.get("a", 0)) + int(args.get("b", 0)))
        else:
            reply(id_, {"content": [{"type": "text", "text": f"unknown tool {name}"}], "isError": True})
            continue
        reply(id_, {"content": [{"type": "text", "text": text}], "isError": False})
    elif method == "ping":
        reply(id_, {})
    elif id_ is not None:
        print(json.dumps({"jsonrpc": "2.0", "id": id_,
                          "error": {"code": -32601, "message": f"method not found: {method}"}}), flush=True)
