"""Desktop-equivalent RPC client using an unchanged official binary, synthetic auth only."""
import json
import os
from pathlib import Path
import queue
import subprocess
import threading
import time


def native_turn(root, installation, base_url, key, name, use_ws=False, steer_when=None):
    home = Path(root) / name
    workspace = home / "workspace"
    workspace.mkdir(parents=True)
    (home / "config.toml").write_text(f'''
model="gpt-6.1-sol"
model_provider="fixture"
cli_auth_credentials_store="file"
[features]
shell_tool=false
apps=false
plugins=false
hooks=false
multi_agent=false
enable_request_compression=true
[model_providers.fixture]
name="Synthetic desktop fixture"
base_url={json.dumps(base_url)}
wire_api="responses"
requires_openai_auth=false
supports_websockets={str(use_ws).lower()}
request_max_retries=0
stream_max_retries=0
experimental_bearer_token={json.dumps(key)}
''', encoding="utf-8")
    env = {k: v for k, v in os.environ.items() if not k.upper().startswith(("CODEX_", "OPENAI_", "CHATGPT_", "ANTHROPIC_", "AZURE_OPENAI_"))}
    env["CODEX_HOME"] = str(home)
    env["NO_PROXY"] = "*"
    messages = queue.Queue()
    with (home / "native.log").open("wb") as log:
        proc = subprocess.Popen([installation["appServerExecutable"], "app-server", "--listen", "stdio://"], cwd=workspace, env=env,
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log,
                                creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        def reader():
            for line in proc.stdout:
                messages.put(json.loads(line))
        thread = threading.Thread(target=reader, daemon=True)
        thread.start()
        counter = 0
        def send(value):
            proc.stdin.write((json.dumps(value) + "\n").encode())
            proc.stdin.flush()
        def rpc(method, params):
            nonlocal counter
            counter += 1
            send({"id": counter, "method": method, "params": params})
            while True:
                value = messages.get(timeout=90)
                if value.get("id") == counter:
                    assert "error" not in value, value.get("error")
                    return value["result"]
        try:
            rpc("initialize", {"clientInfo": installation["nativeClientInfo"], "capabilities": {"experimentalApi": True}})
            send({"method": "initialized"})
            start = rpc("thread/start", {"model": "gpt-6.1-sol", "modelProvider": "fixture", "cwd": str(workspace),
                "approvalPolicy": "untrusted", "sandbox": "read-only", "ephemeral": True,
                "baseInstructions": "DESKTOP_RPC_BASE", "developerInstructions": "DESKTOP_RPC_DEVELOPER",
                "experimentalRawEvents": True, "environments": [], "runtimeWorkspaceRoots": [],
                "config": {"features.code_mode": {"enabled": False, "direct_only_tool_namespaces": ["functions"]}, "project_doc_max_bytes": 0},
                "dynamicTools": [{"type": "function", "name": "client_echo", "description": "Client echo", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}]})
            native_thread = start["thread"]["id"]
            turn=rpc("turn/start", {"threadId": native_thread, "input": [{"type": "text", "text": "STEER_HOLD desktop RPC" if steer_when else "TOOL_ROUNDTRIP desktop RPC", "text_elements": []}]})
            if steer_when:
                deadline=time.monotonic()+30
                while not steer_when():
                    assert time.monotonic()<deadline, "Native model request did not start"
                    time.sleep(0.02)
                rpc("turn/steer", {"threadId":native_thread,"expectedTurnId":turn["turn"]["id"],"input":[{"type":"text","text":"STEER_ADDED","text_elements":[]}]})
            output, calls = [], 0
            while True:
                value = messages.get(timeout=90)
                method, params = value.get("method"), value.get("params", {})
                if method == "item/tool/call":
                    assert params["tool"] == "client_echo", params
                    calls += 1
                    send({"id": value["id"], "result": {"success": True, "contentItems": [{"type": "inputText", "text": "CLIENT_EXECUTED_RESULT"}]}})
                elif "id" in value and method:
                    raise AssertionError(f"Unexpected native callback: {method}")
                elif method == "rawResponseItem/completed" and params["item"]["type"] == "message" and params["item"].get("role") == "assistant":
                    output.append(params["item"])
                elif method == "turn/completed":
                    assert params["turn"]["status"] == "completed", params["turn"]
                    assert calls == (0 if steer_when else 1) and output, (calls, output)
                    return output
        finally:
            proc.stdin.close()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                if os.name == "nt":
                    subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                   creationflags=subprocess.CREATE_NO_WINDOW, check=False)
                else:
                    proc.kill()
                proc.wait(timeout=10)
            thread.join(timeout=5)
            proc.stdout.close()
