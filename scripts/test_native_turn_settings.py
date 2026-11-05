"""Native turn-setting compatibility acceptance; no real model requests."""
import json
import subprocess
import tempfile
import urllib.request
import urllib.error
import urllib.parse
from pathlib import Path


def client_local_compaction(work, base, key, wire):
    from probe_native_output_limit import INSTALL, Native
    from probe_native_context_management import finish_turn
    before = len(wire)
    with tempfile.TemporaryDirectory(prefix="client-local-compact-", dir=work) as directory:
        native = Native(Path(directory), urllib.parse.urlparse(base).port)
        try:
            assert "result" in native.call("initialize", {"clientInfo":INSTALL["nativeClientInfo"],
                "capabilities":{"experimentalApi":True}})
            native.send({"method":"initialized"})
            started = native.call("thread/start", {"model":"gpt-6.1-sol","modelProvider":"synthetic",
                "cwd":str(native.cwd),"ephemeral":True,"approvalPolicy":"untrusted","sandbox":"read-only",
                "dynamicTools":[],"environments":[],"runtimeWorkspaceRoots":[],"selectedCapabilityRoots":[],
                "baseInstructions":"","developerInstructions":"","experimentalRawEvents":True,
                "config":{"model_providers.synthetic.base_url":base+"/v1",
                          "model_providers.synthetic.experimental_bearer_token":key,
                          "model_providers.synthetic.capabilities.remote_compaction":"unsupported"}})
            thread = started["result"]["thread"]["id"]
            assert "result" in native.call("thread/inject_items", {"threadId":thread,"items":[
                {"type":"message","role":"user","content":[{"type":"input_text","text":"concurrency-desktop"}]}]})
            assert "result" in native.call("thread/compact/start", {"threadId":thread})
            events = finish_turn(native, thread)
            assert any(event.get("method")=="rawResponseItem/completed" for event in events)
            assert len(wire) == before + 1
            assert not any(item["type"]=="compaction_trigger" for item in wire[-1]["body"]["input"])
            assert "result" in native.call("turn/start", {"threadId":thread,"input":[
                {"type":"text","text":"concurrency-desktop","text_elements":[]}]})
            finish_turn(native, thread)
            assert len(wire) == before + 2
        finally:
            native.close()


