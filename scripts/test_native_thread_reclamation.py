"""Installed app-server reclamation acceptance. Only synthetic localhost traffic."""
import concurrent.futures
import json
import threading
import time
import uuid

ARRIVED = threading.Event()
RELEASE = threading.Event()
CANARY = "SYNTHETIC_RECLAMATION_PRIVATE_781"


def respond(handler, body, wire, lock):
    with lock:
        wire.append({"body": body, "session": handler.headers.get("session_id") or handler.headers.get("session-id")})
        sequence = len(wire)
    text = json.dumps(body.get("input"))
    if "HOLD_RECLAIM" in text:
        ARRIVED.set()
        assert RELEASE.wait(60)
    outputs = [item for item in body["input"] if item.get("type")=="function_call_output"]
    item = ({"type":"function_call","id":"fc_reclaim","call_id":"call_reclaim", "name":"echo","arguments":"{}"}
            if "TOOL_RECLAIM" in text and not outputs else
            {"type":"message","id":"msg_"+uuid.uuid4().hex,"role":"assistant", "status":"completed",
             "content":[{"type":"output_text","text":"synthetic answer","annotations":[]}]})
    events = [{"type":"response.output_item.done","output_index":0,"item":item},
              {"type":"response.completed","response":{"id":f"resp_reclaim_{sequence}","status":"completed", "output":[item],
                "usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}}]
    data = "".join("data: "+json.dumps(event)+"\n\n" for event in events).encode()
    handler.send_response(200)
    handler.send_header("Content-Type","text/event-stream")
    handler.send_header("Content-Length",str(len(data)))
    handler.end_headers()
    try:
        handler.wfile.write(data)
    except (BrokenPipeError,ConnectionAbortedError,ConnectionResetError):
        pass


def check(work, base, key, other_key, admin, wire, account):
    histories = {}
    def call(name, edit=None, bearer=key, tools=None):
        history=histories.setdefault((bearer,name),[])
        if edit is not None:
            history.clear()
        history.append({"role":"user","content":CANARY+" "+(edit if edit is not None else name)})
        start=len(wire)
        result=admin("/v1/responses",{"model":"gpt-6.1-sol","input":history,
            "client_metadata":{"thread_id":name},"tools":tools or []},bearer)
        assert result["status"]=="completed",result
        history.extend(result["output"])
        return wire[start],result
    a,_=call("A")
    old_a=list(histories[(key,"A")])
    b,_=call("B")
    a_new,_=call("A",edit="A edited")
    assert a["body"]["prompt_cache_key"]!=a_new["body"]["prompt_cache_key"]
    call("D")  # A old is newer than B by LRU, but obsolete version takes priority.
    b_next,_=call("B")
    assert b_next["body"]["prompt_cache_key"]==b["body"]["prompt_cache_key"]
    assert b_next["session"]==b["session"]
    a_next,_=call("A")
    assert a_next["body"]["prompt_cache_key"]==a_new["body"]["prompt_cache_key"]
    histories[(key,"A")]=old_a
    rebuilt,_=call("A")
    assert rebuilt["body"]["prompt_cache_key"]!=a["body"]["prompt_cache_key"]
    assert CANARY+" A" in json.dumps(rebuilt["body"]["input"])
    # More than the old lifetime thread limit, with one hot context protected.
    hot,_=call("HOT")
    for i in range(36):
        call(f"cold-{i}")
        again,_=call("HOT")
        assert again["session"]==hot["session"]
        assert again["body"]["prompt_cache_key"]==hot["body"]["prompt_cache_key"]
    # A waiting callback must survive normal idle-context pressure.
    tools=[{"type":"function","name":"echo","parameters":{"type":"object"}}]
    waiting,result=call("TOOL_RECLAIM",tools=tools)
    assert admin("/api/operations")["active"]==0
    for i in range(4):
        call(f"around-tools-{i}")
    history=histories[(key,"TOOL_RECLAIM")]
    history.append({"type":"function_call_output","call_id":"call_reclaim","output":"EXACT_TOOL_RESULT"})
    resumed=admin("/v1/responses",{"model":"gpt-6.1-sol","input":history,"tools":tools,
        "client_metadata":{"thread_id":"TOOL_RECLAIM"}},key)
    assert resumed["status"]=="completed",resumed
    assert wire[-1]["session"]==waiting["session"]
    assert sum(i.get("type")=="function_call_output" for i in wire[-1]["body"]["input"])==1
    # A running sibling also survives while idle contexts are evicted.
    with concurrent.futures.ThreadPoolExecutor() as pool:
        pending=pool.submit(call,"HOLD_RECLAIM")
        assert ARRIVED.wait(20)
        try:
            for i in range(4):
                call(f"around-active-{i}")
        finally:
            RELEASE.set()
        pending.result(timeout=30)
    cross,_=call("HOT",bearer=other_key)
    assert cross["session"]!=hot["session"]
    # Full replay grows beyond the configured 32 MiB cumulatively, while the
    # native context only receives the large initial message once.
    budget_start=len(wire)
    budget,_=call("BUDGET",edit="x"*(6*1024*1024))
    for _ in range(6):
        continued,_=call("BUDGET")
        assert continued["session"]==budget["session"],"full histories were charged repeatedly"
    for row in wire[budget_start:]:
        row["inputBytes"]=len(json.dumps(row["body"]["input"]).encode())
        row["body"]["input"]=[{"synthetic_capture_omitted":True}]
    assert admin("/api/operations")["active"]==0
    preflights=[]
    for line in (work/"gateway.log").read_text(encoding="utf-8").splitlines():
        try:
            value=json.loads(line)
        except json.JSONDecodeError:
            continue
        if value.get("event")=="request_preflight":preflights.append(value)
    assert len(preflights)==len(wire), "request was replayed or not measured"
    assert len({row["nativeProcessId"] for row in preflights})==1,"idle eviction rotated a healthy shared process"
    old_directories=list((work/"data/accounts"/account/"workspace").glob("worker-*"))
    for i in range(270):
        call(f"directory-limit-{i}")
    time.sleep(.2)
    preflights=[json.loads(line) for line in (work/"gateway.log").read_text(encoding="utf-8").splitlines()
                if '"event":"request_preflight"' in line]
    assert len(preflights)==len(wire)
    assert len({row["nativeProcessId"] for row in preflights})==2,"directory budget must drain and rotate exactly once"
    assert all(not directory.exists() for directory in old_directories),"retired process directory leaked"
    unreadable=[]
    marker=CANARY.encode()
    for path in (work/"data").rglob("*"):
        if path.is_file():
            try:
                with path.open("rb") as source:
                    tail=b""
                    while chunk:=source.read(1024*1024):
                        chunk=tail+chunk
                        assert marker not in chunk,path
                        tail=chunk[-len(marker):]
            except OSError:
                unreadable.append(str(path.relative_to(work/"data")))
    admin(f"/api/accounts/{account}/stop",{})
    assert admin("/api/operations")["active"]==0
    return {"old_version_priority":"passed","hot_context_and_over_32_threads":"passed",
            "active_and_tool_sibling_isolation":"passed","cross_key_and_full_history_rebuild":"passed",
            "incremental_input_budget_and_directory_rotation":"passed", "no_replay_no_body_on_disk":"passed",
            "unreadable_data_files":unreadable}
