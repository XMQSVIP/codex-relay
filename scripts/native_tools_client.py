"""Execute image/search turns with the locally installed original app-server."""
import argparse
import base64
import json
import mimetypes
import os
from pathlib import Path
import queue
import signal
import subprocess
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parent.parent
MODEL = "gpt-6.1-sol"

class Native:
    def __init__(self, installation, home, workspace, log, isolated=False, timeout=1800):
        env = dict(os.environ)
        if isolated:
            env = {k: v for k, v in env.items() if not k.upper().startswith(
                ("CODEX_", "OPENAI_", "CHATGPT_", "ANTHROPIC_", "AZURE_OPENAI_"))}
            for name in list(env):
                if name.upper() in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"):
                    del env[name]
            for name in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"):
                env[name] = ""
            env["NO_PROXY"] = "127.0.0.1,localhost,::1"
        if home is not None:
            env["CODEX_HOME"] = str(home)
        command = [installation["appServerExecutable"], "app-server", "--listen", "stdio://"]
        for override in ('history.persistence="none"', 'otel.log_user_prompt=false',
                         'otel.exporter="none"', 'otel.trace_exporter="none"',
                         'analytics.enabled=false', 'features.apps=false', 'features.plugins=false',
                         'features.hooks=false', 'features.shell_tool=false', 'features.unified_exec=false',
                         'features.multi_agent=false', 'features.browser_use=false', 'features.computer_use=false'):
            command.extend(["-c", override])
        self.process = subprocess.Popen(command,
            cwd=workspace, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0), start_new_session=os.name != "nt")
        self.messages = queue.Queue()
        self.events = []
        self.counter = 0
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()
        self.thread = None
        self.turn = None
        self.timeout = timeout

    def read(self):
        try:
            for line in self.process.stdout:
                self.messages.put(json.loads(line))
        except Exception as error:
            self.messages.put(error)
        finally:
            self.messages.put(EOFError("Official app-server closed stdout"))

    def send(self, value):
        self.process.stdin.write((json.dumps(value) + "\n").encode())
        self.process.stdin.flush()

    def next(self, timeout=90):
        if timeout <= 0:
            raise TimeoutError("Official app-server deadline exceeded; request will not be replayed")
        message = self.messages.get(timeout=timeout)
        if isinstance(message, Exception):
            raise message
        if "method" in message:
            self.events.append(message)
            if "id" in message:
                # This probe never grants shell/file permissions or handles tools itself.
                self.send({"id": message["id"], "error": {"code": -32601, "message": "Unexpected callback in isolated probe"}})
                raise AssertionError("Unexpected callback: " + message["method"])
        return message

    def rpc(self, method, params):
        self.counter += 1
        request_id = self.counter
        self.send({"id": request_id, "method": method, "params": params})
        deadline = time.monotonic() + 90
        while True:
            message = self.next(deadline - time.monotonic())
            if message.get("id") == request_id:
                if "error" in message:
                    raise AssertionError({"method": method, "error": message["error"]})
                return message["result"]

    def start(self, installation, workspace, model=MODEL, provider="native_fixture", config=None):
        self.rpc("initialize", {"clientInfo": installation["nativeClientInfo"], "capabilities": {"experimentalApi": True}})
        self.send({"method": "initialized"})
        if provider is None:
            effective = self.rpc("config/read", {"cwd": str(workspace), "includeLayers": False})["config"]
            provider = effective.get("model_provider") or "openai"
            model = model or effective.get("model")
            if not model:
                raise ValueError("No configured text model; supply --model")
            config = dict(config or {})
            for server in effective.get("mcp_servers", {}):
                if not all(c.isalnum() or c in "_-" for c in server):
                    raise ValueError("Unsupported MCP name in local configuration")
                config[f"mcp_servers.{server}.enabled"] = False
        policy = {"features.code_mode": {"enabled": False}, "features.image_generation": True,
                  "web_search": "live", "project_doc_max_bytes": 0,
                  "features.skip_host_skill_discovery": True, "tools.update_plan.enabled": False,
                  "tools.experimental_request_user_input.enabled": False}
        policy.update(config or {})
        result = self.rpc("thread/start", {"model": model, "modelProvider": provider, "cwd": str(workspace),
            "approvalPolicy": "untrusted", "sandbox": "read-only", "ephemeral": True, "experimentalRawEvents": True,
            "environments": [], "runtimeWorkspaceRoots": [], "config": policy})
        if result["model"] != model or result["modelProvider"] != provider:
            raise RuntimeError("Official app-server selected a different model/provider")
        self.thread = result["thread"]["id"]

    def begin(self, case, images=()):
        self.events.clear()
        inputs = [{"type": "text", "text": case, "text_elements": []}]
        inputs.extend({"type": "image", "url": image} for image in images)
        result = self.rpc("turn/start", {"threadId": self.thread, "input": inputs})
        self.turn = result["turn"]["id"]

    def finish(self):
        deadline = time.monotonic() + self.timeout
        while True:
            for event in self.events:
                if event.get("method") == "turn/completed" and event["params"]["turn"]["id"] == self.turn:
                    return event["params"]["turn"]
            self.next(deadline - time.monotonic())

    def close(self):
        if self.process.poll() is None:
            if os.name == "nt":
                subprocess.run(["taskkill", "/PID", str(self.process.pid), "/T", "/F"],
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False,
                    creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0), timeout=15)
            else:
                os.killpg(self.process.pid, signal.SIGTERM)
        try:
            self.process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            if os.name != "nt":
                os.killpg(self.process.pid, signal.SIGKILL)
            else:
                self.process.kill()
            self.process.wait(timeout=5)
        self.process.stdin.close()
        self.reader.join(timeout=5)
        self.process.stdout.close()


