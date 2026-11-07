"""Isolated relay -> original app-server -> local tool provider regression."""
import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from email.parser import BytesParser
from email.policy import default
import http.server
import json
import os
from pathlib import Path
import queue
import shutil
import socket
import subprocess
import tempfile
import threading
import time

import httpx
from test_native_standalone_tools import Fixture, MODEL, PNG, ROOT, connection_pid


class Provider(Fixture):
    def do_POST(self):
        if self.server.case == "search_partial":
            return self.partial_search()
        if self.server.case not in ("masked", "sized", "invalid_background"):
            return super().do_POST()
        try:
            raw = self.rfile.read(int(self.headers["Content-Length"]))
            assert self.headers["Authorization"] == "Bearer " + self.server.synthetic_token, "Direct auth must use the native account credential"
            assert self.path.startswith("/v1/images/")
            if self.server.case == "masked":
                parsed = BytesParser(policy=default).parsebytes(b"Content-Type: " + self.headers["Content-Type"].encode() + b"\r\n\r\n" + raw)
                fields = {p.get_param("name", header="content-disposition"): p.get_payload(decode=True) for p in parsed.iter_parts()}
                assert fields["mask"] == PNG and fields["image[]"] == PNG
            elif self.server.case == "invalid_background":
                assert json.loads(raw)["background"] is True
            else:
                assert json.loads(raw)["size"] == "1024x1024"
            pid = connection_pid(self.client_address[1], self.server.server_port)
            if os.name == "nt":
                assert pid == self.server.native_pid, "Unsupported parameters must use relay direct transport"
            self.server.requests.append({"case": self.server.case, "path": self.path, "pid": pid})
            if self.server.case == "invalid_background":
                return self.reply({"error": {"message": "Invalid background type"}}, 400)
            self.reply({"created": 1, "data": [{"b64_json": base64.b64encode(PNG).decode()}], "usage": {"input_tokens": 7, "output_tokens": 11}})
        except Exception as error:
            self.server.failures.put(repr(error))
            self.reply({"error": {"message": "Fixture failed"}}, 400)

    def partial_search(self):
        if self.path in ("/codex/analytics", "/codex/events", "/events"):
            self.rfile.read(int(self.headers.get("Content-Length", "0")))
            return self.reply({})
        try:
            assert self.headers["Authorization"] == "Bearer SYNTHETIC_NATIVE_TOOLS"
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            self.server.requests.append({"case": "search_partial", "path": self.path, "body": body})
            count = sum(r["case"] == "search_partial" and r["path"] == self.path for r in self.server.requests)
            if self.path == "/v1/alpha/search":
                assert count <= 2, "Failed search must not be replayed"
                if count == 1:
                    return self.reply({"output":"First search succeeded","results":[{"url":"https://example.test"}]})
                return self.reply({"error":{"message":"Second search failed"}}, 400)
            assert self.path == "/v1/responses" and count <= 3
            if count < 3:
                item = {"type":"function_call","id":f"fc_partial_{count}","call_id":f"partial_{count}",
                        "namespace":"web","name":"run","arguments":json.dumps({"search_query":[{"q":f"fixture {count}"}]})}
            else:
                item = {"type":"message","id":"msg_partial","role":"assistant","status":"completed",
                        "content":[{"type":"output_text","text":"Done despite failed search"}]}
            events = [{"type":"response.output_item.done","output_index":0,"item":item},
                      {"type":"response.completed","response":{"id":f"resp_partial_{count}","status":"completed", "output":[item],
                       "usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}}]
            self.reply("".join("data: " + json.dumps(e) + "\n\n" for e in events).encode(), content_type="text/event-stream")
        except Exception as error:
            self.server.failures.put(repr(error))
            self.reply({"error":{"message":"Partial search fixture failed"}}, 400)


def run(binary, installation_path):
    fixture = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    fixture.daemon_threads = True
    fixture.direct_calls = True
    fixture.lock, fixture.failures = threading.Lock(), queue.Queue()
    fixture.requests, fixture.case = [], ""
    fixture.entered, fixture.release = threading.Event(), threading.Event()
    serving = threading.Thread(target=fixture.serve_forever, daemon=True)
    serving.start()
    installation = json.loads(installation_path.read_text(encoding="utf-8-sig"))
    fixture.native_executable = installation["appServerExecutable"]
    try:
        with tempfile.TemporaryDirectory(prefix="relay-official-tools-") as temporary:
            root = Path(temporary)
            (root / ".cache").mkdir()
            (root / "web").mkdir()
            (root / "web/index.html").write_text("isolated native tools")
            (root / "home").mkdir()
            shutil.copyfile(installation_path, root / ".cache/launch-config.json")
            claims = {"email": "fixture@example.test", "sub": "fixture", "exp": int(time.time()) + 3600,
                      "https://api.openai.com/auth": {"chatgpt_account_id": "fixture", "chatgpt_user_id": "fixture", "chatgpt_plan_type": "pro"}}
            token = "e30." + base64.urlsafe_b64encode(json.dumps(claims).encode()).decode().rstrip("=") + ".synthetic"
            fixture.synthetic_token = token
            (root / "home/auth.json").write_text(json.dumps({"auth_mode": "chatgpt", "OPENAI_API_KEY": None,
                "tokens": {"access_token": token, "id_token": token, "refresh_token": "synthetic", "account_id": "fixture"},
                "last_refresh": datetime.now(timezone.utc).isoformat()}), encoding="utf-8")
            with socket.socket() as probe:
                probe.bind(("127.0.0.1", 0))
                port = probe.getsockname()[1]
            (root / "home/config.toml").write_text(f'''model="{MODEL}"
model_provider="native_fixture"
cli_auth_credentials_store="file"
chatgpt_base_url="http://127.0.0.1:{fixture.server_port}"
[analytics]
enabled=false
[features]
remote_models=false
secret_auth_storage=false
respect_system_proxy=false
system_proxy_fallback=false
enable_request_compression=false
[model_providers.native_fixture]
name="OpenAI"
base_url="http://127.0.0.1:{fixture.server_port}/v1"
wire_api="responses"
requires_openai_auth=true
supports_standalone_web_search=true
supports_websockets=false
request_max_retries=0
stream_max_retries=0
experimental_bearer_token="SYNTHETIC_NATIVE_TOOLS"
''', encoding="utf-8")
            env = {k: v for k, v in os.environ.items() if not k.upper().startswith(("CODEX_", "OPENAI_", "CHATGPT_", "ANTHROPIC_", "AZURE_OPENAI_"))}
            for name in list(env):
                if name.upper() in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"):
                    del env[name]
            env["CODEX_HOME"] = str(root / "home")
            env["NO_PROXY"] = "127.0.0.1,localhost,::1"
            with (root / "service.log").open("wb") as log, httpx.Client(base_url=f"http://127.0.0.1:{port}", trust_env=False, timeout=150) as admin:
                service = subprocess.Popen([str(binary.resolve()), "serve", "--port", str(port), "--web-dir", str(root / "web")],
                    cwd=root, env=env, stdout=log, stderr=log, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
                fixture.native_pid = service.pid
                try:
                    for _ in range(150):
                        assert service.poll() is None, "Relay exited"
                        try:
                            if admin.get("/healthz").is_success:
                                break
                        except httpx.HTTPError:
                            pass
                        time.sleep(0.1)

                    def post(path, body):
                        response = admin.post(path, json=body)
                        assert response.is_success, (path, response.text)
                        return response.json()

                    account = post("/api/accounts", {"name": "Native tool fixture", "nativeConfig": True})
                    revision = admin.get("/api/operations").json()["revision"]
                    post(f"/api/accounts/{account['id']}/route", {"revision": revision, "enabled": True})
                    key = post("/api/api-keys", {"name": "Native tools", "groupId": "00000000-0000-0000-0000-000000000000"})
                    headers = {"Authorization": "Bearer " + key["secret"]}
                    image = {"model": "gpt-image-2", "prompt": "fixture", "background": "transparent", "quality": "auto", "size": "auto"}
                    settings = {"search_context_size":"high", "filters":{"allowed_domains":["example.com"]},
                                "user_location":{"type":"approximate","country":"US","city":"Boston","timezone":"America/New_York"}}
                    for case in ("generate", "edit", "search", "open", "image_error", "masked", "sized", "invalid_background"):
                        fixture.case = case
                        if case == "masked":
                            response = admin.post("/v1/images/edits", data={"model": "gpt-image-2", "prompt": "edit"},
                                files=[("image[]", ("image.png", PNG, "image/png")), ("mask", ("mask.png", PNG, "image/png"))], headers=headers)
                        elif case in ("search", "open"):
                            commands = {"search_query": [{"q": "native fixture"}]} if case == "search" else {"open": [{"ref_id": "turn0search0"}]}
                            response = admin.post("/v1/alpha/search", json={"id": "native-session", "model": MODEL, "input": [], "commands": commands, "settings":settings}, headers=headers)
                        else:
                            body = {**image, "prompt": case}
                            if case == "edit":
                                body["images"] = ["data:image/png;base64," + base64.b64encode(PNG).decode()]
                            if case == "sized":
                                body["size"] = "1024x1024"
                            if case == "invalid_background":
                                body["background"] = True
                            response = admin.post("/v1/images/edits" if case == "edit" else "/v1/images/generations", json=body, headers=headers)
                        if not fixture.failures.empty():
                            raise AssertionError(fixture.failures.get())
                        if case in ("image_error", "invalid_background"):
                            assert not response.is_success, response.text
                            if case == "invalid_background":
                                assert response.status_code == 400, response.text
                                assert response.headers["x-codex-relay-upstream-transport"] == "direct-http"
                        else:
                            assert response.status_code == 200, (case, response.text)
                            assert response.headers["x-codex-relay-upstream-transport"] == ("direct-http" if case in ("masked", "sized") else "native-tools")
                            if case in ("search", "open"):
                                assert response.json()["output"] == "Native search output with turn0search0"
                                assert response.json()["results"][0]["ref_id"] == "turn0search0"
                                assert "encrypted_output" not in response.json()
                                assert response.json()["usage"]["total_tokens"] == 24
                                assert response.json()["native_usage"]["totalTokens"] == 24
                                sent = next(r["body"]["settings"] for r in fixture.requests if r["case"] == case and r["path"] == "/v1/alpha/search")
                                assert sent["search_context_size"] == "high", sent
                                assert sent["filters"]["allowed_domains"] == ["example.com"], sent
                                assert sent["user_location"]["city"] == "Boston", sent
                            else:
                                assert base64.b64decode(response.json()["data"][0]["b64_json"]) == PNG
                        requests = [r for r in fixture.requests if r["case"] == case]
                        if case == "invalid_background":
                            assert all(r["path"] != "/v1/responses" for r in requests), "Invalid background must not start a native turn"
                        assert sum(r["path"] != "/v1/responses" for r in requests) == 1, requests
                        for model_request in (r for r in requests if r["path"] == "/v1/responses"):
                            tool_specs = list(model_request["body"].get("tools", []))
                            for item in model_request["body"]["input"]:
                                if item["type"] == "additional_tools":
                                    tool_specs.extend(item["tools"])
                            expected = "web" if case in ("search", "open") else "image_gen"
                            assert [(t["type"], t["name"]) for t in tool_specs] == [("namespace", expected)], "Unexpected host tools exposed"
                        if case == "open":
                            ids = [r["body"]["id"] for r in fixture.requests if r["path"] == "/v1/alpha/search"]
                            assert len(set(ids)) == 1
                        print("PASS relay " + case, flush=True)
                        if case == "open":
                            count = len(fixture.requests)
                            changed = admin.post("/v1/alpha/search", json={"id": "native-session", "model": MODEL,
                                "input": [], "commands": commands, "settings": {"external_web_access": False}}, headers=headers)
                            assert not changed.is_success and changed.json()["error"]["code"] == "SEARCH_SESSION_EXPIRED"
                            assert len(fixture.requests) == count, "Changed search settings must not start another model turn"
                            print("PASS changed search settings preserve the original session boundary", flush=True)
                    fixture.case = "search_partial"
                    partial = admin.post("/v1/alpha/search", json={"id":"partial-session","model":MODEL,
                        "commands":{"search_query":[{"q":"fixture"}]}}, headers=headers)
                    assert not partial.is_success, partial.text
                    assert partial.json()["error"]["code"] == "NATIVE_TOOL_PROTOCOL", partial.text
                    assert sum(r["case"] == "search_partial" and r["path"] == "/v1/alpha/search" for r in fixture.requests) == 2
                    assert fixture.failures.empty(), list(fixture.failures.queue)
                    record = next(r for r in admin.get("/api/statistics").json()["records"] if r["id"] == partial.headers["x-request-id"])
                    assert record["status"] == "failed", record
                    assert record["inputTokens"] == 30 and record["outputTokens"] == 6, record
                    print("PASS partial search failure preserves confirmed usage without success or replay", flush=True)
                    fixture.case = "cancel"
                    with ThreadPoolExecutor(max_workers=1) as executor:
                        pending = executor.submit(admin.post, "/v1/images/generations", json=image, headers=headers)
                        assert fixture.entered.wait(60)
                        post(f"/api/api-keys/{key['key']['id']}", {"revision": key['key']['revision'], "state": "revoked"})
                        cancelled = pending.result(timeout=15)
                        assert not cancelled.is_success
                    fixture.release.set()
                    assert admin.get("/api/operations").json()["active"] == 0
                    print("PASS relay revocation cancels native image and releases lease", flush=True)
                except Exception:
                    log.flush()
                    (ROOT / ".cache/native-tools-relay-requests.json").write_text(json.dumps(fixture.requests, ensure_ascii=False), encoding="utf-8")
                    print((root / "service.log").read_text(encoding="utf-8", errors="replace")[-3500:], flush=True)
                    raise
                finally:
                    fixture.release.set()
                    try:
                        admin.post("/api/service/stop", json={})
                    except httpx.HTTPError:
                        pass
                    try:
                        service.wait(timeout=15)
                    except subprocess.TimeoutExpired:
                        if os.name == "nt":
                            subprocess.run(["taskkill", "/PID", str(service.pid), "/T", "/F"], check=False,
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=subprocess.CREATE_NO_WINDOW)
                        else:
                            service.terminate()
                        service.wait(timeout=10)
    finally:
        fixture.release.set()
        fixture.shutdown()
        fixture.server_close()
        serving.join(timeout=5)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--relay-binary", type=Path, default=ROOT / "target/debug/codex-relay.exe")
    parser.add_argument("--installation", type=Path, default=ROOT / ".cache/launch-config.json")
    args = parser.parse_args()
    run(args.relay_binary, args.installation)
