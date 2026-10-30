"""Installed-native concurrency acceptance against localhost synthetic responses only.

Uses an isolated CODEX_HOME and gateway data directory; never reads real credentials.
Run after building: python scripts/test_account_concurrency.py --binary <codex-relay.exe>
Evidence and the isolated account remain under .cache/account-concurrency-native-*.
"""
import argparse
import concurrent.futures
import copy
import json
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parent.parent
GROUP = "00000000-0000-0000-0000-000000000000"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/codex-relay.exe")
    parser.add_argument("--check-standby", action="store_true", help="also verify prewarming, 125s idle reuse and disabled-account cleanup")
    parser.add_argument("--capacity", action="store_true", help="also exercise 10 native accounts with 100 simultaneous synthetic requests")
    parser.add_argument("--check-cache-expiry", action="store_true", help="wait the real five-minute tool TTL and verify full-history rebuilding")
    parser.add_argument("--desktop-cache-only", action="store_true", help="check three turns from an installed native client through the relay")
    parser.add_argument("--affinity-only", action="store_true", help="check two native accounts and per-frame thread isolation")
    parser.add_argument("--thread-reclamation-only", action="store_true")
    parser.add_argument("--explicit-compact-only", action="store_true")
    parser.add_argument("--compaction-only", action="store_true", help="check native auto-compaction and retained thread/tool ownership")
    parser.add_argument("--compaction-position", choices=("before", "after"), default="before")
    parser.add_argument("--turn-settings-only", action="store_true", help="check native schema, summary, appended instructions and live tool-turn overrides")
    parser.add_argument("--mixed-models-only", action="store_true", help="check concurrent models share one host while histories and cancellation stay independent")
    parser.add_argument("--group-continuation-only", action="store_true", help="check authorized group moves preserve HTTP and WebSocket native continuations")
    parser.add_argument("--desktop-first-reasoning", action="store_true", help="include a reasoning item only in the first desktop reply")
    from native_cache_audit_cases import CASES, response_items
    parser.add_argument("--desktop-case", choices=CASES, default="text", help="installed client response roundtrip variant")
    args = parser.parse_args()
    binary = args.binary.resolve()
    assert binary.is_file()
    installation = ROOT / ".cache/launch-config.json"
    assert installation.is_file()
    work = ROOT / ".cache" / ("account-concurrency-native-" + uuid.uuid4().hex)
    work.mkdir()
    executable = work / binary.name
    shutil.copyfile(binary, executable)
    (work / ".cache").mkdir()
    shutil.copyfile(installation, work / ".cache/launch-config.json")
    home = work / "synthetic-home"
    home.mkdir()
    (work / "web").mkdir()
    (work / "web/index.html").write_text("synthetic fixture", encoding="utf-8")
    arrived, release = {}, {}
    lock = threading.Lock()
    calls = []
    tool_wire = []

    class Model(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps({"data": [{"id": "gpt-6.1-sol"}]}).encode())

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            assert self.headers.get("Authorization") == "Bearer SYNTHETIC_CONCURRENCY_ONLY"
            if args.thread_reclamation_only:
                from test_native_thread_reclamation import respond
                return respond(self, body, tool_wire, lock)
            if args.explicit_compact_only or (args.turn_settings_only and any(item.get("type")=="compaction_trigger" for item in body.get("input", []))):
                from test_native_explicit_compaction import respond
                return respond(self, body, tool_wire, lock)
            if args.compaction_only:
                from test_native_compaction_reuse import respond
                return respond(self, body, tool_wire, lock)
            # Native environment messages also contain the temporary directory
            # name account-concurrency-native-*. Match only our input markers.
            markers = re.findall(r"concurrency-(?:[A-J]2?|rotation-(?:sibling|[0-9]+)|tool-[ABCD]|desktop)(?:-answer)?", json.dumps(body.get("input")))
            assert markers, "missing synthetic request marker"
            marker = markers[-1]
            allowed = {marker, marker + "-answer"}
            if marker.startswith("concurrency-tool-"):
                allowed.add(marker + "-answer")
            if marker == "concurrency-desktop":
                allowed.add(marker + "-answer")
            if marker in ("concurrency-D2", "concurrency-E2"):
                allowed.update({marker[:-1], marker[:-1] + "-answer"})
            assert set(markers) <= allowed, f"another conversation's history leaked: {set(markers) - allowed}"
            with lock:
                calls.append(marker)
                if args.affinity_only or args.turn_settings_only or args.mixed_models_only or args.group_continuation_only or marker.startswith("concurrency-tool-") or marker == "concurrency-desktop":
                    tool_wire.append({"marker": marker, "body": body, "session": self.headers.get("session-id") or self.headers.get("session_id")})
                started = arrived.setdefault(marker, threading.Event())
                gate = release.setdefault(marker, threading.Event())
            started.set()
            assert gate.wait(90), "synthetic response was never released"
            item = {"type": "message", "id": "msg_" + uuid.uuid4().hex, "role": "assistant", "status": "completed",
                    "content": [{"type": "output_text", "text": marker + "-answer", "annotations": []}]}
            if args.turn_settings_only:
                item["content"][0]["text"] = json.dumps({"value": marker + "-answer"})
            if marker == "concurrency-desktop":
                item["phase"] = "final_answer"
                item["internal_chat_message_metadata_passthrough"] = {"turn_id": str(uuid.uuid4())}
            outputs = [v for v in body.get("input", []) if v.get("type") == "function_call_output"]
            if marker.startswith("concurrency-tool-") and not outputs:
                item = {"type": "function_call", "id": "fc_" + marker, "call_id": marker,
                        "namespace": "functions", "name": "client_echo", "arguments": json.dumps({"marker": marker})}
            items = [item]
            if marker == "concurrency-tool-A" and not outputs:
                items.append({**item, "id": item["id"] + "_parallel", "call_id": marker + "_parallel"})
            elif marker == "concurrency-tool-A" and len(outputs) == 2:
                items = [{"type": "function_call", "id": "fc_custom", "call_id": "custom_call",
                          "namespace": "test_ns", "name": "client_script", "arguments": json.dumps({"input": "real script"})}]
            if marker.startswith("concurrency-tool-"):
                for value in items:
                    value["internal_chat_message_metadata_passthrough"] = {"turn_id": str(uuid.uuid4())}
            if marker == "concurrency-desktop" and args.desktop_case != "text":
                items = response_items(args.desktop_case, calls.count(marker), item, body)
            if args.desktop_first_reasoning and marker == "concurrency-desktop" and calls.count(marker) == 1:
                items.insert(0, {"type": "reasoning", "id": "rs_" + uuid.uuid4().hex,
                    "summary": [], "encrypted_content": "synthetic-reasoning-state"})
            events = [{"type": "response.output_item.done", "output_index": index, "item": value}
                      for index, value in enumerate(items)]
            events.append({"type": "response.completed", "response": {"id": "resp_" + uuid.uuid4().hex,
                       "status": "completed", "output": items, "usage": {"input_tokens": 10, "output_tokens": 2, "total_tokens": 12,
                       "input_tokens_details": {"cached_tokens": 8 if outputs else 0}}}})
            try:
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                self.wfile.write("".join("data: " + json.dumps(event) + "\n\n" for event in events).encode())
            except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                pass  # Cancellation closes only this synthetic request's connection.

    ThreadingHTTPServer.request_queue_size = 128
    fixture = ThreadingHTTPServer(("127.0.0.1", 0), Model)
    threading.Thread(target=fixture.serve_forever, daemon=True).start()
    model_port = fixture.server_port
    (home / "config.toml").write_text(
        f'model="gpt-6.1-sol"\nmodel_provider="synthetic"\n'
        + (f'model_auto_compact_token_limit=1000\nmodel_auto_compact_token_limit_scope="total"\n'
           f'model_post_turn_compact_threshold_percent={0 if args.compaction_position == "before" else 100}\n'
           if args.compaction_only else '')
        + ('model_reasoning_effort="medium"\nservice_tier="priority"\n' if args.desktop_case == "settings_reset" else '') +
        ('model_reasoning_effort="low"\nmodel_reasoning_summary="concise"\nmodel_supports_reasoning_summaries=true\n' if args.turn_settings_only else '') +
        f'[model_providers.synthetic]\nname="Synthetic"\nbase_url="http://127.0.0.1:{model_port}/v1"\n'
        'wire_api="responses"\nenv_key="SYNTHETIC_RELAY_KEY"\n'
        + ('capabilities.remote_compaction="v2"\nrequest_max_retries=0\nstream_max_retries=0\n'
           if args.compaction_only else '') +
        ('capabilities.reasoning_summaries=true\n' if args.turn_settings_only else '') +
        '[features]\nenable_request_compression=false\n', encoding="utf-8")
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    env = {k: v for k, v in os.environ.items() if not k.upper().startswith(("CODEX_", "OPENAI_", "CHATGPT_"))}
    env.update(CODEX_HOME=str(home), SYNTHETIC_RELAY_KEY="SYNTHETIC_CONCURRENCY_ONLY", NO_PROXY="127.0.0.1,localhost")
    if args.thread_reclamation_only:
        env.update(BRIDGE_NATIVE_MAX_THREADS="3", BRIDGE_NATIVE_CONTEXT_BUDGET_MIB="32")
    log = (work / "gateway.log").open("wb")
    process = subprocess.Popen([str(executable), "serve", "--port", str(port), "--web-dir", "web"], cwd=work,
                               env=env, stdout=log, stderr=log, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))

    def request(path, body=None, key=None, streaming=False):
        headers = {"Content-Type": "application/json"}
        if key:
            headers["Authorization"] = "Bearer " + key
        req = urllib.request.Request(base + path, data=None if body is None else json.dumps(body).encode(), headers=headers)
        response = urllib.request.build_opener(urllib.request.ProxyHandler({})).open(req, timeout=90)
        if streaming:
            return response
        with response:
            return json.load(response)

    def wait(predicate, timeout=40):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if predicate():
                return
            time.sleep(.05)
        raise AssertionError("timed out waiting for fixture state")

    def seen(marker):
        with lock:
            return marker in calls

    def unblock(marker):
        with lock:
            release.setdefault(marker, threading.Event()).set()

    def infer(marker, key, history=None, model="gpt-6.1-sol"):
        return request("/v1/responses", {"model": model, "input": history or marker, "stream": False}, key)

    pool = concurrent.futures.ThreadPoolExecutor(max_workers=4)
    report = {"real_model_calls": 0, "binary": str(binary), "evidence": str(work),
              "native_cache_reuse": os.environ.get("BRIDGE_NATIVE_CACHE_REUSE", "1") != "0"}
    try:
        for _ in range(150):
            try:
                request("/healthz")
                break
            except (OSError, urllib.error.URLError):
                time.sleep(.1)
        account = request("/api/accounts", {"name": "parallel-native", "nativeConfig": True})
        account_id = account["id"]
        ops = request("/api/operations")
        try:
            request(f"/api/accounts/{account_id}/route", {"revision": ops["revision"], "enabled": True})
        except urllib.error.HTTPError as error:
            error.add_note(error.read(4096).decode("utf-8", errors="replace"))
            raise
        ops = request("/api/operations")
        request(f"/api/accounts/{account_id}/policy", {"revision": ops["revision"], "enabled": True,
                "groupIds": [GROUP], "maxConcurrency": 2})
        a = request("/api/api-keys", {"name": "user-a", "groupId": GROUP})["secret"]
        b = request("/api/api-keys", {"name": "user-b", "groupId": GROUP})["secret"]
        if args.group_continuation_only:
            for marker in ["concurrency-desktop", "concurrency-tool-C", "concurrency-tool-D"]:
                unblock(marker)
            result = subprocess.run(["node", str(ROOT / "scripts/test_native_cache_websocket.mjs"),
                base, a, "group-change", account_id], capture_output=True, text=True, encoding="utf-8", timeout=180,
                creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
            assert result.returncode == 0, result.stdout + result.stderr
            assert len(calls) == 7, "group moves or denied requests replayed a model turn"
            report["group_continuation"] = json.loads(result.stdout)
            report["synthetic_upstream_requests"] = len(calls)
            print(json.dumps(report, ensure_ascii=False))
            return
        if args.thread_reclamation_only:
            from test_native_thread_reclamation import check
            report["thread_reclamation"] = check(work, base, a, b, request, tool_wire, account_id)
            report["synthetic_upstream_requests"] = len(tool_wire)
            print(json.dumps(report, ensure_ascii=False))
            return
        if args.explicit_compact_only:
            from test_native_explicit_compaction import check
            report["explicit_compaction"] = check(work, base, a, b, request, tool_wire, account_id)
            report["synthetic_upstream_requests"] = len(tool_wire)
            print(json.dumps(report, ensure_ascii=False))
            return
        if args.turn_settings_only:
            from test_native_turn_settings import check
            unblock("concurrency-desktop")
            unblock("concurrency-tool-C")
            report["turn_settings"] = check(work, base, a, request, tool_wire)
            report["synthetic_upstream_requests"] = len(calls)
            print(json.dumps(report, ensure_ascii=False))
            return
        if args.compaction_only:
            from test_native_compaction_reuse import check
            report["compaction"] = check(work, base, a, b, request, tool_wire, args.compaction_position)
            report["synthetic_upstream_requests"] = len(tool_wire)
            print(json.dumps(report, ensure_ascii=False))
            return
        if args.affinity_only:
            from test_native_affinity import check_affinity
            report["affinity"] = check_affinity(work, base, a, request, pool, wait, seen, unblock, tool_wire, account_id)
            report["synthetic_upstream_requests"] = len(calls)
            print(json.dumps(report, ensure_ascii=False))
            return
        if args.desktop_cache_only:
            from test_native_cache_desktop import check_desktop
            unblock("concurrency-desktop")
            report["desktop_cache"] = check_desktop(work, base, a, request, tool_wire,
                                                  first_reasoning=args.desktop_first_reasoning, case=args.desktop_case)
            report["synthetic_upstream_requests"] = len(calls)
            print(json.dumps(report, ensure_ascii=False))
            return
        standby_pid = None
        def standby_events():
            return [json.loads(line) for line in (work / "gateway.log").read_text(encoding="utf-8").splitlines()
                    if '"event":"native_standby_ready"' in line]
        if args.check_standby:
            ops = request("/api/operations")
            request(f"/api/accounts/{account_id}/activation", {"revision": ops["revision"], "enabled": True})
            wait(lambda: bool(standby_events()), timeout=60)
            standby_pid = standby_events()[-1]["nativeProcessId"]
            assert not calls, "prewarming must not submit a model request"
            print("standby ready; checking reuse after 125 seconds idle", flush=True)
            time.sleep(125)
            assert standby_events()[-1]["nativeProcessId"] == standby_pid
        first = pool.submit(infer, "concurrency-A", a)
        sibling_model = "gpt-6-luna" if args.mixed_models_only else "gpt-6.1-sol"
        second = pool.submit(infer, "concurrency-B", b, model=sibling_model)
        wait(lambda: seen("concurrency-A") and seen("concurrency-B"))
        assert request("/api/operations")["active"] == 2
        third = pool.submit(infer, "concurrency-C", a)
        wait(lambda: request("/api/operations")["queued"] == 1)
        assert not seen("concurrency-C")
        unblock("concurrency-A")
        answer_a = first.result(timeout=40)
        assert "concurrency-A-answer" in json.dumps(answer_a)
        wait(lambda: seen("concurrency-C"))
        assert not second.done(), "unrelated conversation must remain active"
        unblock("concurrency-B")
        unblock("concurrency-C")
        assert "concurrency-B-answer" in json.dumps(second.result(timeout=40))
        assert "concurrency-C-answer" in json.dumps(third.result(timeout=40))
        report["two_users_and_overflow_queue"] = "passed"

        # 同一密钥的两个独立对话也必须重叠到达上游，不能仅按密钥串行化。
        one = pool.submit(infer, "concurrency-D", a)
        two = pool.submit(infer, "concurrency-E", a)
        wait(lambda: seen("concurrency-D") and seen("concurrency-E"))
        unblock("concurrency-D")
        unblock("concurrency-E")
        answer_d, answer_e = one.result(timeout=40), two.result(timeout=40)
        report["one_user_two_conversations"] = "passed"
        # 完整历史续聊应找回各自的空闲线程；不同对话不能互相替换上下文。
        history_d = [{"role": "user", "content": "concurrency-D"}, *answer_d["output"], {"role": "user", "content": "concurrency-D2"}]
        history_e = [{"role": "user", "content": "concurrency-E"}, *answer_e["output"], {"role": "user", "content": "concurrency-E2"}]
        one = pool.submit(infer, "concurrency-D2", a, history_d)
        two = pool.submit(infer, "concurrency-E2", a, history_e)
        wait(lambda: seen("concurrency-D2") and seen("concurrency-E2"))
        unblock("concurrency-D2")
        unblock("concurrency-E2")
        assert "concurrency-D2-answer" in json.dumps(one.result(timeout=40))
        assert "concurrency-E2-answer" in json.dumps(two.result(timeout=40))
        report["independent_continuations"] = "passed"
        preflights = [json.loads(line) for line in (work / "gateway.log").read_text(encoding="utf-8").splitlines()
                      if '"event":"request_preflight"' in line]
        assert sum(event.get("reusedNativeThread", False) for event in preflights) >= 2
        report["two_cached_threads_reused"] = "passed"
        pids = {event["nativeProcessId"] for event in preflights}
        assert len(pids) == 1 and None not in pids, "compatible concurrent conversations must share one process"
        if args.check_standby:
            assert pids == {standby_pid}, "first inference must reuse the prewarmed host after idle"
            assert all(event["reusedNativeProcess"] for event in preflights)
            report["prewarmed_host_survives_125s_idle"] = "passed"
        report["shared_process_for_concurrent_threads"] = "passed"
        report["thread_history_isolation"] = "passed"

        stream = request("/v1/responses", {"model": "gpt-6.1-sol", "input": "concurrency-F", "stream": True}, a, True)
        other = pool.submit(infer, "concurrency-G", b, model=sibling_model)
        wait(lambda: seen("concurrency-F") and seen("concurrency-G"))
        stream.close()
        wait(lambda: request("/api/operations")["active"] == 1)
        assert not other.done()
        next_request = pool.submit(infer, "concurrency-H", a)
        wait(lambda: seen("concurrency-H"))
        unblock("concurrency-G")
        unblock("concurrency-H")
        assert "concurrency-G-answer" in json.dumps(other.result(timeout=40))
        next_request.result(timeout=40)
        report["single_disconnect_isolated"] = "passed"
        preflights = [json.loads(line) for line in (work / "gateway.log").read_text(encoding="utf-8").splitlines()
                      if '"event":"request_preflight"' in line]
        assert {event["nativeProcessId"] for event in preflights} == pids, "one cancellation must not restart the shared host"
        report["shared_host_survives_single_cancellation"] = "passed"

        if args.mixed_models_only:
            with lock:
                assert all(entry["body"]["model"] == "gpt-6-luna"
                           for entry in tool_wire if entry["marker"] in ("concurrency-B", "concurrency-G"))
                assert all(entry["body"]["model"] == "gpt-6.1-sol"
                           for entry in tool_wire if entry["marker"] not in ("concurrency-B", "concurrency-G"))
            report["concurrent_model_isolation"] = "passed"
            report["synthetic_upstream_requests"] = len(calls)
            unblock("concurrency-F")
            print(json.dumps(report, ensure_ascii=False))
            return

        # Retained ephemeral contexts are bounded; rotate at request boundaries.
        # All replies are synthetic, and the cancelled fixture request is released.
        unblock("concurrency-F")
        draining = pool.submit(infer, "concurrency-rotation-sibling", b)
        wait(lambda: seen("concurrency-rotation-sibling"))
        for index in range(34):
            marker = f"concurrency-rotation-{index}"
            unblock(marker)
            assert marker + "-answer" in json.dumps(infer(marker, a))
        assert not draining.done(), "retirement must wait for the old generation's active thread"
        unblock("concurrency-rotation-sibling")
        assert "concurrency-rotation-sibling-answer" in json.dumps(draining.result(timeout=40))
        preflights = [json.loads(line) for line in (work / "gateway.log").read_text(encoding="utf-8").splitlines()
                      if '"event":"request_preflight"' in line]
        assert len({event["nativeProcessId"] for event in preflights}) == (1 if report["native_cache_reuse"] else 2), "shared contexts must use the selected reclamation policy"
        report["bounded_context_lifecycle"] = "passed"
        report["rotation_preserves_inflight_sibling"] = "passed"

        tools = [{"type": "function", "name": "client_echo", "parameters": {"type": "object"}},
                 {"type": "namespace", "name": "test_ns", "description": "Synthetic tools", "tools": [{"type": "custom", "name": "client_script", "format": {"type": "text"}}]}]
        def tool_request(marker, key, history=None):
            replay = copy.deepcopy(history) if history else marker
            if isinstance(replay, list):
                for item in replay:
                    item.pop("internal_chat_message_metadata_passthrough", None)
            return request("/v1/responses", {"model": "gpt-6.1-sol", "input": replay, "tools": tools,
                "client_metadata": {"x-codex-turn-metadata": json.dumps({"root_turn_id": str(uuid.uuid4())})}}, key)
        tool_a = pool.submit(tool_request, "concurrency-tool-A", a)
        tool_b = pool.submit(tool_request, "concurrency-tool-B", b)
        def tools_started():
            for future in (tool_a, tool_b):
                if future.done():
                    try:
                        future.result()
                    except urllib.error.HTTPError as error:
                        raise AssertionError(error.read().decode()) from error
            return seen("concurrency-tool-A") and seen("concurrency-tool-B")
        wait(tools_started)
        unblock("concurrency-tool-A")
        first_tool = tool_a.result(timeout=40)
        assert any(item.get("call_id") == "concurrency-tool-A" for item in first_tool["output"])
        assert not tool_b.done(), "ending one tool request must not interrupt another thread"
        unblock("concurrency-tool-B")
        first_tool_b = tool_b.result(timeout=40)
        assert any(item.get("call_id") == "concurrency-tool-B" for item in first_tool_b["output"])
        report["concurrent_tool_callbacks_isolated"] = "passed"

        wait(lambda: request("/api/operations")["active"] == 0)
        history = [{"role": "user", "content": "concurrency-tool-A"}, *first_tool["output"]]
        for item in first_tool["output"]:
            if item.get("type") == "function_call":
                history.append({"type": "function_call_output", "call_id": item["call_id"], "output": "Error: actual tool result"})
        with lock:
            release["concurrency-tool-A"].clear()
        continuing = pool.submit(tool_request, "concurrency-tool-A", a, history)
        wait(lambda: sum(r["marker"] == "concurrency-tool-A" for r in tool_wire) == 2)
        if os.environ.get("BRIDGE_NATIVE_CACHE_REUSE", "1") != "0":
            try:
                tool_request("concurrency-tool-A", a, history)
                raise AssertionError("a competing continuation must return HTTP 409")
            except urllib.error.HTTPError as error:
                assert error.code == 409 and "RESPONSE_CONTINUATION_BUSY" in error.read().decode()
            report["competing_tool_continuation_returns_409"] = "passed"
        unblock("concurrency-tool-A")
        second_tool = continuing.result(timeout=40)
        custom = next(item for item in second_tool["output"] if item["type"] == "custom_tool_call")
        assert custom["namespace"] == "test_ns"
        history.extend(second_tool["output"])
        history.append({"type": "custom_tool_call_output", "call_id": custom["call_id"], "output": [
            {"type": "input_text", "text": "CLIENT_IMAGE_RESULT"},
            {"type": "input_image", "image_url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFUlEQVR4nGNgYPhPIhrVMKph2GoAAJLb/wFh5Z4RAAAAAElFTkSuQmCC"}]})
        tool_answer = tool_request("concurrency-tool-A", a, history)
        assert "concurrency-tool-A-answer" in json.dumps(tool_answer)
        assert first_tool["usage"]["input_tokens"] == second_tool["usage"]["input_tokens"] == tool_answer["usage"]["input_tokens"] == 10
        assert tool_answer["usage"]["input_tokens_details"]["cached_tokens"] == 8
        with lock:
            wire = [r for r in tool_wire if r["marker"] == "concurrency-tool-A"]
        assert len(wire) == 3, "one model call per client request; no automatic replay"
        if os.environ.get("BRIDGE_NATIVE_CACHE_REUSE", "1") != "0":
            assert len({r["body"]["prompt_cache_key"] for r in wire}) == 1
            assert len({r["session"] for r in wire}) == 1 and wire[0]["session"]
            for before, after in zip(wire, wire[1:]):
                prefix = before["body"]["input"]
                assert after["body"]["input"][:len(prefix)] == prefix, "model-visible prefix changed"
            assert "aborted" not in json.dumps(wire)
            report["stable_native_cache_identity_and_prefix"] = "passed"
        report["parallel_tools_custom_namespace_image"] = "passed"
        report["tool_wait_releases_admission_and_usage_is_per_request"] = "passed"

        history.extend(tool_answer["output"])
        history.append({"role": "user", "content": "concurrency-tool-A"})
        assert "concurrency-tool-A-answer" in json.dumps(tool_request("concurrency-tool-A", a, history))
        if os.environ.get("BRIDGE_NATIVE_CACHE_REUSE", "1") != "0":
            with lock:
                latest = [r for r in tool_wire if r["marker"] == "concurrency-tool-A"]
            assert len(latest) == 4
            assert latest[-1]["body"]["prompt_cache_key"] == wire[0]["body"]["prompt_cache_key"]
            report["completed_tool_chain_accepts_next_user_on_same_thread"] = "passed"

        if args.check_cache_expiry:
            print("checking actual five-minute tool expiry and full-history rebuilding", flush=True)
            time.sleep(302)
            wait(lambda: request("/api/operations")["active"] == 0)
            b_history = [{"role": "user", "content": "concurrency-tool-B"}, *first_tool_b["output"],
                         {"type": "function_call_output", "call_id": "concurrency-tool-B", "output": "late result"}]
            assert "concurrency-tool-B-answer" in json.dumps(tool_request("concurrency-tool-B", b, b_history))
            with lock:
                b_wire = [r for r in tool_wire if r["marker"] == "concurrency-tool-B"]
            assert len(b_wire) == 2
            assert b_wire[0]["body"]["prompt_cache_key"] != b_wire[1]["body"]["prompt_cache_key"]
            report["expired_tool_wait_rebuilds_once_from_complete_history"] = "passed"

        unblock("concurrency-tool-C")
        subprocess.run(["node", str(ROOT / "scripts/test_native_cache_websocket.mjs"), base, a], cwd=ROOT,
                       check=True, timeout=130, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        with lock:
            ws_wire = [r for r in tool_wire if r["marker"] == "concurrency-tool-C"]
        assert len(ws_wire) == 3
        assert ws_wire[2]["body"]["reasoning"]["effort"] == "high"
        assert ws_wire[2]["body"]["service_tier"] == "priority"
        if report["native_cache_reuse"]:
            assert len({r["body"]["prompt_cache_key"] for r in ws_wire}) == 1
            assert len({r["session"] for r in ws_wire}) == 1 and ws_wire[0]["session"]
            for before, after in zip(ws_wire, ws_wire[1:]):
                prefix = before["body"]["input"]
                assert after["body"]["input"][:len(prefix)] == prefix
        report["responses_websocket_previous_response_tool_chain"] = "passed"
        report["responses_websocket_next_turn_parameters"] = "passed"

        revoked = request("/api/api-keys", {"name": "tool-wait-revocation", "groupId": GROUP})
        unblock("concurrency-tool-D")
        revoked_first = tool_request("concurrency-tool-D", revoked["secret"])
        request(f"/api/api-keys/{revoked['key']['id']}", {"revision": revoked["key"]["revision"], "state": "revoked"})
        revoked_history = [{"role": "user", "content": "concurrency-tool-D"}, *revoked_first["output"],
                           {"type": "function_call_output", "call_id": "concurrency-tool-D", "output": "must not be dispatched"}]
        try:
            tool_request("concurrency-tool-D", revoked["secret"], revoked_history)
            raise AssertionError("revoked key resumed a retained native callback")
        except urllib.error.HTTPError as error:
            assert error.code in (401, 403)
        wait(lambda: request("/api/operations")["active"] == 0)
        with lock:
            assert sum(marker == "concurrency-tool-D" for marker in calls) == 1
        report["revoked_key_cannot_resume_pending_tool"] = "passed"

        stop_a = pool.submit(infer, "concurrency-I", a)
        stop_b = pool.submit(infer, "concurrency-J", b)
        wait(lambda: seen("concurrency-I") and seen("concurrency-J"))
        preflights = [json.loads(line) for line in (work / "gateway.log").read_text(encoding="utf-8").splitlines()
                      if '"event":"request_preflight"' in line]
        expected_hosts = 3 if args.check_cache_expiry else 2
        assert len({event["nativeProcessId"] for event in preflights}) <= expected_hosts, "concurrent preparation must not duplicate a compatible host"
        report["compatible_host_startup_is_single_flight"] = "passed"
        request(f"/api/accounts/{account_id}/stop", {})
        wait(lambda: request("/api/operations")["active"] == 0)
        for future in (stop_a, stop_b):
            try:
                stopped = future.result(timeout=10)
                # If a terminal native event beats the HTTP cancellation path,
                # Responses can return its failed/incomplete terminal document.
                assert stopped.get("status") in ("failed", "incomplete"), f"stopped request unexpectedly completed: {stopped}"
            except urllib.error.HTTPError:
                pass
        report["account_stop_all_requests"] = "passed"
        unblock("concurrency-I")
        unblock("concurrency-J")
        if args.check_standby:
            ops = request("/api/operations")
            request(f"/api/accounts/{account_id}/activation", {"revision": ops["revision"], "enabled": False})
            count = len(standby_events())
            print("account disabled; checking standby stays stopped", flush=True)
            time.sleep(35)
            assert len(standby_events()) == count, "disabled account must not respawn"
            report["disabled_account_does_not_respawn"] = "passed"
        if args.capacity:
            print("checking native capacity: 10 accounts x 10 concurrent requests", flush=True)
            account_ids = [account_id]
            for index in range(9):
                extra = request("/api/accounts", {"name": f"capacity-{index}", "nativeConfig": True})["id"]
                ops = request("/api/operations")
                request(f"/api/accounts/{extra}/route", {"revision": ops["revision"], "enabled": True})
                account_ids.append(extra)
            for identifier in account_ids:
                ops = request("/api/operations")
                request(f"/api/accounts/{identifier}/policy", {"revision": ops["revision"], "enabled": True,
                        "groupIds": [GROUP], "maxConcurrency": 10})
            markers = [f"concurrency-rotation-{1000 + index}" for index in range(100)]
            with concurrent.futures.ThreadPoolExecutor(max_workers=100) as load_pool:
                jobs = [load_pool.submit(infer, marker, a) for marker in markers]
                try:
                    wait(lambda: all(seen(marker) for marker in markers), timeout=75)
                    snapshot = request("/api/operations")
                    assert snapshot["active"] == 100, snapshot["active"]
                    assert all(account["active"] == 10 for account in snapshot["accounts"] if account["id"] in account_ids)
                finally:
                    for marker in markers:
                        unblock(marker)
                for marker, job in zip(markers, jobs):
                    assert marker + "-answer" in json.dumps(job.result(timeout=45))
            wait(lambda: request("/api/operations")["active"] == 0)
            report["ten_native_accounts_100_simultaneous_requests"] = "passed"
        report["synthetic_upstream_requests"] = len(calls)
        print(json.dumps(report, ensure_ascii=False))
    finally:
        with lock:
            for gate in release.values():
                gate.set()
        if process.poll() is None:
            try:
                request("/api/service/stop", {})
                process.wait(timeout=20)
            except (OSError, urllib.error.URLError, subprocess.TimeoutExpired):
                process.kill()
                process.wait(timeout=10)
        pool.shutdown(wait=True, cancel_futures=True)
        fixture.shutdown()
        log.close()
        # Only synthetic fixture bodies and session IDs; never live credentials.
        (work / "tool-wire.json").write_text(json.dumps(tool_wire, indent=2, ensure_ascii=False), encoding="utf-8")
        (work / "result.json").write_text(json.dumps(report, indent=2, ensure_ascii=False), encoding="utf-8")


if __name__ == "__main__":
    main()
