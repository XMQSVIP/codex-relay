"""Probe standalone image/search execution inside the unmodified official app-server.

Uses isolated configuration, synthetic credentials, and a loopback-only provider.
No real model, image, search, or account request is sent. The local model fixture
selects tools deterministically; it does not prove a live model follows a prompt.
"""
import argparse
import base64
from datetime import datetime, timezone
import hashlib
import http.server
import json
import os
from pathlib import Path
import queue
import subprocess
import socket
import struct
import sys
import tempfile
import threading
import time

from native_tools_client import Native, collect

ROOT = Path(__file__).resolve().parent.parent
PNG = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFUlEQVR4nGNgYPhPIhrVMKph2GoAAJLb/wFh5Z4RAAAAAElFTkSuQmCC")
MODEL = "gpt-6.1-sol"


def connection_pid(client_port, server_port):
    """Read Windows TCP ownership while the accepted connection is still open."""
    if os.name != "nt":
        return None
    import ctypes
    from ctypes import wintypes
    size = wintypes.ULONG(0)
    api = ctypes.windll.iphlpapi.GetExtendedTcpTable
    status = api(None, ctypes.byref(size), False, socket.AF_INET, 5, 0)
    if status != 122:  # ERROR_INSUFFICIENT_BUFFER
        raise RuntimeError("Unable to size TCP ownership table: " + str(status))
    buffer = ctypes.create_string_buffer(size.value)
    status = api(buffer, ctypes.byref(size), False, socket.AF_INET, 5, 0)
    if status:
        raise RuntimeError("Unable to read TCP ownership table: " + str(status))
    rows = struct.unpack_from("<I", buffer.raw)[0]
    for index in range(rows):
        row = struct.unpack_from("<6I", buffer.raw, 4 + index * 24)
        if row[0] == 5 and socket.ntohs(row[2] & 65535) == client_port and socket.ntohs(row[4] & 65535) == server_port:
            return row[5]
    raise RuntimeError("Native connection missing from TCP ownership table")


def verify_native_process(pid, launched, executable):
    # Windows' packaged launcher can spawn the actual app-server as a child.
    command = (f"Get-CimInstance Win32_Process -Filter 'ProcessId={pid}' | "
               "Select-Object ProcessId,ParentProcessId,ExecutablePath | ConvertTo-Json -Compress")
    raw = subprocess.check_output(["powershell", "-NoProfile", "-Command", command],
        creationflags=subprocess.CREATE_NO_WINDOW, timeout=10)
    process = json.loads(raw)
    assert process.get("ExecutablePath"), process
    assert Path(process["ExecutablePath"]).resolve() == Path(executable).resolve(), process
    current = process
    for _ in range(8):
        if current["ProcessId"] == launched:
            return
        parent = current["ParentProcessId"]
        command = (f"Get-CimInstance Win32_Process -Filter 'ProcessId={parent}' | "
                   "Select-Object ProcessId,ParentProcessId | ConvertTo-Json -Compress")
        raw = subprocess.check_output(["powershell", "-NoProfile", "-Command", command],
            creationflags=subprocess.CREATE_NO_WINDOW, timeout=10)
        current = json.loads(raw)
    raise AssertionError("Connection is not owned by the launched native process tree")