def collect(events, tool, output):
    items = [e["params"]["item"] for e in events if e.get("method") == "item/completed"]
    selected = [i for i in items if i["type"] == ("imageGeneration" if tool == "image" else "webSearch")]
    if not selected:
        raise RuntimeError("The model did not execute the requested official tool")
    result = {"tool": tool, "items": [], "assistantText": "\n".join(i.get("text", "") for i in items if i["type"] == "agentMessage")}
    for index, item in enumerate(selected):
        if tool == "image":
            if item.get("status") != "completed":
                raise RuntimeError("Official image tool failed: " + json.dumps(item.get("failure"), ensure_ascii=False))
            data = item.get("result")
            if isinstance(data, str) and data:
                image = base64.b64decode(data, validate=True)
            elif item.get("savedPath"):
                # This path is supplied by the local, trusted official process.
                image = Path(item["savedPath"]).read_bytes()
            else:
                raise RuntimeError("Official tool supplied no downloadable image")
            if len(image) > 64 * 1024 * 1024 or not image.startswith(b"\x89PNG\r\n\x1a\n"):
                raise RuntimeError("Official image must be a PNG of at most 64 MiB")
            path = output / f"image-{index + 1}.png"
            with path.open("xb") as saved:
                saved.write(image)
            result["items"].append({"path": str(path), "revisedPrompt": item.get("revisedPrompt"),
                                    "transparentBackground": item.get("transparentBackground")})
        else:
            result["items"].append(item)
    usages = [e["params"]["tokenUsage"] for e in events if e.get("method") == "thread/tokenUsage/updated"]
    result["nativeTokenUsage"] = usages[-1] if usages else None
    # Native model tokens are not the image endpoint's usage. Never invent it.
    result["imageUsage"] = None
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__ + " Real use incurs configured provider usage.")
    parser.add_argument("--installation", type=Path, default=ROOT / ".cache/launch-config.json")
    parser.add_argument("--tool", choices=("image", "search"), required=True)
    parser.add_argument("--prompt", required=True)
    parser.add_argument("--model", help="Text model that executes the tool; defaults to native config")
    parser.add_argument("--image", type=Path, action="append", default=[], help="Edit input (up to five images)")
    parser.add_argument("--output", type=Path, required=True, help="New result directory")
    parser.add_argument("--timeout", type=float, default=1800)
    args = parser.parse_args()
    if not 0 < args.timeout <= 1800 or len(args.image) > 5 or (args.image and args.tool != "image"):
        parser.error("Use at most five image inputs and a timeout between 0 and 1800 seconds")
    images = []
    for path in args.image:
        if path.stat().st_size > 8 * 1024 * 1024:
            parser.error("Input image exceeds 8 MiB")
        mime = mimetypes.guess_type(path.name)[0]
        if mime not in ("image/png", "image/jpeg", "image/webp"):
            parser.error("Input must be PNG, JPEG or WebP")
        images.append(f"data:{mime};base64," + base64.b64encode(path.read_bytes()).decode())
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    installation = json.loads(args.installation.read_text(encoding="utf-8-sig"))
    instruction = ("Use the built-in image_gen.imagegen tool exactly once for the request below. "
                   "Do not use any other tool. Do not retry an image failure. " if args.tool == "image" else
                   "Use the built-in web.run tool for the search request below. Do not use other tools. ")
    if images:
        instruction += f"Edit the {len(images)} attached image(s) using num_last_images_to_include={len(images)}. "
    with tempfile.TemporaryDirectory(prefix="official-tools-client-") as directory:
        workspace = Path(directory)
        with (workspace / "native.log").open("wb") as log:
            native = Native(installation, None, workspace, log, timeout=args.timeout)
            try:
                native.start(installation, workspace, args.model, provider=None,
                    config={"features.image_generation": args.tool == "image", "web_search": "live" if args.tool == "search" else "disabled"})
                native.begin(instruction + "\n\n" + args.prompt, images)
                turn = native.finish()
                if turn["status"] != "completed":
                    raise RuntimeError("Official turn did not complete: " + json.dumps(turn.get("error"), ensure_ascii=False))
                result = collect(native.events, args.tool, output)
                result["threadId"] = native.thread
                result["transport"] = "official-app-server-tool-turn"
                (output / "result.json").write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")
                print(json.dumps(result, ensure_ascii=False, indent=2))
            finally:
                native.close()


if __name__ == "__main__":
    main()
