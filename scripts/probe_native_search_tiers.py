"""Verify native search settings and tier propagation using localhost only.

The synthetic model catalog advertises Ultrafast to exercise wire behavior;
this does not verify real account access, search execution or speed.
"""
import base64
from datetime import datetime, timezone
import time
import http.server
import json
from pathlib import Path
import subprocess
import tempfile
import threading

from probe_native_output_limit import INSTALL, Native, ROOT, SEEN
from probe_native_context_management import finish_turn
from native_tools_client import Native as ToolNative


class Harness(ToolNative):
    def call(self, method, params):
        return {"result": self.rpc(method, params)}

    def receive(self, timeout=30):
        return self.next(timeout)


class Model(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
        if self.path in ("/events", "/codex/events", "/codex/analytics"):
            value, content_type = {}, "application/json"
        else:
            assert self.headers.get("Authorization") == "Bearer SYNTHETIC_LIMIT_PROBE"
            if self.path == "/v1/alpha/search":
                self.server.searches.append(body)
                value, content_type = {"output":"SYNTHETIC_SEARCH_RESULT","encrypted_output":"SYNTHETIC_OPAQUE","results":[]}, "application/json"
            else:
                assert self.path == "/v1/responses", self.path
                SEEN.append(body)
                assert len(SEEN) <= 4, "Unexpected model replay"
                if any(i["type"] == "function_call_output" for i in body["input"]):
                    item = {"type":"message","id":"m_fixture","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Synthetic result"}]}
                else:
                    item = {"type":"function_call","id":"fc_fixture","namespace":"web","name":"run","call_id":"call_fixture","arguments":json.dumps({"search_query":[{"q":"synthetic fixture"}]})}
                events = [{"type":"response.output_item.done","output_index":0,"item":item},{"type":"response.completed","response":{"id":"r_fixture","status":"completed","output":[item],"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}}]
                value = "".join("data: " + json.dumps(event) + "\n\n" for event in events).encode()
                content_type = "text/event-stream"
        data = value if isinstance(value,bytes) else json.dumps(value).encode()
        self.send_response(200)
        self.send_header("Content-Type",content_type)
        self.send_header("Content-Length",str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def main():
    report = {"nativeVersion": INSTALL["appServerVersion"], "realModelRequests": 0,
              "syntheticServiceTierCatalog": True, "cases": []}
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Model)
    server.searches = []
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix="search-tiers-", dir=ROOT / ".cache") as directory:
            root = Path(directory)
            export = root / "export"
            export.mkdir()
            native = Native(export, server.server_port)
            try:
                catalog = json.loads(subprocess.check_output(
                    [INSTALL["appServerExecutable"], "debug", "models"],
                    env=native.env, cwd=native.cwd, timeout=20,
                    creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0)))
            finally:
                native.close()
            for model in catalog["models"]:
                model.update(tool_mode="direct", multi_agent_version="disabled",
                             apply_patch_tool_type=None, experimental_supported_tools=[])
                if model["slug"] == "gpt-6.1-sol":
                    model["service_tiers"].append({"id": "ultrafast", "name": "Ultrafast",
                                                   "description": "Synthetic fixture only"})
            catalog_path = root / "catalog.json"
            catalog_path.write_text(json.dumps(catalog), encoding="utf-8")
            run = root / "run"
            run.mkdir()
            home = run / "home"
            workspace = run / "workspace"
            home.mkdir()
            workspace.mkdir()
            claims = {"email": "fixture@example.test", "sub": "fixture", "exp": int(time.time()) + 3600,
                      "https://api.openai.com/auth": {"chatgpt_account_id": "fixture", "chatgpt_user_id": "fixture", "chatgpt_plan_type": "pro"}}
            token = "e30." + base64.urlsafe_b64encode(json.dumps(claims).encode()).decode().rstrip("=") + ".synthetic"
            (home / "auth.json").write_text(json.dumps({"auth_mode": "chatgpt", "OPENAI_API_KEY": None,
                "tokens": {"access_token": token, "id_token": token, "refresh_token": "synthetic", "account_id": "fixture"},
                "last_refresh": datetime.now(timezone.utc).isoformat()}), encoding="utf-8")
            (home / "config.toml").write_text(f'''model="gpt-6.1-sol"
model_provider="synthetic"
model_catalog_json={json.dumps(str(catalog_path))}
cli_auth_credentials_store="file"
chatgpt_base_url="http://127.0.0.1:{server.server_port}"
[features]
remote_models=false
secret_auth_storage=false
enable_request_compression=false
[model_providers.synthetic]
name="OpenAI"
base_url="http://127.0.0.1:{server.server_port}/v1"
wire_api="responses"
requires_openai_auth=true
supports_websockets=false
supports_standalone_web_search=true
request_max_retries=0
stream_max_retries=0
experimental_bearer_token="SYNTHETIC_LIMIT_PROBE"
''', encoding="utf-8")
            log = (run / "native.log").open("wb")
            native = Harness(INSTALL, home, workspace, log, isolated=True, timeout=45)
            native.cwd = workspace
            try:
                assert "result" in native.call("initialize", {"clientInfo": INSTALL["nativeClientInfo"],
                    "capabilities": {"experimentalApi": True}})
                native.send({"method": "initialized"})
                models = native.call("model/list", {"includeHidden": True, "limit": 100})["result"]["data"]
                assert any(m["model"] == "gpt-6.1-sol" and any(t["id"] == "ultrafast" for t in m.get("serviceTiers", [])) for m in models)
                for restricted in [True, False]:
                    config = {"features.standalone_web_search": True, "web_search": "cached" if restricted else "live",
                              "tools.web_search.context_size": "high" if restricted else "medium",
                              "tools.web_search.allowed_domains": ["example.com"] if restricted else [],
                              "tools.web_search.location": {"country": "US", "city": "Boston"} if restricted else {},
                              "model_providers.synthetic.capabilities.external_web_access": True}
                    started = native.call("thread/start", {"model": "gpt-6.1-sol", "modelProvider": "synthetic",
                        "cwd": str(native.cwd), "ephemeral": True, "approvalPolicy": "untrusted", "sandbox": "read-only",
                        "dynamicTools": [], "environments": [], "runtimeWorkspaceRoots": [], "selectedCapabilityRoots": [],
                        "baseInstructions": "", "developerInstructions": "", "experimentalRawEvents": True, "config": config})
                    assert "result" in started, started
                    thread = started["result"]["thread"]["id"]
                    turn = native.call("turn/start", {"threadId": thread, "serviceTierForTurn": "ultrafast",
                        "input": [{"type": "text", "text": "SYNTHETIC_SEARCH_SETTINGS", "text_elements": []}]})
                    assert "result" in turn, turn
                    finish_turn(native, thread)
                    body = SEEN[-1]
                    assert len(server.searches) == len(report["cases"]) + 1, SEEN[-1]["input"][-1]
                    search = server.searches[-1]["settings"]
                    assert body.get("service_tier") == "ultrafast", body.get("service_tier")
                    assert search.get("external_web_access") == (not restricted), search
                    assert search.get("search_context_size") == ("high" if restricted else "medium"), search
                    if restricted:
                        assert search["filters"]["allowed_domains"] == ["example.com"], search
                        assert search["user_location"]["city"] == "Boston", search
                    else:
                        assert not search.get("filters", {}).get("allowed_domains"), search
                        assert search.get("user_location") in (None, {"type": "approximate"}), search
                    report["cases"].append({"restricted": restricted, "serviceTier": body["service_tier"], "search": search})
            finally:
                native.close()
                log.close()
    finally:
        server.shutdown()
        server.server_close()
    report["syntheticModelRequests"] = len(SEEN)
    (ROOT / ".cache/native-search-tiers.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
