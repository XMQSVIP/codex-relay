"""Multi-account acceptance using installed native processes and localhost synthetic SSE.

Never uses the user's CODEX_HOME or real model credentials. Build via the memory
wrapper first. Evidence is retained in .cache/smart-scheduling-native-<uuid>.
"""
import argparse
import concurrent.futures
from collections import Counter
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import threading
import time
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parent.parent
GROUP = "00000000-0000-0000-0000-000000000000"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/codex-relay.exe")
    args = parser.parse_args()
    work = ROOT / ".cache" / ("smart-scheduling-native-" + uuid.uuid4().hex)
    (work / ".cache").mkdir(parents=True)
    shutil.copyfile(ROOT / ".cache/launch-config.json", work / ".cache/launch-config.json")
    home = work / "synthetic-home"
    home.mkdir()
    (work / "web").mkdir()
    (work / "web/index.html").write_text("synthetic scheduling fixture", encoding="utf-8")
    accounts, calls, delays = [], [], {}
    gates, arrivals = {}, {}
    lock = threading.Lock()

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
            assert self.headers.get("Authorization") == "Bearer SYNTHETIC_SCHEDULING_ONLY"
            payload = json.dumps(body.get("input"))
            markers = re.findall(r"scheduler-[A-Za-z0-9-]+", payload)
            assert markers, payload
            marker = markers[-1]
            allowed = {marker}
            if marker in {"scheduler-next", "scheduler-held", "scheduler-overflow"}:
                allowed.add("scheduler-first")
            assert set(markers) <= allowed, "another conversation's history entered this native turn"
            # The native protocol need not expose its local workspace to the
            # upstream. Read the admitted account from the gateway journal. All
            # arrivals here are sequential (the held call arrives before overflow).
            account = record()["accountId"]
            assert account in accounts
            with lock:
                calls.append((marker, account))
                arrivals.setdefault(marker, threading.Event()).set()
                gate = gates.get(marker)
            if gate:
                assert gate.wait(60)
            time.sleep(delays.get(account, .03))
            text = "reply to " + marker
            item = {"type": "message", "id": "msg_" + uuid.uuid4().hex, "role": "assistant", "status": "completed", "content": [{"type": "output_text", "text": text, "annotations": []}]}
            events = [
                {"type": "response.output_item.added", "output_index": 0, "item": {**item, "status": "in_progress", "content": []}},
                {"type": "response.content_part.added", "output_index": 0, "content_index": 0, "item_id": item["id"], "part": {"type": "output_text", "text": "", "annotations": []}},
                {"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "item_id": item["id"], "delta": text},
                {"type": "response.output_item.done", "output_index": 0, "item": item},
                {"type": "response.completed", "response": {"id": "resp_" + uuid.uuid4().hex, "status": "completed", "output": [item], "usage": {"input_tokens": 10, "output_tokens": 2, "total_tokens": 12}}},
            ]
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            self.wfile.write("".join("data: " + json.dumps(event) + "\n\n" for event in events).encode())

    fixture = ThreadingHTTPServer(("127.0.0.1", 0), Model)
    threading.Thread(target=fixture.serve_forever, daemon=True).start()
    (home / "config.toml").write_text(
        f'model="gpt-6.1-sol"\nmodel_provider="synthetic"\n[model_providers.synthetic]\nname="Synthetic"\nbase_url="http://127.0.0.1:{fixture.server_port}/v1"\n'
        'wire_api="responses"\nenv_key="SYNTHETIC_RELAY_KEY"\nrequest_max_retries=0\nstream_max_retries=0\n[features]\nenable_request_compression=false\n', encoding="utf-8")
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    env = {k: v for k, v in os.environ.items() if not k.upper().startswith(("CODEX_", "OPENAI_", "CHATGPT_"))}
    env.update(CODEX_HOME=str(home), SYNTHETIC_RELAY_KEY="SYNTHETIC_SCHEDULING_ONLY", NO_PROXY="127.0.0.1,localhost")
    log = (work / "gateway.log").open("wb")
    process = subprocess.Popen([str(args.binary.resolve()), "serve", "--port", str(port), "--web-dir", "web"], cwd=work, env=env, stdout=log, stderr=log, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def request(path, body=None, key=None):
        headers = {"Content-Type": "application/json"}
        if key:
            headers["Authorization"] = "Bearer " + key
        req = urllib.request.Request(f"http://127.0.0.1:{port}" + path, data=None if body is None else json.dumps(body).encode(), headers=headers)
        with opener.open(req, timeout=60) as response:
            return json.load(response)

    def policy(account, enabled):
        request(f"/api/accounts/{account}/policy", {"revision": request("/api/operations")["revision"], "enabled": enabled, "groupIds": [GROUP], "maxConcurrency": 1})

    def infer(marker, history=None):
        result = request("/v1/responses", {"model": "gpt-6.1-sol", "input": history or marker}, key)
        assert result["status"] == "completed", result
        text = "".join(part.get("text", "") for item in result["output"] if item["type"] == "message" for part in item["content"])
        assert text == "reply to " + marker, result
        return result

    def record():
        rows = request("/api/statistics")["records"]
        return next(row for row in rows if row["kind"] == "inference")

    report = {"real_model_calls": 0, "evidence": str(work)}
    pool = concurrent.futures.ThreadPoolExecutor(max_workers=2)
    try:
        for _ in range(150):
            try:
                request("/healthz")
                break
            except OSError:
                time.sleep(.1)
        for name in ["scheduler-a", "scheduler-b"]:
            account = request("/api/accounts", {"name": name, "nativeConfig": True})["id"]
            accounts.append(account)
            request(f"/api/accounts/{account}/route", {"revision": request("/api/operations")["revision"], "enabled": True})
        key = request("/api/api-keys", {"name": "synthetic", "groupId": GROUP})["secret"]
        print("synthetic accounts ready", flush=True)
        initial = infer("scheduler-first")
        origin = record()["accountId"]
        public = [{"role": "user", "content": "scheduler-first"}, {"role": "assistant", "content": "reply to scheduler-first"}]
        infer("scheduler-next", public + [{"role": "user", "content": "scheduler-next"}])
        assert record()["accountId"] == origin and record()["scheduling"]["mode"] == "softAffinity"
        report["plain_text_continuation"] = "passed"
        gates["scheduler-held"] = threading.Event()
        held = pool.submit(infer, "scheduler-held", [{"role": "user", "content": "scheduler-first"}, *initial["output"], {"role": "user", "content": "scheduler-held"}])
        deadline = time.monotonic() + 30
        while "scheduler-held" not in arrivals:
            assert time.monotonic() < deadline
            time.sleep(.05)
        assert record()["scheduling"]["mode"] == "hardBinding"
        infer("scheduler-overflow", public + [{"role": "user", "content": "scheduler-overflow"}])
        assert record()["accountId"] != origin
        assert record()["scheduling"]["affinity"] == "unavailable"
        gates["scheduler-held"].set()
        held.result(timeout=30)
        report["full_account_immediate_fallback_and_strong_binding"] = "passed"
        print("continuity and capacity fallback passed", flush=True)
        for index, account in enumerate(accounts):
            for other in accounts:
                policy(other, other == account)
            for n in range(8):
                infer(f"scheduler-warm-{index}-{n}")
                assert record()["firstDeltaMs"] is not None
        for account in accounts:
            policy(account, True)
        print("latency samples ready", flush=True)
        baseline = []
        for n in range(24):
            infer(f"scheduler-baseline-{n}")
            baseline.append(record()["accountId"])
        assert set(baseline) == set(accounts)
        slow = Counter(baseline).most_common(1)[0][0]
        delays[slow] = 1.8
        degraded, modes = [], []
        for n in range(25):
            infer(f"scheduler-degraded-{n}")
            row = record()
            degraded.append(row["accountId"])
            modes.append(row["scheduling"]["mode"])
        assert degraded[-10:].count(slow) <= 3, degraded
        assert "exploration" in modes
        report.update(baseline_distribution=dict(Counter(baseline)), degraded_distribution=dict(Counter(degraded)), slowdown_reallocated="passed", periodic_exploration="passed")
        assert all(count == 1 for count in Counter(marker for marker, _ in calls).values()), calls
        assert len(calls) == 69, len(calls)
        report.update(no_duplicate_inference="passed", history_isolation="passed", synthetic_upstream_requests=len(calls))
        print(json.dumps(report, ensure_ascii=False))
    finally:
        for gate in gates.values():
            gate.set()
        if process.poll() is None:
            try:
                request("/api/service/stop", {})
                process.wait(timeout=20)
            except (OSError, subprocess.TimeoutExpired):
                process.kill()
                process.wait(timeout=10)
        pool.shutdown(wait=True, cancel_futures=True)
        fixture.shutdown()
        log.close()
        (work / "result.json").write_text(json.dumps(report, indent=2, ensure_ascii=False), encoding="utf-8")


if __name__ == "__main__":
    main()
