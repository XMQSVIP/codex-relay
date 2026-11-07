"""Opt-in real-provider custom-tool roundtrip using an isolated relay instance.

Uses the current user's native provider configuration and makes two model requests.
Does not execute model code or alter the running relay, its accounts, or settings.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time
import uuid

import httpx

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--real-upstream", action="store_true", required=True)
    parser.add_argument("--relay-binary", type=Path, default=ROOT / "target/release/codex-relay.exe")
    parser.add_argument("--model", default="gpt-6.1-sol")
    args = parser.parse_args()
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    with tempfile.TemporaryDirectory(prefix="bridge-real-tool-check-") as temporary:
        root = Path(temporary)
        (root / ".cache").mkdir()
        (root / "web").mkdir()
        (root / "web/index.html").write_text("Isolated tool regression", encoding="utf-8")
        shutil.copyfile(ROOT / ".cache/launch-config.json", root / ".cache/launch-config.json")
        binary = root / "codex-relay.exe"
        shutil.copyfile(args.relay_binary, binary)
        env = {key: value for key, value in os.environ.items()
               if not key.upper().startswith(("CODEX_", "OPENAI_", "CHATGPT_", "ANTHROPIC_", "AZURE_OPENAI_"))}
        with (root / "service.log").open("wb") as log, httpx.Client(
                base_url=f"http://127.0.0.1:{port}", trust_env=False, timeout=180) as admin:
            process = subprocess.Popen([str(binary), "serve", "--port", str(port), "--web-dir", str(root / "web")],
                                       cwd=root, env=env, stdout=log, stderr=log,
                                       creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
            try:
                for _ in range(100):
                    if process.poll() is not None:
                        raise RuntimeError("Isolated relay exited during startup")
                    try:
                        if admin.get("/healthz").is_success:
                            break
                    except httpx.HTTPError:
                        pass
                    time.sleep(0.1)
                else:
                    raise RuntimeError("Isolated relay did not become ready")

                def post(path, body, **options):
                    result = admin.post(path, json=body, **options)
                    result.raise_for_status()
                    return result.json()

                account = post("/api/accounts", {"name":"Real tool regression", "nativeConfig":True})
                revision = admin.get("/api/operations").json()["revision"]
                post(f"/api/accounts/{account['id']}/route", {"revision":revision, "enabled":True})
                key = post("/api/api-keys", {"name":"Isolated diagnostic", "groupId":"00000000-0000-0000-0000-000000000000"})["secret"]
                headers = {"Authorization": f"Bearer {key}"}
                marker = "BRIDGE_TOOL_ID_" + uuid.uuid4().hex
                code = f'text("{marker}");'
                tools = [{"type":"namespace", "name":"functions", "description":"Client diagnostic tools", "tools":[
                    {"type":"custom", "name":"exec", "description":f"Client echo test. Call exactly once with this exact input: {code}",
                     "format":{"type":"text"}}]}]
                prompt = f"Call functions.exec exactly once with input {code} After receiving the tool result, respond with exactly that result and do not call any more tools."
                request = {"model":args.model, "tools":tools, "reasoning":{"effort":"low"}, "store":False}
                first = post("/v1/responses", {**request, "input":prompt}, headers=headers)
                if first.get("status") != "completed":
                    raise RuntimeError(f"First real request failed: {first.get('error')}")
                calls = [item for item in first["output"] if item["type"] in ("custom_tool_call", "function_call")]
                if len(calls) != 1 or calls[0]["type"] != "custom_tool_call":
                    raise RuntimeError("Model did not return exactly one custom tool call")
                call = calls[0]
                if (call.get("namespace"), call["name"], call["input"].strip()) != ("functions", "exec", code):
                    raise RuntimeError("Model did not follow the bounded echo fixture; no code was executed")
                # Interpret only the exact echo fixture above; never execute arbitrary model code.
                result = {"type":"custom_tool_call_output", "id":"ctco_" + uuid.uuid4().hex,
                          "call_id":call["call_id"], "output":marker}
                continued = post("/v1/responses", {**request, "input":[
                    {"role":"user", "content":prompt}, *first["output"], result]}, headers=headers)
                if continued.get("status") != "completed":
                    raise RuntimeError(f"Real tool-result continuation failed: {continued.get('error')}")
                answer = "".join(part.get("text", "") for item in continued["output"]
                                 if item["type"] == "message" for part in item.get("content", []))
                if answer.strip() != marker:
                    raise RuntimeError("Real provider did not return the expected tool result")
                print(json.dumps({"status":"passed", "realModelRequests":2, "model":args.model,
                                  "verified":"functions.exec custom call and ctco result continuation",
                                  "modifiedRunningRelay":False}), flush=True)
            finally:
                try:
                    admin.post("/api/service/stop", json={})
                    process.wait(timeout=40)
                except (httpx.HTTPError, subprocess.TimeoutExpired):
                    process.terminate()
                    process.wait(timeout=10)


if __name__ == "__main__":
    main()
