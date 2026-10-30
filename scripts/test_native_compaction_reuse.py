"""Installed app-server acceptance; invoked by test_account_concurrency.py.

Only loopback synthetic model traffic. The client keeps its original transcript;
native owns the compressed prompt. Captured cache IDs prove continuity, not a
real token cache hit-rate improvement.
"""
import copy
import json
import re
import subprocess
import urllib.request
import uuid
from pathlib import Path


def respond(handler, body, wire, lock):
    compact = any(item.get("type") == "compaction_trigger" for item in body["input"])
    cache = body.get("prompt_cache_key")
    assert cache
    with lock:
        previous = [entry for entry in wire if entry["cache"] == cache]
        generation = sum(not entry["compact"] for entry in previous) + 1
        markers = re.findall(r"fixture-compact:([a-z-]+):[0-9]+", json.dumps(body["input"]))
        scene = markers[-1] if markers else previous[-1]["scene"]
        assert len(wire) < 100, "unexpected native retry/compaction loop"
        entry = {"scene": scene, "cache": cache, "compact": compact, "body": body,
                 "session": handler.headers.get("session-id") or handler.headers.get("session_id")}
        wire.append(entry)
    if compact:
        items = [{"type": "compaction", "id": "cmp_" + uuid.uuid4().hex,
                  "encrypted_content": "SYNTHETIC_COMPACT_" + scene + "_" + uuid.uuid4().hex}]
    elif scene == "tools" and generation == 2:
        items = [{"type": "function_call", "id": "fc_" + str(index), "call_id": "compact-call-" + str(index),
                  "namespace": "functions", "name": "client_echo", "arguments": json.dumps({"index": index})}
                 for index in range(2)]
    else:
        items = [{"type": "message", "id": "msg_" + uuid.uuid4().hex, "role": "assistant", "status": "completed",
                  "content": [{"type": "output_text", "text": f"fixture-answer:{scene}:{generation}", "annotations": []}]}]
    tokens = 10 if compact else (10000 if generation in (1, 3) or (scene == "tools" and generation == 2) else 40)
    if scene == "tools" and generation == 3:
        tokens = 40
    events = [{"type": "response.output_item.done", "output_index": index, "item": item}
              for index, item in enumerate(items)]
    events.append({"type": "response.completed", "response": {"id": "resp_" + uuid.uuid4().hex,
                  "status": "completed", "output": items,
                  "usage": {"input_tokens": tokens, "output_tokens": 2, "total_tokens": tokens + 2,
                            "input_tokens_details": {"cached_tokens": 0}}}})
    data = "".join("data: " + json.dumps(event) + "\n\n" for event in events).encode()
    handler.send_response(200)
    handler.send_header("Content-Type", "text/event-stream")
    handler.send_header("Content-Length", str(len(data)))
    handler.end_headers()
    handler.wfile.write(data)