class Fixture(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, value, status=200, content_type="application/json"):
        data = value if isinstance(value, bytes) else json.dumps(value).encode()
        try:
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        except (BrokenPipeError, ConnectionError):
            pass

    def do_POST(self):
        try:
            if self.path in ("/codex/analytics", "/codex/events", "/events"):
                self.rfile.read(int(self.headers.get("Content-Length", "0")))
                self.reply({})
                return
            assert self.headers.get("Authorization") == "Bearer SYNTHETIC_NATIVE_TOOLS", "Unexpected authorization on " + self.path
            assert self.headers.get("Content-Encoding", "identity") == "identity"
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            pid = connection_pid(self.client_address[1], self.server.server_port)
            if os.name == "nt":
                verify_native_process(pid, self.server.native_pid, self.server.native_executable)
            with self.server.lock:
                case = self.server.case
                self.server.requests.append({"case": case, "path": self.path, "body": request, "pid": pid})
            if self.path == "/v1/responses":
                count = sum(r["path"] == self.path and r["case"] == case for r in self.server.requests)
                if count > 2:
                    raise AssertionError("Unexpected model retry or extra tool loop")
                if count == 2:
                    item = {"type": "message", "id": "msg_" + case, "role": "assistant",
                            "content": [{"type": "output_text", "text": "NATIVE_TOOL_DONE"}], "status": "completed"}
                else:
                    if case in ("generate", "edit", "image_error", "cancel", "cli_generate"):
                        args = {"prompt": case, "transparent_background": True}
                        if case == "edit":
                            args["num_last_images_to_include"] = 1
                        code = "generatedImage(await tools.image_gen__imagegen(" + json.dumps(args) + "));"
                    else:
                        args = {"search_query": [{"q": "native fixture"}]} if case == "search" else {"open": [{"ref_id": "turn0search0"}]}
                        code = "text(await tools.web__run(" + json.dumps(args) + "));"
                    if getattr(self.server, "direct_calls", False):
                        image_call = case in ("generate", "edit", "image_error", "cancel", "cli_generate")
                        item = {"type": "function_call", "id": "fc_" + case, "namespace": "image_gen" if image_call else "web",
                                "name": "imagegen" if image_call else "run", "call_id": "call_" + case,
                                "arguments": json.dumps(args)}
                    else:
                        item = {"type": "custom_tool_call", "id": "ctc_" + case, "namespace": "functions",
                                "name": "exec", "call_id": "call_" + case, "input": code}
                events = [{"type": "response.output_item.done", "output_index": 0, "item": item},
                          {"type": "response.completed", "response": {"id": "resp_" + case + str(count),
                           "status": "completed", "output": [item], "usage": {"input_tokens": 10, "output_tokens": 2, "total_tokens": 12}}}]
                self.reply("".join("data: " + json.dumps(e) + "\n\n" for e in events).encode(), content_type="text/event-stream")
            elif self.path in ("/v1/images/generations", "/v1/images/edits"):
                assert request["model"] == "gpt-image-2"
                assert request["quality"] == "auto" and request["size"] == "auto"
                assert request["background"] == "transparent"
                if case == "edit":
                    assert request["images"], "Official edit omitted input image"
                if case == "image_error":
                    self.reply({"error": {"message": "Synthetic image rejection", "type": "invalid_request_error"}}, 400)
                else:
                    if case == "cancel":
                        self.server.entered.set()
                        self.server.release.wait(20)
                    self.reply({"created": 1, "background": "transparent", "quality": "auto", "size": "auto",
                                "data": [{"b64_json": base64.b64encode(PNG).decode(), "generation_id": "fixture-generation"}],
                                "usage": {"input_tokens": 7, "output_tokens": 11, "total_tokens": 18}})
            elif self.path == "/v1/alpha/search":
                assert request["model"] == MODEL
                assert request["id"] and request["commands"]
                if case == "open":
                    assert request["commands"]["open"][0]["ref_id"] == "turn0search0"
                self.reply({"output": "Native search output with turn0search0", "encrypted_output": "OPAQUE_FIXTURE",
                            "results": [{"ref_id": "turn0search0", "url": "https://example.test", "future_field": True}]})
            else:
                raise AssertionError("Unexpected native HTTP request: " + self.path)
        except Exception as error:
            self.server.failures.put(self.path + ": " + repr(error))
            self.reply({"error": {"message": "Fixture assertion failed"}}, 400)



