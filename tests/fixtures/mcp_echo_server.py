#!/usr/bin/env python3
"""Minimal MCP echo server fixture for integration tests.

Reads newline-delimited JSON-RPC 2.0 requests from stdin and writes
responses to stdout.  Supports the following methods:

  initialize          -> canonical InitializeResult
  notifications/initialized -> ignored (no response)
  tools/list          -> hardcoded tool list
  tools/call          -> echoes arguments back as result content
  resources/list      -> returns one resource
  resources/read      -> returns text content

For testing transport-error simulation the server optionally exits
early when it receives a specially-crafted request (method == "die").

Environment variables:
  MCP_NO_TOOLS_CAP    If set, capabilities.tools is omitted from the
                      initialize response, so clients that guard on the
                      capability will skip tools/list.
  MCP_NO_RESOURCES_CAP  Omits capabilities.resources from the response.
"""

import json
import os
import sys


def respond(req_id, result):
    msg = json.dumps({"jsonrpc": "2.0", "id": req_id, "result": result})
    sys.stdout.write(msg + "\n")
    sys.stdout.flush()


def error_response(req_id, code, message, data=None):
    err = {"code": code, "message": message}
    if data is not None:
        err["data"] = data
    msg = json.dumps({"jsonrpc": "2.0", "id": req_id, "error": err})
    sys.stdout.write(msg + "\n")
    sys.stdout.flush()


def main():
    no_tools_cap = "MCP_NO_TOOLS_CAP" in os.environ
    no_resources_cap = "MCP_NO_RESOURCES_CAP" in os.environ

    for raw_line in sys.stdin:
        line = raw_line.strip()
        if not line:
            continue

        try:
            req = json.loads(line)
        except json.JSONDecodeError as exc:
            # Write a parse-error response without an id
            msg = json.dumps(
                {
                    "jsonrpc": "2.0",
                    "id": None,
                    "error": {"code": -32700, "message": f"Parse error: {exc}"},
                }
            )
            sys.stdout.write(msg + "\n")
            sys.stdout.flush()
            continue

        req_id = req.get("id")
        method = req.get("method", "")

        # Notifications have no id — do not respond
        if req_id is None:
            continue

        if method == "initialize":
            capabilities: dict = {}
            if not no_tools_cap:
                capabilities["tools"] = {"listChanged": False}
            if not no_resources_cap:
                capabilities["resources"] = {}
            respond(
                req_id,
                {
                    "protocolVersion": "2024-11-05",
                    "capabilities": capabilities,
                    "serverInfo": {"name": "echo-server", "version": "0.1.0"},
                },
            )

        elif method == "tools/list":
            respond(
                req_id,
                {
                    "tools": [
                        {
                            "name": "echo",
                            "description": "Echo the input back",
                            "inputSchema": {
                                "type": "object",
                                "properties": {"message": {"type": "string"}},
                                "required": ["message"],
                            },
                        },
                        {
                            "name": "fail_tool",
                            "description": "Returns isError:true in result",
                            "inputSchema": {"type": "object", "properties": {}},
                        },
                        {
                            "name": "die_tool",
                            "description": "Closes stdout to simulate a transport disconnect",
                            "inputSchema": {"type": "object", "properties": {}},
                        },
                    ]
                },
            )

        elif method == "tools/call":
            params = req.get("params", {})
            tool_name = params.get("name", "")
            args = params.get("arguments", {})

            if tool_name == "fail_tool":
                # Return a tool-level error payload (isError=true).
                # OC must surface this as a ToolReportedError, not as a
                # successful raw Value.
                respond(
                    req_id,
                    {
                        "isError": True,
                        "content": [
                            {"type": "text", "text": "tool-level error occurred"}
                        ],
                    },
                )
            elif tool_name == "die_tool":
                # Simulate transport disconnect during tools/call.
                sys.stdout.close()
                sys.exit(0)
            else:
                respond(
                    req_id,
                    {
                        "isError": False,
                        "content": [
                            {"type": "text", "text": json.dumps(args)}
                        ],
                    },
                )

        elif method == "resources/list":
            respond(
                req_id,
                {
                    "resources": [
                        {
                            "uri": "echo://hello",
                            "name": "hello",
                            "mimeType": "text/plain",
                        }
                    ]
                },
            )

        elif method == "resources/read":
            respond(
                req_id,
                {
                    "contents": [
                        {"uri": "echo://hello", "text": "hello world", "mimeType": "text/plain"}
                    ]
                },
            )

        elif method == "rpc_error_with_data":
            # Synthetic method to test error-code surfacing (B5).
            # Returns a JSON-RPC error that includes a structured `data` field.
            error_response(
                req_id,
                -32099,
                "custom server error",
                {"detail": "extra context"},
            )

        elif method == "die":
            # Simulate transport disconnect mid-call (B6).
            # Close stdout without writing a response; the client's read_line
            # will see EOF and return a Transport error.
            sys.stdout.close()
            sys.exit(0)

        else:
            error_response(req_id, -32601, f"Method not found: {method}")


if __name__ == "__main__":
    main()
