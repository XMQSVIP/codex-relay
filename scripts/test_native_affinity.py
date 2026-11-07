"""Admission affinity and identity acceptance using isolated installed app-servers."""
import json
import subprocess
from pathlib import Path


def check_affinity(work, base, key, request, pool, wait, seen, unblock, wire, account_id):
    group = "00000000-0000-0000-0000-000000000000"
    second = request("/api/accounts", {"name": "affinity-second", "nativeConfig": True})["id"]
    for account in (account_id, second):
        ops = request("/api/operations")
        request(f"/api/accounts/{account}/route", {"revision": ops["revision"], "enabled": True})
        ops = request("/api/operations")
        request(f"/api/accounts/{account}/policy", {"revision": ops["revision"], "enabled": True,
                "groupIds": [group], "maxConcurrency": 1})

    def records():
        return [row for row in request("/api/statistics")["records"] if row["action"] == "responses"]

    def send(marker, thread, parent=None, history=None):
        metadata = {"thread_id": thread}
        if parent:
            metadata["parent_thread_id"] = parent
        return request("/v1/responses", {"model": "gpt-6.1-sol", "input": history or marker,
                       "client_metadata": metadata, "stream": False}, key)

    def latest_new(before):
        added = [row for row in records() if row["id"] not in before]
        assert len(added) == 1, added
        return added[0]

    first = pool.submit(send, "concurrency-A", "root")
    wait(lambda: seen("concurrency-A"))
    root_record = records()[0]
    home = root_record["accountId"]
    before = {row["id"] for row in records()}
    spill = pool.submit(send, "concurrency-B", "root")
    wait(lambda: seen("concurrency-B"))
    assert not first.done(), "binding must exist before the first response completes"
    unblock("concurrency-B")
    spill.result(timeout=60)
    spill_record = latest_new(before)
    assert spill_record["accountId"] != home

    before = {row["id"] for row in records()}
    child = pool.submit(send, "concurrency-C", "child", "root")
    wait(lambda: seen("concurrency-C"))
    unblock("concurrency-C")
    child_answer = child.result(timeout=60)
    child_record = latest_new(before)
    assert child_record["accountId"] != home, "busy parent must not cause affinity waiting"
    unblock("concurrency-A")
    root_answer = first.result(timeout=60)

    before = {row["id"] for row in records()}
    root_history = [{"role": "user", "content": "concurrency-A"}, *root_answer["output"],
                    {"role": "user", "content": "concurrency-A"}]
    send("concurrency-A", "root", history=root_history)
    returned = latest_new(before)
    root_record = next(row for row in records() if row["id"] == root_record["id"])
    assert returned["accountId"] == home
    assert returned["nativeThreadId"] == root_record["nativeThreadId"]

    before = {row["id"] for row in records()}
    child_history = [{"role": "user", "content": "concurrency-C"}, *child_answer["output"],
                     {"role": "user", "content": "concurrency-C"}]
    send("concurrency-C", "child", "root", child_history)
    child_returned = latest_new(before)
    assert child_returned["accountId"] == child_record["accountId"]
    assert child_returned["nativeThreadId"] == child_record["nativeThreadId"]

    # A fresh child inherits the available parent, but cannot take its thread.
    before = {row["id"] for row in records()}
    send("concurrency-A", "fork", "root", root_history)
    fork = latest_new(before)
    assert fork["accountId"] == home
    assert fork["nativeThreadId"] != returned["nativeThreadId"]
    assert len(wire) == 6, "a model request must be sent exactly once per HTTP request"
    for marker in ("concurrency-A", "concurrency-C"):
        entries = [entry for entry in wire if entry["marker"] == marker][:2]
        assert entries[0]["session"] == entries[1]["session"]
        assert entries[0]["body"]["prompt_cache_key"] == entries[1]["body"]["prompt_cache_key"]
        prefix = entries[0]["body"]["input"]
        assert entries[1]["body"]["input"][:len(prefix)] == prefix

    unblock("concurrency-desktop")
    result = subprocess.run(["node", str(Path(__file__).with_name("test_native_cache_websocket.mjs")),
                             base, key, "identity"], capture_output=True, text=True, encoding="utf-8", timeout=150,
                            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    assert result.returncode == 0, result.stdout + result.stderr
    assert len(wire) == 9
    return {"first_admission_and_spillover": "passed", "independent_child_home": "passed",
            "parent_inheritance_and_thread_isolation": "passed", "websocket_frame_identity": "passed",
            "model_requests": len(wire), "real_model_calls": 0}
