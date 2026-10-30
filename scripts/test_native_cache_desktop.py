"""Installed Codex client -> relay -> installed app-server -> synthetic localhost model.

Called by test_account_concurrency.py --desktop-cache-only. All homes, keys,
messages and upstreams are isolated fixtures; no real model is contacted.
"""
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import threading
import time
import urllib.error
import urllib.request

from probe_native_output_limit import INSTALL, Native
from native_cache_audit_cases import FALLBACK_CASES, TOOL_CASES


def check_desktop(work, base, key, request, model_wire, first_reasoning=False, case="text"):
    assert base.startswith("http://127.0.0.1:")
    ingress = []
    callbacks = []

    class Proxy(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            assert self.path == "/v1/responses"
            assert self.headers.get("Authorization") == "Bearer SYNTHETIC_LIMIT_PROBE"
            data = self.rfile.read(int(self.headers["Content-Length"]))
            body = json.loads(data)
            # Controlled wire variants prove native-equivalent settings also
            # match. Response/history cases use the unmodified official client.
            if case == "settings_null":
                body["reasoning"].pop("effort", None)
                body["text"].pop("verbosity", None)
                if ingress:
                    body["reasoning"].update(effort=None, summary=None)
                    body["text"]["verbosity"] = None
                    body["instructions"] = ""
            elif case == "settings_auto" and ingress:
                body["service_tier"] = "auto"
            elif case == "settings_reset":
                body["input"] = [item for item in body["input"] if item["type"] != "configuration_update"]
                body["reasoning"]["effort"] = ["low", "high", None][len(ingress)]
                body["service_tier"] = ["priority", "default", "auto"][len(ingress)]
            elif case == "tool_settings_changed" and len(ingress) == 1:
                body["reasoning"]["effort"] = "high"
            if case.startswith("identity_"):
                metadata = body.setdefault("client_metadata", {})
                if case == "identity_header_rotation":
                    metadata["thread_id"] = "logical-stable"
                elif case == "identity_thread_change":
                    metadata["thread_id"] = "logical-child" if ingress else "logical-parent"
                    if ingress:
                        metadata["parent_thread_id"] = "logical-parent"
                elif case == "identity_conflict":
                    # Put both thread IDs at the same source priority. The
                    # installed client also emits a higher-priority direct ID.
                    metadata.pop("thread_id", None)
                    turn_metadata = json.loads(metadata["x-codex-turn-metadata"])
                    turn_metadata["thread_id"] = "original"
                    metadata["x-codex-turn-metadata"] = json.dumps(turn_metadata)
                    body["turn_metadata"] = json.dumps({"thread_id": "conflicting"})
                elif case == "identity_anonymous_child":
                    turn_metadata = json.loads(metadata["x-codex-turn-metadata"])
                    for field in ("session_id", "thread_id", "parent_thread_id"):
                        metadata.pop(field, None)
                        turn_metadata.pop(field, None)
                    turn_metadata["subagent_kind"] = "thread_spawn"
                    metadata["x-codex-turn-metadata"] = json.dumps(turn_metadata)
            data = json.dumps(body).encode()
            headers = {name: value for name, value in self.headers.items()
                       if name.lower() not in ("authorization", "host", "content-length", "connection")}
            if case == "identity_header_rotation":
                headers = {name: value for name, value in headers.items()
                           if name.lower() not in ("session-id", "session_id")}
                headers["session-id"] = "cache-route-" + str(len(ingress))
            entry = {"body": body, "headers": headers}
            ingress.append(entry)
            headers["Authorization"] = "Bearer " + key
            call = urllib.request.Request(base + self.path, data=data, headers=headers)
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
            try:
                response = opener.open(call, timeout=90)
            except urllib.error.HTTPError as error:
                response = error
            with response:
                answer = response.read()
                self.send_response(response.status)
                self.send_header("Content-Type", response.headers.get("Content-Type", "application/json"))
                self.send_header("Content-Length", str(len(answer)))
                self.end_headers()
                self.wfile.write(answer)
                entry["response"] = answer.decode()

    proxy = ThreadingHTTPServer(("127.0.0.1", 0), Proxy)
    threading.Thread(target=proxy.serve_forever, daemon=True).start()
    root = work / "desktop-client"
    root.mkdir()
    class Client(Native):
        def receive(self, timeout=30):
            message = self.messages.get(timeout=timeout)
            if message.get("process_exited"):
                raise RuntimeError("Native process exited")
            if "id" in message and "method" in message:
                assert message["method"] == "item/tool/call" and case in TOOL_CASES, message
                assert message["params"]["tool"] == "unused_fixture_tool", message
                callbacks.append(message["params"]["callId"])
                content = [{"type": "inputText", "text": "Error: synthetic tool result" if case == "error_result" else "Synthetic tool result"}]
                if case == "image_result":
                    content.append({"type": "inputImage", "imageUrl": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFUlEQVR4nGNgYPhPIhrVMKph2GoAAJLb/wFh5Z4RAAAAAElFTkSuQmCC"})
                self.send({"id": message["id"], "result": {"contentItems": content, "success": True}})
            return message

    native = Client(root, proxy.server_port, enabled_features=("code_mode_host",) if case == "custom" else ())
    try:
        initialized = native.call("initialize", {"clientInfo": INSTALL["nativeClientInfo"],
            "capabilities": {"experimentalApi": True}})
        assert "result" in initialized, initialized
        native.send({"method": "initialized"})
        tools = [{"name": "unused_fixture_tool", "description": "Synthetic callback only",
                  "inputSchema": {"type": "object", "properties": {"case": {"type": "string"}}}}]
        if case == "namespace":
            tools = [{"type": "namespace", "name": "audit_ns", "description": "Synthetic namespace", "tools": [{"type": "function", **tools[0]}]}]
        started = native.call("thread/start", {"model": "gpt-6.1-sol", "modelProvider": "synthetic",
            "cwd": str(native.cwd), "ephemeral": True, "approvalPolicy": "never", "sandbox": "read-only",
            "baseInstructions": "Answer the synthetic fixture without using tools.", "developerInstructions": "",
            "dynamicTools": tools,
            "config": {"features.code_mode": {"direct_only_tool_namespaces": ["functions", "audit_ns"]}} if case in TOOL_CASES - {"custom"} else {},
            "environments": [], "runtimeWorkspaceRoots": [], "selectedCapabilityRoots": []})
        assert "result" in started, started
        thread = started["result"]["thread"]["id"]
        for index in range(3):
            overrides = {"effort": ["low", "high", "low"][index]} if case in {"turn_effort", "tool_turn_settings"} else {"effort": "low"}
            if case == "turn_model":
                overrides["model"] = ["gpt-6.1-sol", "gpt-6-luna", "gpt-6.1-sol"][index]
            if case in {"turn_tier", "tool_turn_settings"}:
                overrides["serviceTierForTurn"] = ["priority", "default", "priority"][index]
            turn = native.call("turn/start", {"threadId": thread, **overrides, "input": [
                {"type": "text", "text": "concurrency-desktop", "text_elements": []}]})
            assert "result" in turn, turn
            deadline = time.monotonic() + 90
            while time.monotonic() < deadline:
                message = native.receive(max(.1, deadline - time.monotonic()))
                if message.get("method") == "turn/completed" and message["params"]["threadId"] == thread:
                    assert message["params"]["turn"]["status"] == "completed", message
                    break
            else:
                raise TimeoutError("desktop turn")
        records = request("/api/statistics")["records"]
        records = sorted((row for row in records if row["action"] == "responses"), key=lambda row: row["startedAt"])
        preflights = [json.loads(line) for line in (work / "gateway.log").read_text(encoding="utf-8").splitlines()
                      if '"event":"request_preflight"' in line]
        evidence = {"case": case, "client_requests": len(ingress), "model_requests": len(model_wire),
                    "first_reply_contains_reasoning": first_reasoning or case.startswith("reasoning_"),
                    "callback_count": len(callbacks),
                    "native_threads": [row["nativeThreadId"] for row in records],
                    "reused_threads": [event["reusedNativeThread"] for event in preflights]}
        (work / "desktop-result.json").write_text(json.dumps(evidence, indent=2), encoding="utf-8")
        print(json.dumps(evidence), flush=True)
        expected_calls = 4 if case in TOOL_CASES else 3
        assert len(ingress) == len(model_wire) == len(records) == expected_calls, evidence
        if case == "turn_model":
            expected_models = ["gpt-6.1-sol", "gpt-6-luna", "gpt-6.1-sol"]
            assert [entry["body"]["model"] for entry in model_wire] == expected_models
            assert [row["model"] for row in records] == expected_models
            evidence["upstream_models"] = expected_models
        if case in {"turn_effort", "turn_tier", "settings_reset", "tool_turn_settings", "tool_settings_changed"}:
            efforts = [entry["body"]["reasoning"]["effort"] for entry in model_wire]
            tiers = [entry["body"].get("service_tier") for entry in model_wire]
            expected_efforts = {
                "turn_effort": ["low", "high", "low"], "turn_tier": ["low"] * 3,
                "settings_reset": ["low", "high", "medium"],
                "tool_turn_settings": ["low", "low", "high", "low"],
                "tool_settings_changed": ["low", "high", "low", "low"],
            }[case]
            expected_tiers = {
                "turn_effort": [None] * 3, "turn_tier": ["priority", None, "priority"],
                "settings_reset": ["priority", None, "priority"],
                "tool_turn_settings": ["priority", "priority", None, "priority"],
                "tool_settings_changed": [None] * 4,
            }[case]
            assert efforts == expected_efforts, (efforts, expected_efforts)
            assert tiers == expected_tiers, (tiers, expected_tiers)
            evidence.update(upstream_efforts=efforts, upstream_tiers=tiers)
        expected_callbacks = (2 if case == "parallel" else 1) if case in TOOL_CASES else 0
        assert len(callbacks) == len(set(callbacks)) == expected_callbacks, callbacks
        if case in TOOL_CASES:
            results = [item for item in model_wire[1]["body"]["input"] if item["type"] == "function_call_output"]
            assert len(results) == (2 if case == "parallel" else 1), results
            assert {item["call_id"] for item in results} == ({"audit-call", "audit-parallel"} if case == "parallel" else {"audit-call"})
            assert "Synthetic tool result" in json.dumps(results) or "Error: synthetic tool result" in json.dumps(results)
            if case == "image_result":
                assert any(part["type"] == "input_image" for item in results for part in item["output"])
            if case == "error_result":
                assert results[0]["output"] == "Error: synthetic tool result"
        if first_reasoning:
            events = [json.loads(line[6:]) for line in ingress[0]["response"].splitlines() if line.startswith("data: {")]
            output = next(event["response"]["output"] for event in events if event["type"] == "response.completed")
            original = next(item for item in output if item["type"] == "reasoning")
            replay = next(item for item in ingress[1]["body"]["input"] if item["type"] == "reasoning")
            assert "content" not in original and "content" in replay and replay["content"] is None
            assert original["encrypted_content"] == replay["encrypted_content"] == "synthetic-reasoning-state"
        metadata = [json.loads(entry["body"]["client_metadata"]["x-codex-turn-metadata"]) for entry in ingress]
        assert len({entry["root_turn_id"] for entry in metadata}) == 3, "native client must vary its turn identity"
        assert len({entry["body"]["prompt_cache_key"] for entry in ingress}) == 1
        for row, wire in zip(records, model_wire):
            cached = 8 if any(item["type"] == "function_call_output" for item in wire["body"]["input"]) else 0
            assert (row["inputTokens"], row["outputTokens"], row["cachedInputTokens"]) == (10, 2, cached)
        if case in ("identity_conflict", "identity_anonymous_child"):
            assert evidence["reused_threads"] == [False] * expected_calls, evidence
            assert len({row["nativeThreadId"] for row in records}) == expected_calls, evidence
            evidence.update(status="passed", unsafe_identity_does_not_reuse=True)
            (work / "desktop-result.json").write_text(json.dumps(evidence, indent=2), encoding="utf-8")
            return evidence
        if case in FALLBACK_CASES:
            assert evidence["reused_threads"] == [False, False] + [True] * (expected_calls - 2), evidence
            assert len({row["nativeThreadId"] for row in records}) == 2, evidence
            evidence.update(status="passed", expected_fallback=True)
            (work / "desktop-result.json").write_text(json.dumps(evidence, indent=2), encoding="utf-8")
            return evidence
        assert len({row["nativeThreadId"] for row in records}) == 1, evidence
        assert evidence["reused_threads"] == [False] + [True] * (expected_calls - 1), evidence
        assert len({entry["body"]["prompt_cache_key"] for entry in model_wire}) == 1
        assert len({entry["session"] for entry in model_wire}) == 1
        assert model_wire[0]["session"]
        for previous, current in zip(model_wire, model_wire[1:]):
            prefix = previous["body"]["input"]
            assert current["body"]["input"][:len(prefix)] == prefix, "model-visible prefix changed"
        evidence.update(status="passed", stable_cache_key=True, stable_session_header=True,
                        model_prefix_preserved=True, per_request_usage_preserved=True)
        (work / "desktop-result.json").write_text(json.dumps(evidence, indent=2), encoding="utf-8")
        return evidence
    finally:
        native.close()
        proxy.shutdown()
        proxy.server_close()
        (work / "desktop-wire.json").write_text(json.dumps(ingress, indent=2), encoding="utf-8")