def check(work, base, key, other_key, admin, wire, position):
    tools = [{"type": "function", "name": "client_echo", "parameters": {"type": "object"}}]

    def send(history, identity, token=key, streaming=False, catalog=None):
        before = {row["id"] for row in admin("/api/statistics")["records"]}
        body = {"model": "gpt-6.1-sol", "input": history, "stream": streaming,
                "client_metadata": {"thread_id": identity}, "tools": catalog or []}
        req = urllib.request.Request(base + "/v1/responses", data=json.dumps(body).encode(),
            headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"})
        with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(req, timeout=90) as response:
            if streaming:
                events = [json.loads(line[6:]) for line in response.read().decode().splitlines()
                          if line.startswith("data: ") and line != "data: [DONE]"]
                assert not any(event["type"] in ("error", "response.failed", "response.incomplete") for event in events), events
                result = next(event["response"] for event in events if event["type"] == "response.completed")
            else:
                result = json.load(response)
        assert result["status"] == "completed", result
        assert result["usage"]["input_tokens_details"]["cached_tokens"] == 0, result["usage"]
        rows = [row for row in admin("/api/statistics")["records"] if row["id"] not in before]
        assert len(rows) == 1, rows
        return result, rows[0]

    def user(scene, index):
        return {"role": "user", "content": f"fixture-compact:{scene}:{index}"}

    history, text_records, text_compactions = [], [], []
    for index in range(4):
        history.append(user("text", index))
        result, row = send(history, "compact-text", streaming=index % 2 == 1)
        history.extend(result["output"])
        text_records.append(row)
        text_compactions.append(sum(entry["compact"] for entry in wire if entry["scene"] == "text"))
    assert len({row["nativeThreadId"] for row in text_records}) == 1, text_records
    text = [entry for entry in wire if entry["scene"] == "text"]
    assert 2 <= text_compactions[-1] <= 8, text_compactions
    assert text_compactions[0] == (1 if position == "after" else 0), text_compactions
    generations = [entry for entry in text if not entry["compact"]]
    assert len(generations) == 4
    assert len({entry["cache"] for entry in text}) == len({entry["session"] for entry in text}) == 1
    assert all(any(item["type"] == "compaction" for item in entry["body"]["input"]) for entry in generations[1:])
    prefix = generations[1]["body"]["input"]
    assert generations[2]["body"]["input"][:len(prefix)] == prefix, "compressed prefix changed without compaction"
    for entry in generations:
        questions = [item for item in entry["body"]["input"] if item.get("role") == "user"]
        assert len({json.dumps(item, sort_keys=True) for item in questions}) == len(questions), "history injected twice"

    # A confirmed native compaction never weakens the external transcript scope.
    for identity, token, edited in [("compact-fork", key, False), ("compact-text", other_key, False),
                                    ("compact-text", key, True)]:
        changed = copy.deepcopy(history) + [user("text", 4)]
        if edited:
            changed[0]["content"] += " edited"
        _, row = send(changed, identity, token=token)
        assert row["nativeThreadId"] != text_records[-1]["nativeThreadId"]

    history = [user("tools", 0)]
    warm, root = send(history, "compact-tools", catalog=tools)
    history.extend(warm["output"])
    history.append(user("tools", 1))
    yielded, waiting = send(history, "compact-tools", streaming=True, catalog=tools)
    calls = [item for item in yielded["output"] if item["type"] == "function_call"]
    assert len(calls) == 2, yielded
    assert admin("/api/operations")["active"] == 0, "waiting tools still occupy request capacity"
    history.extend(yielded["output"])
    history.extend({"type": "function_call_output", "call_id": call["call_id"],
                    "output": "ACTUAL_COMPACT_TOOL_RESULT_" + str(index)} for index, call in enumerate(calls))
    answered, resumed = send(history, "compact-tools", catalog=tools)
    assert waiting["nativeTurnId"] == resumed["nativeTurnId"], "tool callback restarted its native turn"
    history.extend(answered["output"])
    history.append(user("tools", 2))
    _, continued = send(history, "compact-tools", catalog=tools)
    assert len({row["nativeThreadId"] for row in [root, waiting, resumed, continued]}) == 1
    tool_entries = [entry for entry in wire if entry["scene"] == "tools"]
    assert len([entry for entry in tool_entries if not entry["compact"]]) == 4
    assert 2 <= len([entry for entry in tool_entries if entry["compact"]]) <= 8
    assert len({entry["cache"] for entry in tool_entries}) == len({entry["session"] for entry in tool_entries}) == 1
    returned = [item for entry in tool_entries for item in entry["body"]["input"] if item["type"] == "function_call_output"]
    assert returned
    for item in returned:
        assert item["output"].startswith("ACTUAL_COMPACT_TOOL_RESULT_"), item
    for entry in tool_entries:
        ids = [item["call_id"] for item in entry["body"]["input"] if item["type"] == "function_call_output"]
        assert len(ids) == len(set(ids)), "duplicate tool result"

    result = subprocess.run(["node", str(Path(__file__).with_name("test_native_cache_websocket.mjs")), base, key, "compaction"],
        capture_output=True, text=True, encoding="utf-8", timeout=150,
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    assert result.returncode == 0, result.stdout + result.stderr
    ws = [entry for entry in wire if entry["scene"] == "websocket"]
    assert len([entry for entry in ws if not entry["compact"]]) == 3
    assert any(entry["compact"] for entry in ws)
    assert len({entry["cache"] for entry in ws}) == len({entry["session"] for entry in ws}) == 1
    assert any(item["type"] == "compaction" for item in ws[-1]["body"]["input"])
    return {"position": position, "json_sse_text": "passed", "tool_callback_and_next_user": "passed",
            "edited_history_and_scope_isolation": "passed", "websocket_previous_response": "passed",
            "text_compactions_after_each_response": text_compactions,
            "synthetic_requests": len(wire), "real_model_calls": 0}
