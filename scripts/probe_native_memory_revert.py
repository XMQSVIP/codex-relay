"""Check same-ID edited-history recovery using installed official memory storage.

Uses only synthetic credentials, localhost model responses and a temporary home.
No production configuration or account is read or modified.
"""
import http.server
import json
import tempfile
import threading
import time
from pathlib import Path

from probe_native_output_limit import INSTALL, Model, Native, ROOT, SEEN
from probe_native_context_management import finish_turn

OLD = "SYNTHETIC_REVERT_OLD_82AC"
EDITED = "SYNTHETIC_REVERT_EDITED_82AC"


def inspect(response):
    if "error" in response:
        return {"error": response["error"]}
    return {"thread": response.get("result", {}).get("thread", {}).get("id"), "ok": True}


def scan(root):
    found, unreadable = [], []
    for path in root.rglob("*"):
        if not path.is_file():
            continue
        try:
            if any(marker.encode() in path.read_bytes() for marker in (OLD, EDITED)):
                found.append(str(path.relative_to(root)))
        except OSError:
            unreadable.append(str(path.relative_to(root)))
    return {"matches": found, "unreadable": unreadable}


def main():
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Model)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    report = {"nativeVersion": INSTALL["appServerVersion"], "realModelRequests": 0}
    try:
        with tempfile.TemporaryDirectory(prefix="memory-revert-", dir=ROOT / ".cache") as temporary:
            root = Path(temporary)
            native = Native(root, server.server_port, config_overrides=[
                'experimental_thread_store={type="in_memory",id="memory-revert-audit"}'])
            try:
                assert "result" in native.call("initialize", {"clientInfo": INSTALL["nativeClientInfo"],
                    "capabilities": {"experimentalApi": True}})
                native.send({"method": "initialized"})
                options = {"model": "gpt-6.1-sol", "modelProvider": "synthetic", "cwd": str(native.cwd),
                    "ephemeral": False, "historyMode": "legacy", "approvalPolicy": "untrusted",
                    "sandbox": "read-only", "dynamicTools": [], "environments": [],
                    "runtimeWorkspaceRoots": [], "selectedCapabilityRoots": [],
                    "baseInstructions": "", "developerInstructions": "",
                    "config": {"thread_unload_delay_secs": 0}}
                report["paginatedStart"] = inspect(native.call("thread/start", {**options, "historyMode": "paginated"}))
                started = native.call("thread/start", options)
                thread = started["result"]["thread"]["id"]
                report["originalThread"] = thread
                turns = []
                for index in range(2):
                    started = native.call("turn/start", {"threadId": thread,
                        "input": [{"type": "text", "text": OLD + str(index), "text_elements": []}]})
                    turns.append(started["result"]["turn"]["id"])
                    finish_turn(native, thread)
                report["legacyRevert"] = inspect(native.call("thread/revert", {"threadId": thread, "beforeTurnId": turns[1]}))
                replacement = [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": EDITED}]}]
                report["liveHistoryReplacement"] = inspect(native.call("thread/resume", {"threadId": thread, "history": replacement}))
                report["unsubscribe"] = native.call("thread/unsubscribe", {"threadId": thread})
                for _ in range(60):
                    loaded = native.call("thread/loaded/list", {})
                    if thread not in loaded["result"]["data"]:
                        break
                    time.sleep(.1)
                report["unloaded"] = thread not in loaded["result"]["data"]
                restored = native.call("thread/resume", {"threadId": thread, "history": replacement,
                    "cwd": str(native.cwd), "model": "gpt-6.1-sol", "modelProvider": "synthetic"})
                report["resumeEditedHistory"] = inspect(restored)
                if "result" in restored:
                    restored_id = restored["result"]["thread"]["id"]
                    report["sameThreadId"] = restored_id == thread
                    assert "result" in native.call("turn/start", {"threadId": restored_id,
                        "input": [{"type": "text", "text": "Continue synthetic edit", "text_elements": []}]})
                    finish_turn(native, restored_id)
                    report["editedHistoryApplied"] = EDITED in json.dumps(SEEN[-1]["input"]) and OLD not in json.dumps(SEEN[-1]["input"])
                    report["samePromptCacheKey"] = SEEN[0].get("prompt_cache_key") == SEEN[-1].get("prompt_cache_key")
                report["filesWhileAlive"] = scan(root)
            finally:
                native.close()
            report["filesAfterExit"] = scan(root)
    finally:
        server.shutdown()
        server.server_close()
    report["syntheticModelRequests"] = len(SEEN)
    path = ROOT / ".cache/native-memory-revert.json"
    path.write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(json.dumps(report, ensure_ascii=False))


if __name__ == "__main__":
    main()
