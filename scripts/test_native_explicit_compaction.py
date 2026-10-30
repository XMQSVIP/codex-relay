"""Installed auth process + direct compaction acceptance. Synthetic upstream only."""
import concurrent.futures
import json
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

CANARY = "SYNTHETIC_NO_DISK_COMPACT"
CIPHER = "SYNTHETIC_DIRECT_CIPHER"


def respond(handler, body, wire, lock):
    with lock:
        wire.append({"body": body, "path": handler.path})
        index = len(wire)
    assert handler.path == "/v1/responses"
    assert body["store"] is False
    compact = any(item.get("type") == "compaction_trigger" for item in body["input"])
    serialized = json.dumps(body)
    if "TEST_HOLD" in serialized:
        time.sleep(3)
    if "TEST_HTTP_FAILURE" in serialized:
        handler.send_response(429)
        handler.end_headers()
        return
    item = ({"type": "compaction", "id": "cmp_direct", "encrypted_content": CIPHER}
            if compact else {"type":"message","id":"msg_direct","role":"assistant",
                             "content":[{"type":"output_text","text":"continued","annotations":[]}]})
    events = [{"type":"response.output_item.done","output_index":0,"item":item}]
    if "TEST_TRUNCATED" not in serialized:
        events.append({"type":"response.completed","response":{"id":f"resp_direct_{index}",
            "object":"response", "created_at":1234567890,"status":"completed", "output":[item],
            "usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}})
    if "TEST_FAILED_USAGE" in serialized:
        events[-1]["type"] = "response.failed"
        events[-1]["response"]["status"] = "failed"
        events[-1]["response"]["error"] = {"message":CANARY}
    handler.send_response(200)
    handler.send_header("Content-Type", "text/event-stream")
    handler.end_headers()
    try:
        handler.wfile.write("".join("data: "+json.dumps(event)+"\n\n" for event in events).encode())
    except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
        pass


def check(work, base, key, other_key, admin, wire, account):
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    def post(path, body, bearer=key):
        req = urllib.request.Request(base+path, data=json.dumps(body).encode(),
            headers={"Authorization":"Bearer "+bearer,"Content-Type":"application/json"})
        with opener.open(req, timeout=90) as response:
            data = response.read()
            if "text/event-stream" in response.headers.get("content-type", ""):
                events = [json.loads(line[6:]) for line in data.decode().splitlines() if line.startswith("data: ")]
                return next(e["response"] for e in events if e["type"]=="response.completed")
            return json.loads(data)
    body = {"model":"gpt-6.1-sol", "input":[{"role":"user","content":CANARY}]}
    result = post("/v1/responses/compact", body)
    assert result["object"] == "response.compaction"
    expected = {"type":"compaction","id":"cmp_direct","encrypted_content":CIPHER}
    assert result["output"] == [expected]
    assert "input_tokens_details" not in result["usage"]
    for stream in (False, True):
        result = post("/v1/responses", {**body, "stream":stream, "input":body["input"]+[{"type":"compaction_trigger"}]})
        assert result["output"] == [expected]
    assert len(wire) == 3
    continued = post("/v1/responses", {**body, "input":[expected,{"role":"user","content":"continue"}]})
    assert continued["status"] == "completed"
    before = len(wire)
    try:
        post("/v1/responses/compact", {**body, "input":[expected]}, other_key)
        raise AssertionError("Cross-key encrypted history must fail")
    except urllib.error.HTTPError:
        pass
    assert len(wire)==before
    ws = subprocess.run(["node",str(Path(__file__).with_name("test_native_cache_websocket.mjs")),base,key,"compact-direct"],
        capture_output=True,text=True,encoding="utf-8",timeout=120,creationflags=getattr(subprocess,"CREATE_NO_WINDOW",0))
    assert ws.returncode==0,ws.stdout+ws.stderr
    assert len(wire)==before+2
    assert wire[-1]["body"]["input"][0]==expected
    for marker in ("TEST_TRUNCATED", "TEST_HTTP_FAILURE"):
        before = len(wire)
        try:
            post("/v1/responses/compact", {**body,"input":CANARY+marker})
            raise AssertionError("Failed compaction must not succeed")
        except urllib.error.HTTPError:
            pass
        assert len(wire)==before+1, "Compaction was replayed"
    failed = post("/v1/responses/compact", {**body,"input":CANARY+"TEST_FAILED_USAGE"})
    assert failed["status"]=="failed" and failed["usage"]["total_tokens"]==12
    assert failed["output"]==[] and CANARY not in json.dumps(failed)
    # The installed official client invokes remote v2 through the relay, receives
    # the genuine item, installs it in RAM, then sends it in its next model request.
    from probe_native_output_limit import INSTALL, Native
    from probe_native_context_management import finish_turn
    with tempfile.TemporaryDirectory(prefix="remote-client-",dir=work) as directory:
        native = Native(Path(directory), int(base.rsplit(":",1)[1]))
        try:
            assert "result" in native.call("initialize",{"clientInfo":INSTALL["nativeClientInfo"],"capabilities":{"experimentalApi":True}})
            native.send({"method":"initialized"})
            start = native.call("thread/start",{"model":"gpt-6.1-sol","modelProvider":"synthetic",
                "cwd":str(native.cwd),"ephemeral":True,"approvalPolicy":"untrusted","sandbox":"read-only",
                "dynamicTools":[],"environments":[],"runtimeWorkspaceRoots":[],"selectedCapabilityRoots":[],
                "baseInstructions":"","developerInstructions":"", "config":{
                    "model_providers.synthetic.base_url":base+"/v1",
                    "model_providers.synthetic.experimental_bearer_token":key,
                    "model_providers.synthetic.capabilities.remote_compaction":"v2"}})
            thread = start["result"]["thread"]["id"]
            native.call("thread/inject_items",{"threadId":thread,"items":[{"type":"message","role":"user","content":[{"type":"input_text","text":CANARY}]}]})
            before = len(wire)
            assert "result" in native.call("thread/compact/start",{"threadId":thread})
            finish_turn(native,thread)
            assert len(wire)==before+1
            native.call("turn/start",{"threadId":thread,"input":[{"type":"text","text":"continue","text_elements":[]}]})
            finish_turn(native,thread)
            assert len(wire)==before+2
            assert any(i.get("encrypted_content")==CIPHER for i in wire[-1]["body"]["input"])
        finally:
            native.close()
    assert admin("/api/operations")["active"] == 0
    records = admin("/api/statistics")["records"]
    compact_records = [row for row in records if row.get("inputTokens") is not None and not row.get("nativeThreadId")]
    assert len(compact_records)==7
    assert all(row["inputTokens"]==10 and row["outputTokens"]==2 and row.get("cachedInputTokens") is None for row in compact_records)
    # Scan service-owned data/logs while alive. The harness writes its synthetic
    # capture only after this check; production must never write either marker.
    found = []
    for path in work.rglob("*"):
        if path.is_file() and path.stat().st_size < 64*1024*1024:
            try:
                data = path.read_bytes()
                if CANARY.encode() in data or CIPHER.encode() in data:
                    found.append(str(path.relative_to(work)))
            except OSError:
                pass  # native .lock files are not readable while running
    assert not found, found
    before = len(wire)
    with concurrent.futures.ThreadPoolExecutor() as pool:
        pending=pool.submit(post,"/v1/responses/compact",{**body,"input":CANARY+"TEST_HOLD"})
        for _ in range(300):
            if len(wire)>before: break
            time.sleep(.05)
        assert len(wire)==before+1
        admin(f"/api/accounts/{account}/stop",{})
        try:
            value=pending.result(timeout=15)
            assert value.get("status")=="failed", value
        except urllib.error.HTTPError:
            pass
    assert admin("/api/operations")["active"]==0
    return {"http_json_sse_websocket":"passed","official_remote_compaction_and_continue":"passed",
            "no_replay_failure_and_account_stop":"passed","body_disk_scan":"passed"}