def probe(installation, report_path):
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    server.daemon_threads = True
    server.lock, server.failures = threading.Lock(), queue.Queue()
    server.requests, server.case = [], ""
    server.entered, server.release = threading.Event(), threading.Event()
    serving = threading.Thread(target=server.serve_forever, daemon=True)
    serving.start()
    result = {"nativeVersion": installation.get("appServerVersion"), "nativeSha256": hashlib.sha256(
        Path(installation["appServerExecutable"]).read_bytes()).hexdigest(), "realUpstream": False, "cases": []}
    try:
        with tempfile.TemporaryDirectory(prefix="native-tools-") as directory:
            home = Path(directory)
            workspace = home / "workspace"
            workspace.mkdir()
            claims = {"email": "fixture@example.test", "sub": "fixture", "exp": int(time.time()) + 3600,
                      "https://api.openai.com/auth": {"chatgpt_account_id": "fixture", "chatgpt_user_id": "fixture", "chatgpt_plan_type": "pro"}}
            token = "e30." + base64.urlsafe_b64encode(json.dumps(claims).encode()).decode().rstrip("=") + ".synthetic"
            (home / "auth.json").write_text(json.dumps({"auth_mode": "chatgpt", "OPENAI_API_KEY": None,
                "tokens": {"access_token": token, "id_token": token, "refresh_token": "synthetic", "account_id": "fixture"},
                "last_refresh": datetime.now(timezone.utc).isoformat()}), encoding="utf-8")
            (home / "config.toml").write_text(f'''model="{MODEL}"
model_provider="native_fixture"
cli_auth_credentials_store="file"
chatgpt_base_url="http://127.0.0.1:{server.server_port}"
web_search="live"
[analytics]
enabled=false
[otel]
exporter="none"
trace_exporter="none"
log_user_prompt=false
[features]
respect_system_proxy=false
system_proxy_fallback=false
remote_models=false
secret_auth_storage=false
image_generation=true
shell_tool=false
apps=false
plugins=false
hooks=false
multi_agent=false
enable_request_compression=false
[model_providers.native_fixture]
name="OpenAI"
base_url="http://127.0.0.1:{server.server_port}/v1"
wire_api="responses"
requires_openai_auth=true
supports_standalone_web_search=true
supports_websockets=false
request_max_retries=0
stream_max_retries=0
experimental_bearer_token="SYNTHETIC_NATIVE_TOOLS"
''', encoding="utf-8")
            with (home / "native.log").open("wb") as log:
                native = Native(installation, home, workspace, log, isolated=True, timeout=90)
                server.native_pid = native.process.pid
                server.native_executable = installation["appServerExecutable"]
                result["nativePid"] = native.process.pid
                try:
                    native.start(installation, workspace)
                    for case in ("generate", "edit", "search", "open", "image_error", "cancel"):
                        server.case = case
                        native.begin(case)
                        if case == "cancel":
                            assert server.entered.wait(30), "Image request did not reach the provider"
                            native.rpc("turn/interrupt", {"threadId": native.thread, "turnId": native.turn})
                        turn = native.finish()
                        expected = "interrupted" if case == "cancel" else "completed"
                        if not server.failures.empty():
                            raise AssertionError(server.failures.get())
                        assert turn["status"] == expected, turn
                        requests = [r for r in server.requests if r["case"] == case]
                        tool_requests = [r for r in requests if r["path"] != "/v1/responses"]
                        assert len(tool_requests) == 1, {"case": case, "paths": [r["path"] for r in requests], "events": native.events}
                        items = [e["params"]["item"] for e in native.events if e.get("method") == "item/completed"]
                        if case in ("generate", "edit"):
                            item = next(i for i in items if i["type"] == "imageGeneration")
                            assert item["status"] == "completed"
                            saved = Path(item["savedPath"])
                            assert saved.resolve().is_relative_to(home.resolve()), str(saved)
                            assert Path(item["savedPath"]).read_bytes() == PNG
                        if case == "image_error":
                            item = next(i for i in items if i["type"] == "imageGeneration")
                            assert item["status"] == "failed", item
                            try:
                                collect(native.events, "image", workspace)
                            except RuntimeError as error:
                                assert "image tool failed" in str(error)
                            else:
                                raise AssertionError("Failed image treated as success")
                        if case == "open":
                            initial = next(r for r in server.requests if r["case"] == "search" and r["path"] == "/v1/alpha/search")
                            assert tool_requests[0]["body"]["id"] == initial["body"]["id"]
                        if case in ("search", "open"):
                            assert any(i["type"] == "webSearch" for i in items), items
                            follow = next(r for r in requests if r["path"] == "/v1/responses" and
                                          "Native search output with turn0search0" in json.dumps(r["body"]))
                            assert follow
                        if case in ("generate", "edit", "search", "open"):
                            output = workspace / case
                            output.mkdir()
                            extracted = collect(native.events, "image" if case in ("generate", "edit") else "search", output)
                            assert extracted["items"] and extracted["imageUsage"] is None
                            if case in ("generate", "edit"):
                                assert Path(extracted["items"][0]["path"]).read_bytes() == PNG
                            else:
                                assert extracted["items"][0]["results"][0]["ref_id"] == "turn0search0"
                        record = {"case": case, "turnStatus": turn["status"], "toolPath": tool_requests[0]["path"],
                                  "modelRequests": len(requests) - 1, "toolRequests": 1,
                                  "connectionPid": tool_requests[0]["pid"],
                                  "connectionOwnershipVerified": os.name == "nt",
                                  "itemTypes": sorted(set(i["type"] for i in items))}
                        if case in ("generate", "edit", "image_error"):
                            record["imageStatus"] = item["status"]
                            record["imageRpcFields"] = sorted(item)
                        if case in ("search", "open"):
                            record["encryptedOutputInRpc"] = "OPAQUE_FIXTURE" in json.dumps(native.events)
                            record["searchOutputInRpc"] = "Native search output with turn0search0" in json.dumps(native.events)
                        result["cases"].append(record)
                        print("PASS " + json.dumps(record), flush=True)
                except Exception:
                    log.flush()
                    result["nativeLogTail"] = (home / "native.log").read_text(encoding="utf-8", errors="replace")[-6000:]
                    raise
                finally:
                    server.release.set()
                    native.close()
                    result["processExited"] = native.process.poll() is not None
                # Exercise real-client config discovery with only synthetic local credentials.
                server.case = "cli_generate"
                client_output = workspace / "client-result"
                cli_installation = home / "installation.json"
                cli_installation.write_text(json.dumps(installation), encoding="utf-8")
                env = dict(os.environ)
                for name in list(env):
                    if name.upper().startswith(("CODEX_", "OPENAI_", "CHATGPT_", "ANTHROPIC_", "AZURE_OPENAI_")):
                        del env[name]
                env["CODEX_HOME"] = str(home)
                env["NO_PROXY"] = "127.0.0.1,localhost,::1"
                # The separate CLI starts another original binary. Verify its executable;
                # its parent is this test's explicitly spawned client process.
                cli = subprocess.Popen([sys.executable, str(ROOT / "scripts/native_tools_client.py"),
                    "--installation", str(cli_installation), "--tool", "image", "--prompt", "CLI fixture",
                    "--output", str(client_output)], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
                server.native_pid = cli.pid
                try:
                    stdout, stderr = cli.communicate(timeout=90)
                    assert cli.returncode == 0, stderr.decode("utf-8", errors="replace")
                    value = json.loads(stdout)
                    assert value["transport"] == "official-app-server-tool-turn"
                    assert (client_output / "image-1.png").read_bytes() == PNG
                    result["cliPassed"] = True
                    print("PASS standalone client config discovery and image extraction", flush=True)
                finally:
                    if cli.poll() is None:
                        if os.name == "nt":
                            subprocess.run(["taskkill", "/PID", str(cli.pid), "/T", "/F"],
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False,
                                creationflags=subprocess.CREATE_NO_WINDOW, timeout=15)
                        else:
                            cli.kill()
                        cli.wait(timeout=15)
    finally:
        server.release.set()
        server.shutdown()
        server.server_close()
        serving.join(timeout=5)
        report_path.parent.mkdir(parents=True, exist_ok=True)
        report_path.write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")
    print("Report: " + str(report_path), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--installation", type=Path, default=ROOT / ".cache/launch-config.json")
    parser.add_argument("--report", type=Path, default=ROOT / ".cache/native-standalone-tools.json")
    args = parser.parse_args()
    probe(json.loads(args.installation.read_text(encoding="utf-8-sig")), args.report)
