"""Inspect live-turn settings with an installed app-server and synthetic tools."""
import http.server
import json
import tempfile
import threading
from pathlib import Path

from probe_native_output_limit import INSTALL, Native, ROOT, SEEN
from probe_native_context_management import finish_turn


class Model(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        assert self.path == "/v1/responses"
        assert self.headers.get("Authorization") == "Bearer SYNTHETIC_LIMIT_PROBE"
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        SEEN.append(body)
        item = {"type": "function_call", "id": "fc_synthetic", "call_id": "call_synthetic",
                "name": "synthetic_tool", "arguments": "{}", "status": "completed"}
        if any(i["type"] == "function_call_output" for i in body["input"]):
            item = {"type": "message", "id": "m_synthetic", "role": "assistant", "status": "completed",
                    "content": [{"type": "output_text", "text": "Synthetic result", "annotations": []}]}
        events = [{"type": "response.output_item.done", "output_index": 0, "item": item},
                  {"type": "response.completed", "response": {"id": "r_synthetic", "status": "completed",
                   "output": [item], "usage": {"input_tokens": 10, "output_tokens": 2, "total_tokens": 12}}}]
        data = "".join(f"data: {json.dumps(e)}\n\n" for e in events).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def main():
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Model)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    report = {"nativeVersion": INSTALL["appServerVersion"], "realModelRequests": 0, "updates": []}
    try:
        with tempfile.TemporaryDirectory(prefix="live-settings-", dir=ROOT / ".cache") as directory:
            native = Native(Path(directory), server.server_port)
            try:
                assert "result" in native.call("initialize", {"clientInfo": INSTALL["nativeClientInfo"],
                    "capabilities": {"experimentalApi": True}})
                native.send({"method": "initialized"})
                started = native.call("thread/start", {"model": "gpt-6.1-sol", "modelProvider": "synthetic",
                    "cwd": str(native.cwd), "ephemeral": True, "approvalPolicy": "untrusted", "sandbox": "read-only",
                    "dynamicTools": [{"name": "synthetic_tool", "description": "Synthetic", "inputSchema": {"type": "object"}}],
                    "environments": [], "runtimeWorkspaceRoots": [], "selectedCapabilityRoots": [],
                    "baseInstructions": "", "developerInstructions": "", "experimentalRawEvents": True,
                    "config": {"features.step_model_switching": True,
                               "model_providers.synthetic.capabilities.reasoning_summaries": True}})
                thread = started["result"]["thread"]["id"]
                turn = native.call("turn/start", {"threadId": thread, "input": [
                    {"type": "text", "text": "SYNTHETIC_TOOL_REQUEST", "text_elements": []}]})["result"]["turn"]["id"]
                while True:
                    event = native.messages.get(timeout=30)
                    if event.get("method") == "item/tool/call":
                        callback = event
                        break
                for fields in [{"effort": "high"}, {"summary": "detailed"}, {"serviceTier": "priority"}]:
                    response = native.call("turn/settings/update", {"threadId": thread, "turnId": turn, **fields})
                    report["updates"].append({"fields": fields, "response": response})
                    assert response.get("result") == {"status": "applied"}, response
                native.send({"id": callback["id"], "result": {"contentItems": [{"type": "inputText", "text": "SYNTHETIC_RESULT"}], "success": True}})
                finish_turn(native, thread)
                report["resultingReasoning"] = SEEN[-1].get("reasoning")
                report["resultingServiceTier"] = SEEN[-1].get("service_tier")
                assert report["resultingReasoning"]["effort"] == "high"
                assert report["resultingReasoning"]["summary"] == "detailed"
                assert report["resultingServiceTier"] == "priority"
            finally:
                native.close()
    finally:
        server.shutdown()
        server.server_close()
    report["syntheticModelRequests"] = len(SEEN)
    (ROOT / ".cache/native-live-settings.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
