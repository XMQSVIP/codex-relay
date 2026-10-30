"""Isolated, synthetic native/HTTP/official OpenAI SDK integration.

Requires a built debug executable, prepared native installation, openai/httpx,
and --model-host set to a private IPv4 address already assigned to this machine.
No production state, credentials or external model service are used.
"""
import argparse
import http.server
import ipaddress
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import gzip
from contextlib import closing
import zstandard
import websocket
import httpx
from openai import OpenAI, NotFoundError, APIStatusError
from responses_native_client import native_turn

ROOT = Path(__file__).resolve().parent.parent
seen = []
CLIENT_IMAGE_CODE = 'const result = await tools.image_gen__imagegen({prompt: "blue whale"});\ngeneratedImage(result);\n'
CLIENT_IMAGE = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFUlEQVR4nGNgYPhPIhrVMKph2GoAAJLb/wFh5Z4RAAAAAElFTkSuQmCC'


def full_history(prompt, response, *extra):
    return [{"role": "user", "content": prompt},
            *(item.model_dump(exclude_none=True) for item in response.output), *extra]


class Model(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        raw = self.rfile.read(int(self.headers["Content-Length"]))
        if self.headers.get("Content-Encoding") == "zstd":
            raw = zstandard.ZstdDecompressor().decompress(raw, max_output_size=32*1024*1024)
        elif self.headers.get("Content-Encoding") == "gzip":
            raw = gzip.decompress(raw)
        body = json.loads(raw)
        assert self.headers["Authorization"] == "Bearer SYNTHETIC_SDK_ONLY"
        seen.append(body)
        history = body["input"]
        # Real upstream validates IDs against the converted item type. A permissive
        # fixture used to hide custom/tool-search IDs leaking into function history.
        for index, entry in enumerate(history):
            if (entry.get("type") in ("function_call", "function_call_output")
                    and entry.get("id") is not None and not entry["id"].startswith("fc")):
                message = f"Invalid 'input[{index}].id': '{entry['id']}'. Expected an ID that begins with 'fc'."
                print(message, flush=True)
                data = json.dumps({"error":{"message":message,"type":"invalid_request_error",
                                            "param":f"input[{index}].id","code":"invalid_value"}}).encode()
                self.send_response(400)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
                return
        if "STEER_HOLD" in str(history) and "STEER_ADDED" not in str(history):
            time.sleep(2)
        if "REVOKE_HOLD" in str(history):
            time.sleep(8)
        wants_tool = any("TOOL_ROUNDTRIP" in str(item) for item in history)
        result = next((item for item in history if item.get("type") == "function_call_output" and item.get("call_id") == "sdk_call"), None)
        events = []
        custom_result = next((item for item in history if item.get("type") == "function_call_output" and item.get("call_id") == "client_image_call" and not item.get("name")), None)
        if "UNDECLARED_SCOPE_FIXTURE" in str(history):
            item = {"id":"fc_rejected", "type":"function_call", "namespace":"client", "name":"undeclared_fixture", "call_id":"rejected_call", "arguments":'{"private":"DO_NOT_ECHO_SCOPE_ARGUMENTS"}'}
        elif "CUSTOM_IMAGE_TOOL" in str(history) and custom_result is None:
            namespace = next(tool["name"] for entry in reversed(history) if entry.get("type") == "additional_tools"
                             for tool in entry["tools"] if tool.get("type") == "namespace" and tool.get("name", "").startswith("relay_")
                             and any(member.get("name") == "exec" for member in tool.get("tools", [])))
            if "ORIGINAL_CUSTOM_NAME" in str(history):
                namespace = "functions"
            item = {"id":"ctc_desktop_exec", "type":"custom_tool_call", "namespace":namespace, "name":"exec", "call_id":"client_image_call", "input":CLIENT_IMAGE_CODE}
            if "OMITTED_CUSTOM_NAMESPACE" in str(history):
                del item["namespace"]
        elif "Call client.bridge_check once" in str(history) and not any(item.get("call_id")=="probe_call" and item.get("type")=="function_call_output" for item in history):
            item = {"id":"fc_probe", "type":"function_call", "namespace":"client", "name":"bridge_check", "call_id":"probe_call", "arguments":"{}"}
        elif history and history[-1].get("type") == "compaction_trigger":
            item = {"type": "compaction", "id": f"cmp_fixture_{len(seen)}", "encrypted_content": f"SYNTHETIC_COMPACTION_{len(seen)}"}
        elif wants_tool and result is None:
            item = {"id": "fc_sdk", "type": "function_call", "call_id": "sdk_call", "name": "client_echo", "arguments": '{"text":"hello"}'}
            namespaces = [tool["name"] for entry in history if entry.get("type")=="additional_tools" for tool in entry["tools"]
                          if tool.get("type")=="namespace" and any(member.get("name")=="client_echo" for member in tool.get("tools",[]))]
            if namespaces:
                item["namespace"] = namespaces[-1]
            if "ORIGINAL_FUNCTION_NAME" in str(history):
                item["namespace"] = "functions"
            if "MALFORMED_ARGUMENT_REPLAY" in str(history):
                item["arguments"] = "{broken"
        else:
            if "CUSTOM_IMAGE_TOOL" in str(history):
                assert custom_result["output"][0]["text"] == "CLIENT_GENERATED_IMAGE"
                assert custom_result["output"][1]["image_url"] == CLIENT_IMAGE
            if wants_tool:
                assert "CLIENT_EXECUTED_RESULT" in str(result["output"])
                if "BARE_NAMESPACE_REPLAY" in str(history):
                    replayed = next(i for i in history if i.get("call_id") == "sdk_call" and i.get("type") == "function_call")
                    assert "namespace" not in replayed and "namespace" not in result
                if "NAMESPACE_ONLY_RESULT" in str(history):
                    replayed = next(i for i in history if i.get("call_id") == "sdk_call" and i.get("type") == "function_call")
                    assert replayed["namespace"].startswith("relay_") and result["namespace"] == replayed["namespace"]
                    assert "name" not in result
                if "MALFORMED_ARGUMENT_REPLAY" in str(history):
                    replayed = next(i for i in history if i.get("call_id") == "sdk_call" and i.get("type") == "function_call")
                    assert replayed["arguments"] == "{broken"
            message_id = f"msg_{len(seen)}"
            events += [
                {"type": "response.output_item.added", "output_index": 0, "item": {"id": message_id, "type": "message", "role": "assistant", "content": [], "status": "in_progress"}},
                {"type": "response.content_part.added", "item_id": message_id, "output_index": 0, "content_index": 0, "part": {"type": "output_text", "text": "", "annotations": []}},
                {"type": "response.output_text.delta", "item_id": message_id, "output_index": 0, "content_index": 0, "delta": "SDK "},
                {"type": "response.output_text.delta", "item_id": message_id, "output_index": 0, "content_index": 0, "delta": "verified"},
            ]
            item = {"id": message_id, "type": "message", "role": "assistant", "status": "completed", "content": [{"type": "output_text", "text": "SDK verified", "annotations": []}]}
        items = [item]
        if "REASONING_REPLAY" in str(history):
            items.append({"type":"reasoning", "id":f"rs_{len(seen)}", "summary":[],
                          "encrypted_content":"SYNTHETIC_REASONING_REPLAY"})
        if "PARALLEL_TOOLS" in str(history) and item["type"]=="function_call":
            items.append({**item,"id":"fc_parallel","call_id":"parallel_call","arguments":'{"text":"parallel"}'})
        events.extend({"type":"response.output_item.done","output_index":index,"item":value} for index,value in enumerate(items))
        events.append({"type":"response.completed","response":{"id":f"resp_upstream_{len(seen)}","status":"completed","output":items,"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}})
        data = "".join(f"event: {event['type']}\ndata: {json.dumps(event)}\n\n" for event in events).encode()
        try:
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass  # Expected when native cancellation closes an in-flight model request.


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--model-host", required=True)
    parser.add_argument("--relay-binary", type=Path, default=ROOT / "target/debug/codex-relay.exe")
    parser.add_argument("--compact-only", action="store_true", help="verify SDK explicit compaction with synthetic upstream")
    args = parser.parse_args()
    assert ipaddress.ip_address(args.model_host).is_private
    model = http.server.ThreadingHTTPServer((args.model_host, 0), Model)
    threading.Thread(target=model.serve_forever, daemon=True).start()
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    with tempfile.TemporaryDirectory(prefix="bridge-sdk-test-") as temporary:
        root = Path(temporary)
        (root / ".cache").mkdir()
        (root / "web").mkdir()
        (root / "home").mkdir()
        (root / "web/index.html").write_text("Synthetic test", encoding="utf-8")
        shutil.copyfile(ROOT / ".cache/launch-config.json", root / ".cache/launch-config.json")
        # Windows locks running executables; leave the Cargo build output free.
        shutil.copyfile(args.relay_binary, root / "codex-relay.exe")
        (root / "home/config.toml").write_text(f'''
model="gpt-6.1-sol"
model_provider="synthetic"
[features]
enable_request_compression=false
[model_providers.synthetic]
name="Synthetic"
base_url="http://{args.model_host}:{model.server_port}/v1"
wire_api="responses"
requires_openai_auth=false
supports_websockets=false
request_max_retries=0
stream_max_retries=0
experimental_bearer_token="SYNTHETIC_SDK_ONLY"
[model_providers.synthetic.capabilities]
remote_compaction="v2"
''', encoding="utf-8")
        env = {k: v for k, v in os.environ.items() if not k.upper().startswith(("CODEX_", "OPENAI_", "CHATGPT_", "ANTHROPIC_", "AZURE_OPENAI_"))}
        env["CODEX_HOME"] = str(root / "home")
        env["NO_PROXY"] = f"127.0.0.1,localhost,{args.model_host}"
        with (root / "service.log").open("wb") as log:
            proc = subprocess.Popen([str(root / "codex-relay.exe"), "serve", "--port", str(port), "--web-dir", str(root / "web")], cwd=root, env=env, stdout=log, stderr=log, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
            base = f"http://127.0.0.1:{port}"
            admin = httpx.Client(base_url=base, trust_env=False, timeout=90)
            try:
                for _ in range(100):
                    if proc.poll() is not None:
                        raise RuntimeError("Synthetic bridge exited before startup")
                    try:
                        if admin.get("/healthz").status_code == 200:
                            break
                    except httpx.ConnectError:
                        pass
                    time.sleep(0.1)

                def post(path, body):
                    response = admin.post(path, json=body)
                    response.raise_for_status()
                    return response.json()

                account = post("/api/accounts", {"name": "SDK fixture", "nativeConfig": True})
                revision = admin.get("/api/operations").json()["revision"]
                post(f"/api/accounts/{account['id']}/route", {"revision": revision, "enabled": True})
                key = post("/api/api-keys", {"name": "SDK caller", "groupId": "00000000-0000-0000-0000-000000000000"})["secret"]
                sdk = OpenAI(base_url=base + "/v1", api_key=key, max_retries=0, timeout=90, http_client=httpx.Client(trust_env=False))
                if args.compact_only:
                    compacted = sdk.responses.compact(model="gpt-6.1-sol", input="SYNTHETIC_NO_DISK_HISTORY")
                    assert compacted.object == "response.compaction"
                    assert compacted.output[0].encrypted_content == "SYNTHETIC_COMPACTION_1"
                    assert len(seen) == 1
                    assert admin.get("/api/operations").json()["active"] == 0
                    sdk.close()
                    model.shutdown()
                    model.server_close()
                    print("PASS: official SDK compact returns real item; 1 synthetic request; admission released", flush=True)
                    return
                for stream_options in [{}, {"stream": False}, {"stream": None}]:
                    plain = sdk.responses.create(model="gpt-6.1-sol", input="Hello", store=False,
                                                 max_output_tokens=1, **stream_options)
                    assert plain.output_text == "SDK verified" and plain.usage.total_tokens == 12
                    assert plain.max_output_tokens is None and plain.usage.output_tokens == 2
                    assert "max_output_tokens" not in seen[-1]
                try:
                    sdk.responses.retrieve(plain.id)
                    raise AssertionError("store=false response was saved")
                except NotFoundError:
                    pass
                print("SDK JSON for omitted/false/null stream and store=false passed", flush=True)

                with sdk.responses.stream(model="gpt-6.1-sol", input="Streaming", store=False, max_output_tokens=1) as stream:
                    chunks = [event.delta for event in stream if event.type == "response.output_text.delta"]
                    final = stream.get_final_response()
                assert "".join(chunks) == final.output_text == "SDK verified"
                assert final.max_output_tokens is None and final.usage.output_tokens == 2
                assert "max_output_tokens" not in seen[-1]
                print("SDK JSON/SSE ignore max_output_tokens without truncation or upstream forwarding", flush=True)
                metadata=sdk.responses.create(model="gpt-6.1-sol",input="Metadata mapping",store=False,extra_body={
                    "access_programs":{"cyber":"daybreak_blue"},
                    "client_metadata":{"ignored_secret":"NOT_FOR_UPSTREAM","x-codex-turn-metadata":json.dumps({"custom_tag":"sdk-test","session_id":"spoofed"})}})
                assert metadata.output_text=="SDK verified"
                # Official native policy only sends cyber programs for its
                # OpenAI provider; this fixture deliberately uses another ID.
                assert "access_programs" not in seen[-1]
                turn_metadata=json.loads(seen[-1]["client_metadata"]["x-codex-turn-metadata"])
                assert turn_metadata["custom_tag"]=="sdk-test" and turn_metadata.get("session_id")!="spoofed"
                assert "NOT_FOR_UPSTREAM" not in str(seen[-1])
                print("Native provider-specific access program policy and allowed turn metadata passed",flush=True)

                original_tools = [{"type":"namespace", "name":"functions", "description":"Caller system tools", "tools":[{"type":"function", "name":"client_echo", "parameters":{"type":"object"}}]}]
                for streaming in [False, True]:
                    prompt = "TOOL_ROUNDTRIP ORIGINAL_FUNCTION_NAME"
                    if streaming:
                        events = list(sdk.responses.create(model="gpt-6.1-sol", input=prompt, tools=original_tools, stream=True))
                        assert events[-1].type == "response.completed", events[-1]
                        original = events[-1].response
                    else:
                        original = sdk.responses.create(model="gpt-6.1-sol", input=prompt, tools=original_tools)
                    assert original.status == "completed", original.error
                    assert (original.output[0].namespace, original.output[0].name) == ("functions", "client_echo")
                    output = {"type":"function_call_output", "call_id":"sdk_call", "output":"CLIENT_EXECUTED_RESULT"}
                    resumed = sdk.responses.create(model="gpt-6.1-sol", tools=original_tools, input=full_history(prompt, original, output))
                    assert resumed.output_text == "SDK verified"
                print("Original function names preserved through alias registration and client-result replay in JSON/SSE", flush=True)
                namespace_result = {"type":"function_call_output", "call_id":"sdk_call", "namespace":"functions", "output":"CLIENT_EXECUTED_RESULT"}
                resumed = sdk.responses.create(model="gpt-6.1-sol", tools=original_tools,
                    input=full_history("TOOL_ROUNDTRIP NAMESPACE_ONLY_RESULT", original, namespace_result))
                assert resumed.output_text == "SDK verified"

                def scope_failure(response):
                    assert response.status == "failed", response.model_dump()
                    assert response.error.code == "NATIVE_TOOL_SCOPE"
                    message = response.error.message
                    assert "未注册的客户端函数" in message and "namespace=client, name=undeclared_fixture" in message, message
                    assert "DO_NOT_ECHO_SCOPE_ARGUMENTS" not in message

                rejected = sdk.responses.create(model="gpt-6.1-sol", input="UNDECLARED_SCOPE_FIXTURE")
                scope_failure(rejected)
                rejected_events = list(sdk.responses.create(model="gpt-6.1-sol", input="UNDECLARED_SCOPE_FIXTURE", stream=True))
                terminals = [event for event in rejected_events if event.type in ("response.completed", "response.failed", "response.incomplete")]
                assert len(terminals) == 1
                scope_failure(terminals[0].response)
                with closing(websocket.create_connection(base.replace('http:', 'ws:') + '/v1/responses',
                        header=[f'Authorization: Bearer {key}'], timeout=90, suppress_origin=True, http_proxy_host=None)) as ws:
                    ws.send(json.dumps({"type":"response.create", "model":"gpt-6.1-sol", "input":"UNDECLARED_SCOPE_FIXTURE"}))
                    while True:
                        event = json.loads(ws.recv())
                        if event["type"] in ("error", "response.failed", "response.completed"):
                            assert event["type"] == "response.failed", event
                            assert event["response"]["error"]["message"] == rejected.error.message
                            break
                    ws.send(json.dumps({"type":"response.create", "model":"gpt-6.1-sol", "input":"Continue after scope failure"}))
                    while True:
                        event = json.loads(ws.recv())
                        if event["type"] in ("error", "response.failed", "response.completed"):
                            assert event["type"] == "response.completed", event
                            break
                    ws.send(json.dumps({"type":"response.create", "model":"gpt-6.1-sol", "input":"TOOL_ROUNDTRIP ORIGINAL_FUNCTION_NAME", "tools":original_tools}))
                    while True:
                        event = json.loads(ws.recv())
                        if event["type"] in ("error", "response.failed", "response.completed"):
                            assert event["type"] == "response.completed", event
                            tool_response = event["response"]
                            assert tool_response["output"][0]["namespace"] == "functions"
                            break
                    ws.send(json.dumps({"type":"response.create", "model":"gpt-6.1-sol", "previous_response_id":tool_response["id"], "input":[namespace_result], "tools":original_tools}))
                    while True:
                        event = json.loads(ws.recv())
                        if event["type"] in ("error", "response.failed", "response.completed"):
                            assert event["type"] == "response.completed", event
                            break
                print("JSON/SSE/WebSocket preserve tool-scope reason and identity without arguments; same connection recovers", flush=True)
                scope_records = [json.loads(line) for line in (root / "service.log").read_text(encoding="utf-8").splitlines()
                                 if line.startswith('{') and '"event":"native_tool_scope_rejected"' in line]
                assert len(scope_records) >= 3 and all(record["requestId"] and record["message"] == rejected.error.message for record in scope_records)
                assert all("DO_NOT_ECHO_SCOPE_ARGUMENTS" not in str(record) for record in scope_records)

                image_tools = [{"type":"namespace", "name":"functions", "description":"Client code tools",
                                "tools":[{"type":"custom", "name":"exec", "format":{"type":"text"}}]}]
                image_output = {"type":"custom_tool_call_output", "id":"ctco_01a17cae-5132-78f1-bf02-38b2cb758c0d", "call_id":"client_image_call", "output":[
                    {"type":"input_text", "text":"CLIENT_GENERATED_IMAGE"}, {"type":"input_image", "image_url":CLIENT_IMAGE}]}
                for streaming, prompt in [(streaming, "CUSTOM_IMAGE_TOOL" + suffix)
                                          for streaming in [False, True]
                                          for suffix in ["", " ORIGINAL_CUSTOM_NAME", " OMITTED_CUSTOM_NAMESPACE"]]:
                    before = len(seen)
                    if streaming:
                        with sdk.responses.stream(model="gpt-6.1-sol", input=prompt, tools=image_tools) as stream:
                            deltas = [event.delta for event in stream if event.type == "response.custom_tool_call_input.delta"]
                            generated = stream.get_final_response()
                        assert "".join(deltas) == CLIENT_IMAGE_CODE
                    else:
                        generated = sdk.responses.create(model="gpt-6.1-sol", input=prompt, tools=image_tools)
                    assert generated.status == "completed", generated.model_dump()
                    call = generated.output[0]
                    assert (call.type, call.name, call.namespace, call.call_id, call.input) == ("custom_tool_call", "exec", "functions", "client_image_call", CLIENT_IMAGE_CODE)
                    time.sleep(0.2)
                    assert len(seen) == before + 1, "Native must yield image code to the client"
                    continued = sdk.responses.create(model="gpt-6.1-sol", input=full_history(prompt, generated, image_output), tools=image_tools)
                    assert continued.output_text == "SDK verified"
                print("SDK JSON/SSE native custom image-code calls and client image-result continuation passed", flush=True)
                generated=sdk.responses.create(model="gpt-6.1-sol",input="CUSTOM_IMAGE_TOOL",tools=image_tools)
                notification={"type":"custom_tool_call_output","id":"ctco_desktop_notify","call_id":"client_image_call","name":"exec","output":"CLIENT_PROGRESS"}
                continued=sdk.responses.create(model="gpt-6.1-sol",input=full_history("CUSTOM_IMAGE_TOOL", generated, notification, notification, image_output, notification),tools=image_tools)
                assert continued.output_text=="SDK verified"
                notifications=[i for i in seen[-1]["input"] if i.get("call_id")=="client_image_call" and i.get("output")=="CLIENT_PROGRESS"]
                assert len(notifications)==3 and all(i.get("name")=="exec" and i.get("namespace","").startswith("relay_") for i in notifications)
                print("Native injection preserves repeated code-mode progress and final media result",flush=True)

                search_tools = [{"type":"tool_search", "execution":"client"}]
                search_history = [
                    {"role":"user", "content":"Find an echo tool"},
                    {"type":"tool_search_call", "id":"tsc_desktop_search", "call_id":"search_history_call",
                     "execution":"client", "arguments":{"query":"echo"}},
                    {"type":"tool_search_output", "id":"tso_desktop_search", "call_id":"search_history_call",
                     "execution":"client", "tools":[{"type":"function", "name":"echo", "parameters":{"type":"object"}}]},
                    {"role":"user", "content":"Continue after tool search"},
                ]
                for streaming in [False, True]:
                    if streaming:
                        with sdk.responses.stream(model="gpt-6.1-sol", input=search_history, tools=search_tools) as stream:
                            continued = stream.get_final_response()
                    else:
                        continued = sdk.responses.create(model="gpt-6.1-sol", input=search_history, tools=search_tools)
                    assert continued.output_text == "SDK verified", continued.error
                    searched = [item for item in seen[-1]["input"] if item.get("call_id") == "search_history_call"]
                    assert [item["type"] for item in searched] == ["function_call", "function_call_output"]
                    assert all(item.get("id") is None or item["id"].startswith("fc") for item in searched)
                print("Custom and tool-search typed IDs adapt to native function history with strict upstream validation", flush=True)

                large_catalog=[{"type":"function","name":f"tool_{i}","parameters":{"type":"object"}} for i in range(256)]
                large_catalog[0]["defer_loading"]=True
                assert sdk.responses.create(model="gpt-6.1-sol",input="Large tool catalog",tools=large_catalog,store=False).output_text=="SDK verified"
                assert "tool_255" in str(seen[-1]["input"])
                large_catalog[0]["name"]="client_echo"
                deferred=sdk.responses.create(model="gpt-6.1-sol",input="TOOL_ROUNDTRIP deferred",tools=large_catalog)
                deferred_call=next(item for item in deferred.output if item.type=="function_call")
                assert deferred_call.name=="client_echo" and not getattr(deferred_call,"namespace",None)
                deferred_result=sdk.responses.create(model="gpt-6.1-sol",input=full_history("TOOL_ROUNDTRIP deferred", deferred, {"type":"function_call_output","call_id":deferred_call.call_id,"name":"client_echo","output":"CLIENT_EXECUTED_RESULT"}),tools=large_catalog)
                assert deferred_result.output_text=="SDK verified"
                assert sdk.responses.create(model="gpt-6.1-sol",input=[{"role":"user","content":[{"type":"input_text","text":"Nine images"},*({"type":"input_image","image_url":CLIENT_IMAGE} for _ in range(9))]}],store=False).output_text=="SDK verified"
                print("256 tools with bare deferred registration and nine inline images passed",flush=True)

                with closing(websocket.create_connection(base.replace('http:', 'ws:') + '/v1/responses',
                        header=[f'Authorization: Bearer {key}'], timeout=90, suppress_origin=True, http_proxy_host=None)) as ws:
                    before = len(seen)
                    ws.send(json.dumps({"type":"response.create", "model":"gpt-6.1-sol", "input":"CUSTOM_IMAGE_TOOL OMITTED_CUSTOM_NAMESPACE", "tools":image_tools, "store":False}))
                    image_events = []
                    while True:
                        event = json.loads(ws.recv())
                        assert event["type"] not in ("error", "response.failed"), event
                        image_events.append(event)
                        if event["type"] == "response.completed":
                            generated = event["response"]
                            break
                    assert "".join(e["delta"] for e in image_events if e["type"] == "response.custom_tool_call_input.delta") == CLIENT_IMAGE_CODE
                    assert generated["output"][0]["namespace"] == "functions"
                    time.sleep(0.2)
                    assert len(seen) == before + 1
                    ws.send(json.dumps({"type":"response.create", "model":"gpt-6.1-sol", "input":[image_output], "tools":image_tools, "previous_response_id":generated["id"], "store":False}))
                    while True:
                        event = json.loads(ws.recv())
                        assert event["type"] not in ("error", "response.failed"), event
                        if event["type"] == "response.completed":
                            assert event["response"]["output"][0]["content"][0]["text"] == "SDK verified"
                            break
                print("Responses WebSocket custom image-code calls and client image-result continuation passed", flush=True)

                tools = [{"type": "function", "name": "client_echo", "description": "Client echo", "parameters": {"type": "object", "properties": {"text": {"type": "string"}}}, "strict": False}]
                first = sdk.responses.create(stream=False, model="gpt-6.1-sol", input="TOOL_ROUNDTRIP", tools=tools)
                call = first.output[0]
                assert call.type == "function_call" and json.loads(call.arguments) == {"text": "hello"}
                count = len(seen)
                assert first.store is False
                try:
                    sdk.responses.retrieve(first.id)
                    raise AssertionError("Default request saved a response body")
                except NotFoundError:
                    pass
                time.sleep(0.2)
                assert len(seen) == count, "Model continued before client tool execution"
                output = {"type": "function_call_output", "call_id": call.call_id, "output": "CLIENT_EXECUTED_RESULT"}
                second = sdk.responses.create(stream=False, model="gpt-6.1-sol", input=full_history("TOOL_ROUNDTRIP", first, output), tools=tools)
                assert second.output_text == "SDK verified"
                history = [{"role": "user", "content": "TOOL_ROUNDTRIP"}, *(item.model_dump(exclude_none=True) for item in first.output), output]
                third = sdk.responses.create(stream=False, model="gpt-6.1-sol", input=history, tools=tools, store=False)
                assert third.output_text == "SDK verified"
                for response in [first, second]:
                    try:
                        sdk.responses.delete(response.id)
                        raise AssertionError("Stateless response was deletable")
                    except NotFoundError:
                        pass
                try:
                    sdk.responses.create(model="gpt-6.1-sol", previous_response_id=first.id, input="missing")
                    raise AssertionError("HTTP continuation found a stored response")
                except NotFoundError:
                    pass
                print("SDK client tools/full replay passed; HTTP retrieve/delete/ID continuation return 404", flush=True)
                for named_result in [False, True]:
                    bare_call = first.output[0].model_dump(exclude_none=True)
                    bare_call["namespace"] = "functions"
                    bare_result = {"type":"function_call_output", "call_id":"sdk_call", "namespace":"functions", "output":"CLIENT_EXECUTED_RESULT"}
                    if named_result:
                        bare_result["name"] = "client_echo"
                    restored = sdk.responses.create(model="gpt-6.1-sol", tools=tools, input=[
                        {"role":"user", "content":"TOOL_ROUNDTRIP BARE_NAMESPACE_REPLAY"}, bare_call, bare_result])
                    assert restored.output_text == "SDK verified"
                print("Default functions namespace and namespace-only results replay without identity drift", flush=True)
                malformed = sdk.responses.create(model="gpt-6.1-sol", input="TOOL_ROUNDTRIP MALFORMED_ARGUMENT_REPLAY", tools=tools)
                assert malformed.status == "completed" and malformed.output[0].arguments == "{broken"
                recovered = sdk.responses.create(model="gpt-6.1-sol", tools=tools, input=full_history(
                    "TOOL_ROUNDTRIP MALFORMED_ARGUMENT_REPLAY", malformed,
                    {"type":"function_call_output", "call_id":"sdk_call", "output":"CLIENT_EXECUTED_RESULT: arguments could not be parsed; nothing executed"}))
                assert recovered.output_text == "SDK verified"
                print("Malformed native arguments are preserved for client error reporting and continued generation", flush=True)
                parallel=sdk.responses.create(model="gpt-6.1-sol",input="TOOL_ROUNDTRIP PARALLEL_TOOLS",tools=tools)
                assert [item.call_id for item in parallel.output]==["sdk_call","parallel_call"]
                followed=sdk.responses.create(model="gpt-6.1-sol",tools=tools,
                    input=full_history("TOOL_ROUNDTRIP PARALLEL_TOOLS", parallel, *({"type":"function_call_output","call_id":item.call_id,"output":"CLIENT_EXECUTED_RESULT"} for item in parallel.output)))
                assert followed.output_text=="SDK verified"
                print("Parallel tool identities and complete result roundtrip passed",flush=True)
                for stream_mode in [False, True]:
                    if stream_mode:
                        with sdk.responses.stream(model="gpt-6.1-sol", input="Hello", temperature=0.5,
                                                  top_p=0.8, background=True, store=False) as stream:
                            ignored = stream.get_final_response()
                    else:
                        ignored = sdk.responses.create(stream=False, model="gpt-6.1-sol", input="Hello", temperature=0.5,
                                                       top_p=0.8, background=True, store=False)
                    assert ignored.output_text == "SDK verified"
                    assert ignored.background is False
                    assert ignored.temperature is None and ignored.top_p is None
                    assert 'temperature' not in seen[-1] and 'top_p' not in seen[-1]
                print("SDK JSON/SSE ignore unsupported optional parameters", flush=True)
                raw_client = httpx.Client(base_url=base, headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"}, trust_env=False, timeout=90)
                for encoding, compress in [("gzip", gzip.compress), ("zstd", zstandard.ZstdCompressor().compress)]:
                    compressed = raw_client.post("/v1/responses", headers={"Content-Encoding": encoding}, content=compress(json.dumps({"model": "gpt-6.1-sol", "input": "Compressed client", "store": False}).encode()))
                    assert compressed.status_code == 200, compressed.text
                    assert compressed.json()["status"] == "completed"
                print("Raw HTTP gzip/zstd passed", flush=True)
                before = len(seen)
                compacted = sdk.responses.compact(model="gpt-6.1-sol", input="Do not persist this history")
                assert compacted.output[0].type == "compaction"
                trigger = raw_client.post("/v1/responses", json={"model":"gpt-6.1-sol", "input":[
                    {"role":"user","content":"Compact"},{"type":"compaction_trigger"}]})
                assert trigger.status_code == 200, trigger.text
                assert trigger.json()["output"][0]["type"] == "compaction"
                assert len(seen) == before + 2
                print("SDK explicit compact and compaction_trigger passed", flush=True)
                ws_url = base.replace("http://", "ws://") + "/v1/responses"
                def connect(bearer=key):
                    return websocket.create_connection(ws_url, header=[f"Authorization: Bearer {bearer}"], suppress_origin=True, timeout=90, http_no_proxy=["127.0.0.1"])
                def receive(ws, lane=None):
                    events = []
                    while True:
                        event = json.loads(ws.recv()); events.append(event)
                        assert event.get("stream_id") == lane, event
                        if event["type"] in ("response.completed", "response.failed", "error"):
                            assert event["type"] == "response.completed", event
                            return event["response"]
                with closing(connect()) as ws:
                    count = len(seen)
                    ws.send(json.dumps({"type":"response.create","model":"gpt-6.1-sol","input":"Warm input","generate":False,"store":False,"stream_id":"coding"}))
                    warm=receive(ws,"coding")
                    assert len(seen)==count
                    ws.send(json.dumps({"type":"response.create","previous_response_id":warm["id"],"input":"Delta input","store":False,"stream_id":"coding"}))
                    created=receive(ws,"coding")
                    assert "Warm input" in str(seen[-1]["input"]) and "Delta input" in str(seen[-1]["input"])
                    ws.send(json.dumps({"type":"response.create","model":"gpt-6.1-sol","previous_response_id":created["id"],"input":"Next turn","store":False}))
                    continued=receive(ws)
                    assert raw_client.get(f"/v1/responses/{continued['id']}").status_code==404
                with closing(connect()) as ws:
                    ws.send(json.dumps({"type":"response.create","model":"gpt-6.1-sol","previous_response_id":continued["id"],"input":"Reconnect","store":False}))
                    assert json.loads(ws.recv())["code"]=="previous_response_not_found"
                    ws.send(json.dumps({"type":"response.create","model":"gpt-6.1-sol","input":[{"role":"user","content":"Full replay"},*continued["output"],{"role":"user","content":"Reconnect"}],"store":False}))
                    receive(ws)
                print("Responses WebSocket prewarm, incremental history, store=false, reconnect/replay passed", flush=True)
                with closing(connect()) as ws:
                    before=len(seen)
                    ws.send(json.dumps({"type":"response.create","model":"gpt-6.1-sol","input":"STEER_HOLD","store":False}))
                    first=json.loads(ws.recv()); assert first["type"]=="response.created",first
                    ws.send(json.dumps({"type":"response.steer","response_id":first["response"]["id"],"input":"STEER_ADDED"}))
                    steered=receive(ws)
                    assert steered["status"]=="completed"
                    assert any("STEER_ADDED" in str(value["input"]) for value in seen[before:]), "Steering never reached native model history"
                print("In-flight text steering reaches turn/steer and native model history",flush=True)
                with closing(connect()) as ws:
                    ws.send(json.dumps({"type":"response.create","model":"gpt-6.1-sol","input":"STEER_HOLD interrupt target","store":False,"stream_id":"cancel-me"}))
                    first=json.loads(ws.recv()); assert first["type"]=="response.created",first
                    interrupted_id=first["response"]["id"]
                    interrupt={"type":"response.interrupt","response_id":interrupted_id,"mode":"discard_partial_items"}
                    ws.send(json.dumps({**interrupt,"stream_id":"wrong"}))
                    while True:
                        event=json.loads(ws.recv())
                        if event["type"]=="response.interrupt.failed":break
                        assert event["type"] not in ("error","response.failed","response.completed"),event
                    ws.send(json.dumps({"type":"response.create","model":"gpt-6.1-sol","input":"Unaffected stream","store":False,"stream_id":"keep-me"}))
                    for _ in range(2):ws.send(json.dumps(interrupt))
                    acknowledgements=0;terminals={}
                    while acknowledgements<2 or len(terminals)<2:
                        event=json.loads(ws.recv())
                        assert event["type"] not in ("error","response.failed","response.interrupt.failed"),event
                        if event["type"]=="response.interrupt.accepted":
                            assert event["response_id"]==interrupted_id
                            acknowledgements+=1
                        if event["type"] in ("response.completed","response.incomplete"):
                            lane=event["stream_id"]
                            assert lane not in terminals,"Duplicate terminal"
                            terminals[lane]=event
                    stopped=terminals["cancel-me"]["response"]
                    assert stopped["status"]=="incomplete" and stopped["incomplete_details"]["reason"]=="interrupted"
                    assert stopped["output"]==[] and stopped["usage"] is None
                    assert terminals["keep-me"]["response"]["status"]=="completed"
                    ws.send(json.dumps(interrupt))
                    assert json.loads(ws.recv())["type"]=="response.interrupt.accepted"
                    ws.send(json.dumps({"type":"response.interrupt","response_id":terminals["keep-me"]["response"]["id"],"mode":"discard_partial_items"}))
                    assert json.loads(ws.recv())["type"]=="response.interrupt.failed"
                    ws.send(json.dumps({"type":"response.create","model":"gpt-6.1-sol","input":"After interruption","store":False}))
                    assert receive(ws)["status"]=="completed"
                print("Scoped/repeated interruption, unknown usage, completion race and same-connection recovery passed",flush=True)
                with closing(connect()) as ws:
                    for lane,step in [("a","1"),("a","2"),("b","1")]:
                        ws.send(json.dumps({"type":"response.create","model":"gpt-6.1-sol","input":"Lane test","store":False,"stream_id":lane,"metadata":{"step":step}}))
                    done=[]
                    while len(done)<3:
                        event=json.loads(ws.recv())
                        assert event["type"] not in ("error","response.failed"),event
                        if event["type"]=="response.completed":
                            done.append((event["stream_id"],event["response"]["metadata"]["step"]))
                    assert [step for lane,step in done if lane=="a"]==["1","2"]
                print("Concurrent stream_id lanes preserve per-lane FIFO",flush=True)
                js_module = ROOT / ".cache/responses-sdk/node_modules/openai/index.mjs"
                assert js_module.is_file(), "Install test dependency: npm install --prefix .cache/responses-sdk openai"
                js_env = {**env, "BRIDGE_TEST_URL": base+"/v1", "BRIDGE_TEST_KEY": key, "BRIDGE_TEST_OPENAI_JS": str(js_module)}
                js=subprocess.run(["node", str(ROOT / "scripts/test_responses_js.mjs")], env=js_env, check=True, timeout=120,
                               capture_output=True,text=True,creationflags=getattr(subprocess,"CREATE_NO_WINDOW",0))
                print(js.stdout.strip(),flush=True)
                installation=json.loads((ROOT / ".cache/launch-config.json").read_text(encoding="utf-8-sig"))
                policy = next(a for a in admin.get("/api/operations").json()["accounts"] if a["id"] == account["id"])
                assert policy["disabledFeatures"] == []
                assert set(policy["features"]) == {"namespacedTools", "imageInput", "webSearch", "imageGeneration"}
                # Both SDK and desktop paths must work without a paid capability probe or manual enablement.
                before=len(seen)
                direct=native_turn(root,installation,f"http://{args.model_host}:{model.server_port}/v1","SYNTHETIC_SDK_ONLY","desktop-direct")
                direct_requests=seen[before:]
                before=len(seen)
                relayed=native_turn(root,installation,base+"/v1",key,"desktop-relayed")
                relayed_requests=seen[before:]
                assert "".join(part.get("text","") for part in direct[-1]["content"])=="".join(part.get("text","") for part in relayed[-1]["content"])
                assert len(direct_requests)==len(relayed_requests)==2
                for requests in [direct_requests,relayed_requests]:
                    assert "DESKTOP_RPC_BASE" in str(requests[0]) and "DESKTOP_RPC_DEVELOPER" in str(requests[0])
                    assert "client_echo" in str(requests[0])
                    assert "CLIENT_EXECUTED_RESULT" in str(requests[1]["input"])
                print("Desktop RPC -> official app-server -> relay -> second official app-server -> model matches direct coding roundtrip",flush=True)
                before=len(seen)
                desktop_ws=native_turn(root,installation,base+"/v1",key,"desktop-websocket",use_ws=True)
                assert "".join(part.get("text","") for part in desktop_ws[-1]["content"])=="SDK verified"
                assert len(seen[before:])==2
                print("Official desktop app-server Responses WebSocket coding roundtrip passed",flush=True)
                before=len(seen)
                steered=native_turn(root,installation,base+"/v1",key,"desktop-websocket-steer",use_ws=True,
                    steer_when=lambda: any("STEER_HOLD" in str(r["input"]) for r in seen[before:]))
                assert "".join(part.get("text","") for part in steered[-1]["content"])=="SDK verified"
                assert any("STEER_ADDED" in str(r["input"]) for r in seen[before:])
                print("Official turn/steer -> response.interrupt -> second native cancellation -> continued response passed",flush=True)
                native_config=root/"home/config.toml"
                original_config=native_config.read_text(encoding="utf-8")
                try:
                    native_config.write_text(original_config.replace('remote_compaction="v2"','remote_compaction="unsupported"'),encoding="utf-8")
                    local_compact=raw_client.post("/v1/responses/compact",json={"model":"gpt-6.1-sol","input":"Retain this native plaintext summary"})
                    assert local_compact.status_code==200,local_compact.text
                    assert local_compact.json()["output"][0]["type"]=="compaction"
                finally:
                    native_config.write_text(original_config,encoding="utf-8")
                print("Explicit compaction is independent of native RPC capability toggle",flush=True)
                revoked=post("/api/api-keys",{"name":"Revocation fixture","groupId":"00000000-0000-0000-0000-000000000000"})
                with closing(connect(revoked["secret"])) as ws:
                    ws.settimeout(15)
                    ws.send(json.dumps({"type":"response.create","model":"gpt-6.1-sol","input":"REVOKE_HOLD","store":False}))
                    assert json.loads(ws.recv())["type"]=="response.created"
                    for _ in range(100):
                        if "REVOKE_HOLD" in str(seen[-1]["input"]): break
                        time.sleep(0.02)
                    assert "REVOKE_HOLD" in str(seen[-1]["input"])
                    response=admin.request("DELETE",f"/api/api-keys/{revoked['key']['id']}",json={"revision":revoked["key"]["revision"]})
                    response.raise_for_status()
                    while True:
                        try:
                            frame=ws.recv()
                            if not frame: break
                            assert json.loads(frame)["type"]!="response.completed", "Revoked request completed successfully"
                        except websocket.WebSocketConnectionClosedException:
                            break
                assert admin.get("/api/operations").json()["active"]==0
                print("Revoking a key cancels its in-flight native request and closes its WebSocket",flush=True)
                raw_client.close()
                assert admin.get("/api/operations").json()["active"] == 0
                with sdk.responses.stream(model="gpt-6.1-sol",input="Disconnect test",store=False) as stream:
                    for event in stream:
                        if event.type == "response.created":
                            break
                for _ in range(300):
                    if admin.get("/api/operations").json()["active"] == 0:
                        break
                    time.sleep(0.1)
                assert admin.get("/api/operations").json()["active"] == 0, "Disconnected request retained its lease"
                print("SDK disconnect releases native instance and concurrency", flush=True)
                kept=sdk.responses.create(model="gpt-6.1-sol",input="Do not persist this response",store=True)
                assert kept.store is False
                opaque=sdk.responses.create(model="gpt-6.1-sol",input="REASONING_REPLAY",store=False)
                replay=full_history("REASONING_REPLAY", opaque)
                assert any(item.get("encrypted_content") for item in replay)
                admin.post("/api/service/stop",json={}).raise_for_status()
                proc.wait(timeout=40)
                proc=subprocess.Popen([str(root / "codex-relay.exe"),"serve","--port",str(port),"--web-dir",str(root / "web")],cwd=root,env=env,stdout=log,stderr=log,creationflags=getattr(subprocess,"CREATE_NO_WINDOW",0))
                for _ in range(100):
                    assert proc.poll() is None,"Restarted relay exited"
                    try:
                        if admin.get("/healthz").status_code==200: break
                    except httpx.HTTPError: pass
                    time.sleep(0.1)
                for id in [kept.id, plain.id]:
                    try:
                        sdk.responses.retrieve(id)
                        raise AssertionError("Restart retained a response body")
                    except NotFoundError:
                        pass
                try:
                    sdk.responses.create(model="gpt-6.1-sol",previous_response_id=kept.id,input="After restart")
                    raise AssertionError("Restart retained ID-only history")
                except NotFoundError:
                    pass
                assert sdk.responses.create(model="gpt-6.1-sol",input=full_history("Do not persist this response", kept, {"role":"user","content":"After restart"})).output_text=="SDK verified"
                assert sdk.responses.create(model="gpt-6.1-sol",input=[*replay,{"role":"user","content":"After encrypted replay"}],store=False).output_text=="SDK verified"
                try:
                    sdk.responses.retrieve(plain.id)
                    raise AssertionError("Restart persisted store=false body")
                except NotFoundError: pass
                print("SDK store=true is ignored; text/encrypted full replay survives restart without stored response bodies",flush=True)
                sdk.close()
                print(f"PASS: {len(seen)} synthetic model requests; native transport retained", flush=True)
            finally:
                try:
                    admin.post("/api/service/stop", json={})
                    proc.wait(timeout=40)
                except (httpx.HTTPError, subprocess.TimeoutExpired):
                    proc.terminate()
                    proc.wait(timeout=10)
                admin.close()
    model.shutdown()


if __name__ == "__main__":
    main()
