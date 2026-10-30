"""Exercise the official app-server's built-in image_gen through a test relay."""
import json
import base64
from datetime import datetime, timezone
import os
from pathlib import Path
import queue
import subprocess
import threading
import time


def native_image_turn(root, installation, base_url, key, expected_image):
    home = Path(root) / "desktop-images"
    workspace = home / "workspace"
    workspace.mkdir(parents=True)
    claims = {"email":"image-fixture@example.test", "sub":"image-fixture", "exp":int(time.time())+3600,
              "https://api.openai.com/auth":{"chatgpt_account_id":"image-fixture","chatgpt_user_id":"image-fixture","chatgpt_plan_type":"pro"}}
    token = "e30." + base64.urlsafe_b64encode(json.dumps(claims).encode()).decode().rstrip('=') + ".synthetic"
    (home / "auth.json").write_text(json.dumps({"auth_mode":"chatgpt", "OPENAI_API_KEY":None,
        "tokens":{"access_token":token,"id_token":token,"refresh_token":"synthetic-image-refresh","account_id":"image-fixture"},
        "last_refresh":datetime.now(timezone.utc).isoformat()}), encoding="utf-8")
    (home / "config.toml").write_text(f'''
model="gpt-6.1-sol"
model_provider="image_fixture"
cli_auth_credentials_store="file"
chatgpt_base_url={json.dumps(base_url.rsplit('/v1', 1)[0])}
[features]
remote_models=false
secret_auth_storage=false
image_generation=true
shell_tool=false
apps=false
plugins=false
hooks=false
multi_agent=false
enable_request_compression=false
[model_providers.image_fixture]
name="OpenAI"
base_url={json.dumps(base_url)}
wire_api="responses"
requires_openai_auth=true
supports_websockets=false
request_max_retries=0
stream_max_retries=0
experimental_bearer_token={json.dumps(key)}
''', encoding="utf-8")
    env = {k:v for k,v in os.environ.items() if not k.upper().startswith(
        ("CODEX_","OPENAI_","CHATGPT_","ANTHROPIC_","AZURE_OPENAI_"))}
    env["CODEX_HOME"] = str(home)
    env["NO_PROXY"] = "*"
    messages = queue.Queue()
    with (home / "native.log").open("wb") as log:
        process = subprocess.Popen([installation["appServerExecutable"],"app-server","--listen","stdio://"],
                                   cwd=workspace,env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=log,
                                   creationflags=getattr(subprocess,"CREATE_NO_WINDOW",0))
        def read():
            for line in process.stdout:
                messages.put(json.loads(line))
        reader = threading.Thread(target=read,daemon=True)
        reader.start()
        counter = 0
        def send(value):
            process.stdin.write((json.dumps(value)+"\n").encode())
            process.stdin.flush()
        def rpc(method,params):
            nonlocal counter
            counter += 1
            send({"id":counter,"method":method,"params":params})
            while True:
                value = messages.get(timeout=90)
                if value.get("id") == counter:
                    assert "error" not in value,value.get("error")
                    return value["result"]
        try:
            rpc("initialize",{"clientInfo":installation["nativeClientInfo"],"capabilities":{"experimentalApi":True}})
            send({"method":"initialized"})
            thread = rpc("thread/start",{"model":"gpt-6.1-sol","modelProvider":"image_fixture","cwd":str(workspace),
                "approvalPolicy":"untrusted","sandbox":"read-only","ephemeral":True,"experimentalRawEvents":True,
                "environments":[],"runtimeWorkspaceRoots":[],"config":{"features.code_mode":{"enabled":False},"features.image_generation":True,"project_doc_max_bytes":0}})["thread"]["id"]
            rpc("turn/start",{"threadId":thread,"input":[{"type":"text","text":"DESKTOP_IMAGE_ROUNDTRIP","text_elements":[]}]})
            image_done = False
            while True:
                value = messages.get(timeout=90)
                method,params = value.get("method"),value.get("params",{})
                if "id" in value and method:
                    raise AssertionError(f"Unexpected callback: {method}")
                if method == "item/completed" and params.get("item",{}).get("type") == "imageGeneration":
                    item = params["item"]
                    assert item["status"] == "completed", item
                    assert Path(item["savedPath"]).read_bytes() == expected_image
                    image_done = True
                if method == "turn/completed":
                    assert params["turn"]["status"] == "completed", params["turn"]
                    assert image_done, "Desktop never completed built-in image generation"
                    return
        finally:
            process.stdin.close()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.terminate()
                process.wait(timeout=10)
            reader.join(timeout=5)
            process.stdout.close()