def check(work, base, key, admin, wire):
    schema = {"type": "object", "properties": {"value": {"type": "string"}},
              "required": ["value"], "additionalProperties": False}
    first = {"type": "json_schema", "name": "first", "schema": schema, "strict": True}
    second = {**first, "name": "second", "schema": {**schema, "description": "Changed for this turn"}}
    rows, history = [], []
    for index, (summary, format_) in enumerate([
            (None, None), ("detailed", first), ("none", second), (None, None),
            ("auto", None), ("detailed", {"type": "json_object"}), (None, None)]):
        if index == 1:
            history.extend([{"role": "system", "content": "SYNTHETIC_NEW_SYSTEM_INSTRUCTION"},
                            {"role": "developer", "content": "SYNTHETIC_NEW_DEVELOPER_INSTRUCTION"}])
        history.append({"role": "user", "content": "concurrency-desktop"})
        before = {row["id"] for row in admin("/api/statistics")["records"]}
        body = {"model": "gpt-6.1-sol", "input": history, "stream": index == 1,
                "client_metadata": {"thread_id": "settings-client"}, "reasoning": {"summary": summary}}
        if format_:
            body["text"] = {"format": format_}
        req = urllib.request.Request(base + "/v1/responses", data=json.dumps(body).encode(),
            headers={"Authorization": "Bearer " + key, "Content-Type": "application/json"})
        with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(req, timeout=90) as response:
            data = response.read()
        if body["stream"]:
            events = [json.loads(line[6:]) for line in data.decode().splitlines()
                      if line.startswith("data: ") and line != "data: [DONE]"]
            assert not any(event["type"] in ("error", "response.failed", "response.incomplete") for event in events), events
            result = next(event["response"] for event in events if event["type"] == "response.completed")
        else:
            result = json.loads(data)
        assert result["status"] == "completed", result
        history.extend(result["output"])
        new = [row for row in admin("/api/statistics")["records"] if row["id"] not in before]
        assert len(new) == 1
        rows.extend(new)
    assert len(wire) == 7, "turn changes replayed a model request"
    assert len({row["nativeThreadId"] for row in rows[:5]}) == 1, rows
    assert rows[5]["nativeThreadId"] != rows[4]["nativeThreadId"], "json_object changes thread instructions"
    assert rows[6]["nativeThreadId"] != rows[5]["nativeThreadId"]
    expected = ["concise", "detailed", None, "concise", "auto", "detailed", "concise"]
    assert [entry["body"].get("reasoning", {}).get("summary") for entry in wire] == expected
    for index, format_ in [(1, first), (2, second)]:
        assert wire[index]["body"]["text"]["format"]["schema"] == format_["schema"]
    assert all(entry["body"].get("text", {}).get("format") is None for entry in [wire[0], *wire[3:]])
    prefix = wire[0]["body"]["input"]
    for entry in wire[1:5]:
        assert entry["body"]["input"][:len(prefix)] == prefix
        assert entry["body"]["prompt_cache_key"] == wire[0]["body"]["prompt_cache_key"]
        assert entry["session"] == wire[0]["session"]
    assert "SYNTHETIC_NEW_DEVELOPER_INSTRUCTION" in json.dumps(wire[1]["body"]["input"])
    assert "SYNTHETIC_NEW_SYSTEM_INSTRUCTION" in json.dumps(wire[1]["body"]["input"])
    for mode in ["live-settings", "live-reset"]:
        before = {row["id"] for row in admin("/api/statistics")["records"]}
        start = len(wire)
        result = subprocess.run(["node", str(Path(__file__).with_name("test_native_cache_websocket.mjs")),
            base, key, mode], capture_output=True, text=True, encoding="utf-8", timeout=150,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        assert result.returncode == 0, result.stdout + result.stderr
        records = [row for row in admin("/api/statistics")["records"] if row["id"] not in before]
        assert len(records) == 3 and len({row["nativeThreadId"] for row in records}) == 1, records
        assert len(wire) == start + 3, "live settings caused an extra model request"
        frames = wire[start:]
        assert len({entry["session"] for entry in frames}) == 1
        assert len({entry["body"]["prompt_cache_key"] for entry in frames}) == 1
        assert all(entry["body"]["input"][:len(frames[0]["body"]["input"])] == frames[0]["body"]["input"]
                   for entry in frames[1:])
        for entry in frames[1:]:
            outputs = [item for item in entry["body"]["input"] if item["type"] == "function_call_output"]
            assert len(outputs) == 1 and "WS_CLIENT_RESULT" in json.dumps(outputs)
    assert len(wire) == 13
    assert wire[8]["body"]["reasoning"]["effort"] == "high"
    assert wire[8]["body"]["reasoning"]["summary"] == "detailed"
    assert wire[8]["body"]["service_tier"] == "priority"
    assert wire[9]["body"]["reasoning"]["summary"] == "concise"
    assert wire[11]["body"]["reasoning"] == wire[7]["body"]["reasoning"]
    assert wire[11]["body"].get("service_tier") is None
    assert wire[12]["body"]["reasoning"] == wire[7]["body"]["reasoning"]
    assert wire[12]["body"].get("service_tier") is None
    client_local_compaction(work, base, key, wire)
    return {"schema_summary_and_default_reset": "passed", "appended_instructions": "passed",

            "official_client_local_compaction_and_continuation": "passed",
            "live_tool_turn_settings": "passed", "live_default_reset": "passed",
            "model_requests": len(wire), "real_model_calls": 0}
