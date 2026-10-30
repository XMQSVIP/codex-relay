"""Standalone Images/Search integration via an isolated relay and native auth/config.

Default mode uses a strict local image provider. --real-upstream instead generates
one puppy image using the user's configured provider and saves it to --output.
"""
import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
from email.parser import BytesParser
from email.policy import default
import hashlib
import http.server
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import threading
import time

import httpx
from images_native_client import native_image_turn

ROOT = Path(__file__).resolve().parent.parent
PNG = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFUlEQVR4nGNgYPhPIhrVMKph2GoAAJLb/wFh5Z4RAAAAAElFTkSuQmCC")
seen = []
slow_entered = threading.Event()
slow_release = threading.Event()


class Images(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        raw = self.rfile.read(int(self.headers["Content-Length"]))
        assert self.headers["Authorization"] == "Bearer SYNTHETIC_IMAGE_PROVIDER"
        assert self.headers["Originator"] == "codex_desktop"
        assert self.headers["X-Image-Fixture"] == "native-config-header"
        if self.path == "/v1/alpha/search":
            assert len(self.headers.get_all("Content-Type")) == 1
            body = json.loads(raw)
            assert body["model"] == "gpt-6.1-sol" and body["id"] == "native-search-fixture"
            assert body["future_field"] == {"kept": True}
            seen.append({"path": self.path, "prompt": "SEARCH"})
            failed = body["commands"].get("test_failure", False)
            value = ({"error": {"code": "search_fixture_rejected", "message": "Synthetic search rejected"}}
                     if failed else {"output": "Synthetic search result", "encrypted_output": "OPAQUE_SEARCH",
                                     "results": [{"ref_id": "turn0search0", "url": "https://example.test", "future_field": True}]})
            data = json.dumps(value).encode()
            self.send_response(400 if failed else 200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        if self.path == "/v1/responses":
            request = json.loads(raw)
            history = request["input"]
            assert "DESKTOP_IMAGE_ROUNDTRIP" in str(history)
            if any(item.get("type") == "function_call_output" and item.get("call_id") == "desktop_image_call" for item in history):
                item = {"type":"message","id":"msg_desktop_image","role":"assistant","content":[{"type":"output_text","text":"IMAGE_TEST_DONE"}],"status":"completed"}
            else:
                # The relay registers caller freeform tools as JSON callbacks and
                # escapes reserved namespaces. Select the advertised tool identity.
                declarations = list(request.get("tools", []))
                for entry in history:
                    if entry.get("type") == "additional_tools":
                        declarations.extend(entry["tools"])
                namespace, tool = next((group["name"], member) for group in declarations
                    if group.get("type") == "namespace" for member in group["tools"]
                    if member.get("name") == "exec" and member.get("type") == "function")
                assert tool["parameters"]["properties"]["input"]["type"] == "string"
                code = 'const result = await tools.image_gen__imagegen({prompt: "DESKTOP_IMAGE", transparent_background: true});\ngeneratedImage(result);'
                item = {"type":"function_call","id":"fc_desktop_image","namespace":namespace,"name":"exec","call_id":"desktop_image_call", "arguments":json.dumps({"input":code})}
            events = [{"type":"response.output_item.done","output_index":0,"item":item},
                      {"type":"response.completed","response":{"id":"resp_desktop_image","status":"completed","output":[item],"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}}]
            data = "".join("data: " + json.dumps(event) + "\n\n" for event in events).encode()
            self.send_response(200)
            self.send_header("Content-Type","text/event-stream")
            self.send_header("Content-Length",str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        assert self.path in ("/v1/images/generations", "/v1/images/edits")
        if self.headers["Content-Type"].startswith("multipart/form-data"):
            message = BytesParser(policy=default).parsebytes(
                b"Content-Type: " + self.headers["Content-Type"].encode() + b"\r\n\r\n" + raw)
            fields = {part.get_param("name", header="content-disposition"): part.get_payload(decode=True)
                      for part in message.iter_parts()}
            assert fields["image[]"] == PNG
            assert fields["mask"] == PNG
            assert fields["model"] == b"gpt-image-2"
            prompt = fields["prompt"].decode()
        else:
            body = json.loads(raw)
            assert body["model"] == "gpt-image-2"
            prompt = body["prompt"]
            if self.path.endswith("generations"):
                assert body["background"] == "transparent" and body["quality"] == "auto"
        seen.append({"path":self.path, "prompt":prompt})
        if prompt == "SLOW":
            slow_entered.set()
            slow_release.wait(40)
        if prompt == "FAIL":
            status, body = 400, {"error":{"code":"fixture_rejected", "message":"Synthetic image rejected", "type":"invalid_request_error"}}
        else:
            status, body = 200, {"created":1,"background":"transparent","data":[{"b64_json":base64.b64encode(PNG).decode()}],
                                 "usage":{"input_tokens":7,"output_tokens":11,"total_tokens":18}}
        data = json.dumps(body).encode()
        try:
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.send_header("x-codex-imagegen-request-id", "image-fixture-request")
            self.end_headers()
            self.wfile.write(data)
        except (BrokenPipeError, ConnectionError):
            pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--relay-binary", type=Path, default=ROOT / "target/debug/codex-relay.exe")
    parser.add_argument("--real-upstream", action="store_true")
    parser.add_argument("--output", type=Path, default=ROOT / ".cache/images-real-puppy.png")
    args = parser.parse_args()
    model = None
    if not args.real_upstream:
        model = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Images)
        threading.Thread(target=model.serve_forever, daemon=True).start()
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    with tempfile.TemporaryDirectory(prefix="bridge-images-test-") as temporary:
        root = Path(temporary)
        (root / ".cache").mkdir()
        (root / "web").mkdir()
        (root / "web/index.html").write_text("Isolated Images integration", encoding="utf-8")
        shutil.copyfile(ROOT / ".cache/launch-config.json", root / ".cache/launch-config.json")
        shutil.copyfile(args.relay_binary, root / "codex-relay.exe")
        env = {key:value for key,value in os.environ.items() if not key.upper().startswith(
            ("CODEX_", "OPENAI_", "CHATGPT_", "ANTHROPIC_", "AZURE_OPENAI_"))}
        env["NO_PROXY"] = "127.0.0.1,localhost"
        if model:
            (root / "home").mkdir()
            env["CODEX_HOME"] = str(root / "home")
            (root / "home/config.toml").write_text(f'''
model="gpt-6.1-sol"
model_provider="images_fixture"
[model_providers.images_fixture]
name="Images fixture"
base_url="http://127.0.0.1:{model.server_port}/v1"
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="SYNTHETIC_IMAGE_PROVIDER"
[model_providers.images_fixture.http_headers]
X-Image-Fixture="native-config-header"
''', encoding="utf-8")
        with (root / "service.log").open("wb") as log, httpx.Client(
                base_url=f"http://127.0.0.1:{port}", trust_env=False, timeout=300) as admin:
            process = subprocess.Popen([str(root / "codex-relay.exe"), "serve", "--port", str(port), "--web-dir", str(root / "web")],
                                       cwd=root, env=env, stdout=log, stderr=log,
                                       creationflags=getattr(subprocess,"CREATE_NO_WINDOW",0))
            try:
                for _ in range(100):
                    assert process.poll() is None, "Relay exited"
                    try:
                        if admin.get("/healthz").is_success:
                            break
                    except httpx.HTTPError:
                        pass
                    time.sleep(0.1)

                def post(path, body):
                    response = admin.post(path, json=body)
                    response.raise_for_status()
                    return response.json()

                account = post("/api/accounts", {"name":"Image fixture", "nativeConfig":True})
                revision = admin.get("/api/operations").json()["revision"]
                post(f"/api/accounts/{account['id']}/route", {"revision":revision, "enabled":True})
                key = post("/api/api-keys", {"name":"Image caller", "groupId":"00000000-0000-0000-0000-000000000000"})
                headers = {"Authorization":"Bearer " + key["secret"]}
                body = {"model":"gpt-image-2", "prompt":"A cute small golden puppy sitting on grass, soft natural light, no text.",
                        "background":"transparent", "quality":"auto", "size":"auto"}
                response = admin.post("/v1/images/generations", json=body, headers=headers)
                assert response.status_code == 200, (response.status_code, response.text[:1000])
                result = response.json()
                image = base64.b64decode(result["data"][0]["b64_json"], validate=True)
                assert image.startswith(b"\x89PNG") or image.startswith(b"\xff\xd8") or image.startswith(b"RIFF")
                if args.real_upstream:
                    args.output.parent.mkdir(parents=True, exist_ok=True)
                    args.output.write_bytes(image)
                    print(json.dumps({"status":"passed", "realImageRequests":1, "bytes":len(image),
                                      "sha256":hashlib.sha256(image).hexdigest(), "output":str(args.output)}), flush=True)
                else:
                    assert image == PNG
                    assert response.headers["x-codex-imagegen-request-id"] == "image-fixture-request"
                    generation_id = response.headers["x-request-id"]
                    failed = admin.post("/v1/images/generations", json={**body,"prompt":"FAIL"}, headers=headers)
                    assert failed.status_code == 400 and failed.json()["error"]["code"] == "fixture_rejected"
                    failure_id = failed.headers["x-request-id"]
                    content = admin.get(f"/api/requests/{failure_id}/content").json()
                    assert content["response"]["statusCode"] == 400
                    assert content["response"]["body"] == failed.json()
                    assert "SYNTHETIC_IMAGE_PROVIDER" not in json.dumps(content)
                    edited = admin.post("/v1/images/edits", data={"model":"gpt-image-2","prompt":"EDIT"},
                                        files=[("image[]",("puppy.png",PNG,"image/png")),("mask",("mask.png",PNG,"image/png"))], headers=headers)
                    assert edited.status_code == 200, edited.text[:1000]
                    assert base64.b64decode(edited.json()["data"][0]["b64_json"]) == PNG
                    edit_id = edited.headers["x-request-id"]
                    invalid = admin.post("/v1/images/generations",json={"prompt":"PRIVATE_PROMPT"},headers=headers)
                    assert invalid.status_code == 400
                    assert admin.get(f"/api/requests/{invalid.headers['x-request-id']}/content").status_code == 200
                    restricted = post("/api/api-keys", {"name":"No images", "models":["text-only"], "groupId":"00000000-0000-0000-0000-000000000000"})
                    denied = admin.post("/v1/images/generations",json=body,headers={"Authorization":"Bearer "+restricted["secret"]})
                    assert denied.status_code == 403
                    assert admin.get(f"/api/requests/{denied.headers['x-request-id']}/content").status_code == 200
                    stats = admin.get("/api/statistics").json()
                    records = {record["id"]:record for record in stats["records"]}
                    assert records[generation_id]["action"] == "images.generations"
                    assert records[generation_id]["inputTokens"] == 7 and records[generation_id]["outputTokens"] == 11
                    assert records[edit_id]["action"] == "images.edits"
                    assert records[failure_id]["status"] == "failed"
                    search_body = {"id": "native-search-fixture", "model": "gpt-6.1-sol", "future_field": {"kept": True},
                                   "commands": {"search_query": [{"q": "SYNTHETIC_PRIVATE_SEARCH"}]}}
                    for commands in [search_body["commands"], {"open": [{"ref_id": "turn0search0"}]}]:
                        searched = admin.post("/v1/alpha/search", json={**search_body, "commands": commands}, headers=headers)
                        assert searched.status_code == 200, (searched.status_code, searched.text[:1000])
                        assert searched.json()["encrypted_output"] == "OPAQUE_SEARCH"
                        assert searched.json()["results"][0]["ref_id"] == "turn0search0"
                    failed_search = admin.post("/v1/alpha/search", json={**search_body, "commands": {"test_failure": True}}, headers=headers)
                    assert failed_search.status_code == 400
                    search_content = admin.get(f"/api/requests/{failed_search.headers['x-request-id']}/content").json()
                    assert search_content["response"]["statusCode"] == 400
                    assert "SYNTHETIC_PRIVATE_SEARCH" not in json.dumps(search_content)
                    print("PASS: native-auth standalone search, reference continuation and failure journal", flush=True)
                    installation = json.loads((ROOT / ".cache/launch-config.json").read_text(encoding="utf-8-sig"))
                    native_image_turn(root, installation, str(admin.base_url).rstrip('/') + "/v1", key["secret"], PNG)
                    assert any(request["prompt"] == "DESKTOP_IMAGE" for request in seen)
                    print("Official desktop app-server image_gen -> relay Images API -> provider -> saved image passed", flush=True)
                    with ThreadPoolExecutor(max_workers=1) as executor:
                        pending = executor.submit(admin.post, "/v1/images/generations", json={**body,"prompt":"SLOW"}, headers=headers)
                        assert slow_entered.wait(30), "Slow image request did not reach upstream"
                        post(f"/api/api-keys/{key['key']['id']}", {"revision":key['key']['revision'], "state":"revoked"})
                        cancelled = pending.result(timeout=15)
                        assert cancelled.status_code != 200
                    slow_release.set()
                    assert admin.get("/api/operations").json()["active"] == 0
                    print(f"PASS: {len(seen)} standalone tool requests; real native config/auth, search, JSON generation, multipart editing, upstream failures, records, usage, key policy and revocation", flush=True)
            finally:
                slow_release.set()
                try:
                    admin.post("/api/service/stop",json={})
                    process.wait(timeout=40)
                except (httpx.HTTPError,subprocess.TimeoutExpired):
                    process.terminate()
                    process.wait(timeout=10)
    if model:
        model.shutdown()


if __name__ == "__main__":
    main()
