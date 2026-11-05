"""Audit installed native history APIs against isolated synthetic histories only."""
import http.server
import json
import tempfile
import threading
from pathlib import Path

from probe_native_output_limit import INSTALL, Model, Native, ROOT, SEEN
from probe_native_context_management import finish_turn


def summary(response):
    if "error" in response:
        return {"supported": False, "error": {"code": response["error"]["code"],
                "message": response["error"]["message"][:512]}}
    thread = response.get("result", {}).get("thread", {})
    return {"supported": True, "thread": thread.get("id"), "ephemeral": thread.get("ephemeral")}


def main():
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Model)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    report = {"nativeVersion": INSTALL["appServerVersion"], "realModelRequests": 0, "cases": []}
    try:
        with tempfile.TemporaryDirectory(prefix="history-capabilities-", dir=ROOT / ".cache") as directory:
            for ephemeral, mode in [(True, None), (True, "legacy"), (False, None)]:
                root = Path(directory) / (("ephemeral" if ephemeral else "persisted") + (mode or "default"))
                root.mkdir()
                native = Native(root, server.server_port)
                try:
                    initialized = native.call("initialize", {"clientInfo": INSTALL["nativeClientInfo"],
                        "capabilities": {"experimentalApi": True}})
                    assert "result" in initialized, initialized
                    native.send({"method": "initialized"})
                    started = native.call("thread/start", {"model": "gpt-6.1-sol", "modelProvider": "synthetic",
                        "cwd": str(native.cwd), "ephemeral": ephemeral, "approvalPolicy": "untrusted",
                        "sandbox": "read-only", "dynamicTools": [], "environments": [],
                        "runtimeWorkspaceRoots": [], "selectedCapabilityRoots": [],
                        "baseInstructions": "", "developerInstructions": "", "experimentalRawEvents": True,
                        **({"historyMode": mode} if mode else {})})
                    assert "result" in started, started
                    thread = started["result"]["thread"]["id"]
                    turns = []
                    for index in range(2):
                        turn = native.call("turn/start", {"threadId": thread, "input": [
                            {"type": "text", "text": f"SYNTHETIC_HISTORY_{index}", "text_elements": []}]})
                        assert "result" in turn, turn
                        turns.append(turn["result"]["turn"]["id"])
                        finish_turn(native, thread)
                    case = {"ephemeralSource": ephemeral, "historyMode": mode or "default"}
                    for label, extra in [("fork", {}), ("forkAtTurn", {"lastTurnId": turns[0]}),
                                         ("forkBeforeTurn", {"beforeTurnId": turns[1]})]:
                        fork = native.call("thread/fork", {"threadId": thread, "ephemeral": True,
                            "excludeTurns": True, **extra})
                        case[label] = summary(fork)
                        if "result" in fork:
                            child = fork["result"]["thread"]["id"]
                            turn = native.call("turn/start", {"threadId": child, "input": [
                                {"type": "text", "text": "SYNTHETIC_BRANCH", "text_elements": []}]})
                            assert "result" in turn, turn
                            finish_turn(native, child)
                            case[label]["containsFirstTurn"] = "SYNTHETIC_HISTORY_0" in json.dumps(SEEN[-1]["input"])
                            case[label]["containsSecondTurn"] = "SYNTHETIC_HISTORY_1" in json.dumps(SEEN[-1]["input"])
                    case["replaceLiveHistory"] = summary(native.call("thread/resume", {"threadId": thread,
                        "history": [{"type": "message", "role": "user", "content": [
                            {"type": "input_text", "text": "SYNTHETIC_REPLACEMENT"}]}]}))
                    case["revert"] = summary(native.call("thread/revert", {"threadId": thread, "beforeTurnId": turns[1]}))
                    compact = native.call("thread/compact/start", {"threadId": thread})
                    case["memoryCompaction"] = summary(compact)
                    if "result" in compact:
                        events = finish_turn(native, thread)
                        case["memoryCompaction"]["rawItemTypes"] = [event["params"]["item"]["type"] for event in events
                            if event.get("method") == "rawResponseItem/completed"]
                    case["readHistory"] = summary(native.call("thread/read", {"threadId": thread, "includeTurns": True}))
                    case["listItems"] = summary(native.call("thread/items/list", {"threadId": thread}))
                    report["cases"].append(case)
                    print(json.dumps(case), flush=True)
                finally:
                    native.close()
    finally:
        server.shutdown()
        server.server_close()
    report["syntheticModelRequests"] = len(SEEN)
    path = ROOT / ".cache/native-history-capabilities.json"
    path.write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(json.dumps({"report": str(path), "syntheticModelRequests": len(SEEN)}))


if __name__ == "__main__":
    main()
