"""Probe official RPC alternatives for exporting remote compaction without disk.

All inputs and ciphertext are synthetic and the only upstream is localhost.
The file scan checks whether the isolated app-server persisted either canary.
"""
import http.server
import argparse
import json
import tempfile
import threading
from pathlib import Path

from probe_native_context_management import Model
from probe_native_output_limit import INSTALL, Native, ROOT

CANARY = "SYNTHETIC_NO_DISK_COMPACTION_CANARY_12876"
CIPHER = "SYNTHETIC_CONTEXT_COMPACTION"


def contains_cipher(value):
    return CIPHER in json.dumps(value)


def inspect_response(response):
    if "error" in response:
        return {"error":str(response["error"])[:512]}
    return {"ok":True,"cipherExposed":contains_cipher(response)}


def canary_files(root):
    found, unreadable = [], []
    for path in root.rglob("*"):
        if not path.is_file():
            continue
        try:
            if path.stat().st_size >= 64 * 1024 * 1024:
                unreadable.append(str(path.relative_to(root)))
                continue
            data = path.read_bytes()
            if any(marker.encode() in data for marker in [CANARY,CIPHER]):
                found.append(str(path.relative_to(root)))
        except OSError:
            unreadable.append(str(path.relative_to(root)))
    return {"matches":found,"unreadable":unreadable}


def run_case(root, server, label, memory=False, inject=False, paginated=False,
             raw=True, remote="v2", old_flag=False):
    root.mkdir()
    overrides = [
        'experimental_thread_store={type="in_memory",id="synthetic-compaction-audit"}'
    ] if memory else []
    if old_flag:
        overrides.append('features.remote_compaction_v2=true')
    native = Native(root, server.server_port, config_overrides=overrides)
    case = {"case":label,"remoteCompaction":remote,"rawEvents":raw,"deprecatedFlag":old_flag}
    try:
        init = native.call("initialize", {"clientInfo":INSTALL["nativeClientInfo"],
            "capabilities":{"experimentalApi":True}})
        assert "result" in init, init
        native.send({"method":"initialized"})
        started = native.call("thread/start", {"model":"gpt-6.1-sol","modelProvider":"synthetic",
            "cwd":str(native.cwd),"ephemeral":not memory,"historyMode":"paginated" if paginated else "legacy",
            "approvalPolicy":"untrusted","sandbox":"read-only","dynamicTools":[],"environments":[],
            "runtimeWorkspaceRoots":[],"selectedCapabilityRoots":[],"baseInstructions":"","developerInstructions":"",
            "experimentalRawEvents":raw,"config":{"model_providers.synthetic.capabilities.remote_compaction":remote}})
        case["start"] = inspect_response(started)
        if "result" not in started:
            return case
        thread = started["result"]["thread"]["id"]
        items = [{"type":"message","role":"user","content":[{"type":"input_text","text":CANARY}]}]
        if inject:
            items.append({"type":"compaction_trigger"})
        case["inject"] = inspect_response(native.call("thread/inject_items", {"threadId":thread,"items":items}))
        before = len(server.seen)
        started = native.call("turn/start", {"threadId":thread,"input":[]}) if inject else native.call("thread/compact/start", {"threadId":thread})
        case["execute"] = inspect_response(started)
        events = []
        if "result" in started:
            while True:
                event = native.receive(timeout=35)
                events.append(event)
                if event.get("method")=="turn/completed":
                    case["turnStatus"] = event["params"]["turn"]["status"]
                    turn = event["params"]["turn"]["id"]
                    break
            case["allEventsExposeCipher"] = contains_cipher(events)
            case["eventMethods"] = sorted({event.get("method", "") for event in events})
            case["modelRequests"] = server.seen[before:]
            for method, params in [
                ("thread/read", {"threadId":thread,"includeTurns":True}),
                ("thread/turns/list", {"threadId":thread}),
                ("thread/items/list", {"threadId":thread}),
                ("thread/resume", {"threadId":thread,"excludeTurns":False}),
            ]:
                case[method] = inspect_response(native.call(method,params))
            compactions = [event["params"]["item"]["id"] for event in events
                if event.get("method")=="item/completed" and event["params"]["item"]["type"]=="contextCompaction"]
            if compactions:
                case["thread/items/read"] = inspect_response(native.call("thread/items/read", {
                    "threadId":thread,"turnId":turn,"itemIds":compactions}))
            if memory:
                case["memoryFork"] = inspect_response(native.call("thread/fork", {"threadId":thread,"ephemeral":True,"excludeTurns":True}))
    except Exception as error:
        case["probeError"] = str(error)[:512]
    finally:
        try:
            case["canaryFilesWhileRunning"] = canary_files(root)
        finally:
            native.close()
        case["canaryFiles"] = canary_files(root)
    return case


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--flags-only", action="store_true", help="compare remote capability and raw event switches")
    args = parser.parse_args()
    server = http.server.ThreadingHTTPServer(("127.0.0.1",0),Model)
    server.seen = []
    threading.Thread(target=server.serve_forever,daemon=True).start()
    report = {"nativeVersion":INSTALL["appServerVersion"],"realModelRequests":0,"cases":[]}
    try:
        with tempfile.TemporaryDirectory(prefix="memory-compaction-",dir=ROOT/".cache") as directory:
            cases = [("remote_raw_off",{"raw":False}), ("remote_raw_on",{}),
                     ("remote_deprecated_flag_on",{"old_flag":True}),
                     ("local_summary_raw_on",{"remote":"unsupported"})] if args.flags_only else [
                ("ephemeral_manual",{}),("ephemeral_injected_trigger",{"inject":True}),
                ("memory_store_legacy",{"memory":True}),
                ("memory_store_paginated",{"memory":True,"paginated":True})]
            for label, options in cases:
                case = run_case(Path(directory)/label,server,label,**options)
                report["cases"].append(case)
                print(json.dumps(case),flush=True)
    finally:
        server.shutdown()
        server.server_close()
    report["syntheticModelRequests"] = len(server.seen)
    target = ROOT/".cache"/("native-compaction-flags.json" if args.flags_only else "native-memory-compaction.json")
    target.write_text(json.dumps(report,indent=2),encoding="utf-8")
    print(json.dumps({"report":str(target),"syntheticModelRequests":len(server.seen)}))


if __name__=="__main__":
    main()
