#!/usr/bin/env python3
"""End-to-end modern MCP HTTP checks against an isolated Target Ops process."""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request


def rpc(url, method, ident, params=None, *, modern=True, headers=None):
    params = dict(params or {})
    request_headers = {
        "Content-Type": "application/json",
        "Accept": "application/json, text/event-stream",
    }
    if modern:
        params["_meta"] = {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.modelcontextprotocol/clientInfo": {"name": "smoke", "version": "1"},
        }
        request_headers.update({
            "MCP-Protocol-Version": "2026-07-28",
            "Mcp-Method": method,
        })
        if method == "tools/call":
            request_headers["Mcp-Name"] = params["name"]
    request_headers.update(headers or {})
    data = json.dumps({
        "jsonrpc": "2.0", "id": ident, "method": method, "params": params,
    }).encode()
    request = urllib.request.Request(url, data, headers=request_headers)
    try:
        response = urllib.request.urlopen(request, timeout=5)
    except urllib.error.HTTPError as err:
        response = err
    with response:
        return response.status, response.headers, json.load(response)


def main():
    with tempfile.TemporaryDirectory(prefix="target-ops-smoke-") as temp:
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        config = Path(temp) / "config.toml"
        config.write_text(
            f"""[config]
rewrite_on_start = false
[server]
oauth_enabled = false
name = 'mcp-smoke'
public_base_url = 'http://127.0.0.1:{port}'
[activity]
enabled = false
"""
        )
        # Do not inherit the production authentication overrides in this isolated test.
        env = {k: v for k, v in os.environ.items()
               if not k.startswith("MCP_TARGET_OPS_")}
        executable = Path(__file__).resolve().parents[1] / "target/debug/mcp-target-ops"
        with (Path(temp) / "server.log").open("w") as log:
            server = subprocess.Popen(
                [str(executable), "--config", str(config),
                 "--http", f"127.0.0.1:{port}"],
                stdout=log, stderr=log, env=env,
            )
            try:
                for _ in range(40):
                    try:
                        with urllib.request.urlopen(
                            f"http://127.0.0.1:{port}/health", timeout=0.5
                        ):
                            break
                    except (OSError, urllib.error.URLError):
                        time.sleep(0.1)
                else:
                    raise AssertionError(
                        "smoke server startup failed: "
                        + (Path(temp) / "server.log").read_text()[-1500:]
                    )
                url = f"http://127.0.0.1:{port}/mcp"
                status, headers, result = rpc(
                    url, "tools/call", 1, {"name": "target_list", "arguments": {}}
                )
                assert status == 200 and result["result"]["resultType"] == "complete"
                assert "Mcp-Session-Id" not in headers
                assert isinstance(result["result"]["structuredContent"]["targets"], list)

                status, headers, result = rpc(url, "server/discover", 2)
                assert status == 200 and "Mcp-Session-Id" not in headers
                assert result["result"]["supportedVersions"][0] == "2026-07-28"
                assert result["result"]["resultType"] == "complete"

                status, _, result = rpc(url, "tools/list", 3)
                assert status == 200 and result["result"]["resultType"] == "complete"
                assert len(result["result"]["tools"]) >= 20

                status, _, result = rpc(url, "not/implemented", 4)
                assert status == 404 and result["error"]["code"] == -32601

                status, _, result = rpc(
                    url, "tools/call", 5, {"name": "target_list", "arguments": {}},
                    headers={"Mcp-Name": "different"},
                )
                assert status == 400 and result["error"]["code"] == -32020

                status, _, result = rpc(
                    url, "server/discover", 6,
                    headers={"MCP-Protocol-Version": "2099-01-01"},
                )
                assert status == 400 and result["error"]["code"] == -32020

                status, headers, result = rpc(
                    url, "initialize", 7,
                    {"protocolVersion": "2025-06-18"}, modern=False,
                )
                assert status == 200 and "Mcp-Session-Id" in headers
                assert result["result"]["protocolVersion"] == "2025-06-18"

                status, headers, result = rpc(
                    url, "server/discover", 8,
                    headers={"Mcp-Session-Id": "ignored-old-session"},
                )
                assert status == 200 and "Mcp-Session-Id" not in headers
                print("PASS: modern direct tools/call, discovery, tools/list, "
                      "404/400 validation, legacy handshake, stateless requests")
            finally:
                server.terminate()
                try:
                    server.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait(timeout=3)


if __name__ == "__main__":
    main()
