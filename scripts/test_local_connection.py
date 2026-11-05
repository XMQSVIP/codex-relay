"""Opt-in installed-native HTTP acceptance. Uses temporary accounts; no inference.

Build the release binary and web UI, prepare the local Codex installation, then
run: python scripts/test_local_connection.py
"""
import base64
import concurrent.futures
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parent.parent
BINARY = ROOT / "target/release" / ("codex-relay.exe" if os.name == "nt" else "codex-relay")
DEFAULT_GROUP = "00000000-0000-0000-0000-000000000000"


def main():
    assert BINARY.is_file(), "Build the release binary first."
    assert (ROOT / "web/dist/index.html").is_file(), "Build the web UI first."
    installation = ROOT / ".cache/launch-config.json"
    assert installation.is_file(), "Prepare the local native installation first."
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    base = f"http://127.0.0.1:{port}"

    def request(path, body=None, expected=200):
        # Each concurrent request owns its opener; never use system HTTP proxies.
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        req = urllib.request.Request(
            base + path,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
        )
        try:
            response = opener.open(req, timeout=120)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            value = json.load(response)
            assert response.code == expected, (path, response.code, value.get("error"))
            return value

    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="local-connection-test-", dir=ROOT / ".cache") as directory:
        work = Path(directory)
        (work / ".cache").mkdir()
        shutil.copyfile(installation, work / ".cache/launch-config.json")
        process = subprocess.Popen(
            [str(BINARY), "serve", "--port", str(port), "--web-dir", str(ROOT / "web/dist")],
            cwd=work, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
        )
        try:
            for _ in range(100):
                assert process.poll() is None, "Test server exited during startup."
                try:
                    request("/healthz")
                    break
                except (OSError, urllib.error.URLError):
                    time.sleep(0.1)
            else:
                raise AssertionError("Test server did not start.")

            def overview():
                return request("/api/overview")

            def connection(preview, **fields):
                return {
                    "requestId": str(uuid.uuid4()), "inspectionId": preview["id"],
                    "name": "native-acceptance", "groupId": DEFAULT_GROUP,
                    "revision": overview()["operations"]["revision"], **fields,
                }

            preview = request("/api/local-connection/inspect", {})
            assert preview["inferenceDispatched"] is False
            assert not overview()["accounts"], "Preview must not create accounts."
            assert set(preview) == {"id", "provider", "auth", "model", "endpoint", "inferenceDispatched"}
            body = connection(preview)
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                results = list(pool.map(lambda _: request("/api/local-connection/connect", body), range(2)))
            assert all(result["status"] == "enabled" and not result.get("error") for result in results)
            assert results[0]["account"]["id"] == results[1]["account"]["id"]
            assert len(overview()["accounts"]) == 1, "Retries created duplicate accounts."

            empty = request("/api/accounts", {"name": "empty-original"}, 201)
            converted = request("/api/local-connection/connect", connection(
                preview, accountId=empty["id"], name="must-not-rename",
            ))
            assert converted["status"] == "enabled"
            assert converted["account"]["id"] == empty["id"]
            assert converted["account"]["name"] == "empty-original"
            assert converted["account"]["source"] == "native-config"

            protected = request("/api/accounts", {"name": "protected-synthetic"}, 201)
            claims = {"exp": int(time.time()) + 3600, "sub": "synthetic-user",
                      "https://api.openai.com/auth": {"chatgpt_account_id": "synthetic-account"}}
            payload = base64.urlsafe_b64encode(json.dumps(claims).encode()).decode().rstrip("=")
            token = "e30." + payload + ".synthetic-signature"
            request(f"/api/accounts/{protected['id']}/credentials", {"document": {"accessToken": token}})
            rejected = request("/api/local-connection/connect", connection(
                preview, accountId=protected["id"],
            ), 409)
            assert rejected["error"]["code"] == "LOCAL_ACCOUNT_NOT_EMPTY"
            protected_now = next(a for a in overview()["accounts"] if a["id"] == protected["id"])
            assert protected_now.get("source", "isolated") == "isolated"
            assert protected_now["credentialSource"] == "external"
            assert token not in json.dumps(overview())
            assert request("/api/statistics?hours=24")["summary"]["inferenceRequests"] == 0
            print(f"PASS: native preview, concurrent retries, empty conversion, credential protection; no inference ({time.monotonic() - started:.1f}s).")
        finally:
            if process.poll() is None:
                try:
                    request("/api/service/stop", {}, 202)
                    process.wait(timeout=15)
                except (OSError, urllib.error.URLError, subprocess.TimeoutExpired):
                    process.kill()
                    process.wait(timeout=5)


if __name__ == "__main__":
    main()
