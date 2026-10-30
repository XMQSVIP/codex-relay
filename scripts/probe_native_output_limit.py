"""Check installed app-server output-limit support against a local synthetic model.

Never reads personal provider credentials or contacts a real model. The report
records parameter propagation, not a simulated enforcement of token limits.
"""
import http.server
import json
import os
from pathlib import Path
import queue
import subprocess
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parent.parent
INSTALL = json.loads((ROOT / '.cache/launch-config.json').read_text(encoding='utf-8'))
SEEN = []


class Model(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        assert self.path == '/v1/responses'
        assert self.headers['Authorization'] == 'Bearer SYNTHETIC_LIMIT_PROBE'
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        SEEN.append(body)
        item = {'type': 'message', 'id': f'm_{len(SEEN)}', 'role': 'assistant',
                'status': 'completed', 'content': [{'type': 'output_text', 'text': 'Synthetic response', 'annotations': []}]}
        events = [
            {'type': 'response.output_item.done', 'output_index': 0, 'item': item},
            {'type': 'response.completed', 'response': {'id': f'r_{len(SEEN)}', 'status': 'completed',
             'output': [item], 'usage': {'input_tokens': 10, 'output_tokens': 2, 'total_tokens': 12}}},
        ]
        data = ''.join(f'data: {json.dumps(e)}\n\n' for e in events).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)


class Native:
    def __init__(self, root, port, enabled_features=(), config_overrides=()):
        self.home = root / 'home'
        self.cwd = root / 'workspace'
        self.home.mkdir()
        self.cwd.mkdir()
        self.config = f'''model="gpt-6.1-sol"
model_provider="synthetic"
model_reasoning_effort="medium"
[features]
enable_request_compression=false
[model_providers.synthetic]
name="Synthetic"
base_url="http://127.0.0.1:{port}/v1"
wire_api="responses"
requires_openai_auth=false
supports_websockets=false
request_max_retries=0
stream_max_retries=0
experimental_bearer_token="SYNTHETIC_LIMIT_PROBE"
'''
        (self.home / 'config.toml').write_text(self.config, encoding='utf-8')
        self.env = {k: v for k, v in os.environ.items() if not k.upper().startswith(
            ('CODEX_', 'OPENAI_', 'CHATGPT_', 'ANTHROPIC_', 'AZURE_OPENAI_'))}
        self.env['CODEX_HOME'] = str(self.home)
        self.env['NO_PROXY'] = '127.0.0.1,localhost,::1'
        self.command = [INSTALL['appServerExecutable'], 'app-server', '--listen', 'stdio://']
        for feature in ['apps', 'plugins', 'hooks', 'shell_tool', 'unified_exec', 'multi_agent',
                        'browser_use', 'computer_use', 'image_generation', 'goals', 'memories',
                        'skill_search', 'workspace_dependencies', 'view_image', 'sleep_tool',
                        'tool_suggest', 'browser_use_external', 'in_app_browser', 'code_mode_host',
                        'request_permissions_tool', 'unbounded_connection_retries']:
            self.command += ['-c', f'features.{feature}={str(feature in enabled_features).lower()}']
        for override in config_overrides:
            self.command += ['-c', override]
        self.process = subprocess.Popen(self.command, cwd=self.cwd, env=self.env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            text=True, encoding='utf-8', creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
        self.messages = queue.Queue()
        self.next_id = 1
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        for line in self.process.stdout:
            self.messages.put(json.loads(line))
        self.messages.put({'process_exited': True})

    def send(self, message):
        self.process.stdin.write(json.dumps(message) + '\n')
        self.process.stdin.flush()

    def receive(self, timeout=30):
        message = self.messages.get(timeout=timeout)
        if message.get('process_exited'):
            raise RuntimeError('Native process exited')
        if 'id' in message and 'method' in message:
            self.send({'id': message['id'], 'error': {'code': -32601, 'message': 'No tool execution in probe'}})
            raise RuntimeError('Unexpected native tool callback')
        return message

    def call(self, method, params):
        identifier = self.next_id
        self.next_id += 1
        self.send({'id': identifier, 'method': method, 'params': params})
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            message = self.receive(max(0.1, deadline - time.monotonic()))
            if message.get('id') == identifier:
                return message
        raise TimeoutError(method)

    def close(self):
        self.process.stdin.close()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            subprocess.run(['taskkill.exe', '/PID', str(self.process.pid), '/T', '/F'],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True,
                creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
            self.process.wait(timeout=5)
        self.process.stdout.close()

    def probe(self, label, config, turn):
        start = self.call('thread/start', {'model': 'gpt-6.1-sol', 'modelProvider': 'synthetic',
            'cwd': str(self.cwd), 'ephemeral': True, 'approvalPolicy': 'untrusted', 'sandbox': 'read-only',
            'dynamicTools': [], 'environments': [], 'runtimeWorkspaceRoots': [],
            'selectedCapabilityRoots': [], 'experimentalRawEvents': True, 'config': config})
        if 'error' in start:
            return {'case': label, 'threadError': start['error']}
        thread = start['result']['thread']['id']
        before = len(SEEN)
        result = self.call('turn/start', {'threadId': thread, 'input': [
            {'type': 'text', 'text': 'Return the synthetic fixture response.', 'text_elements': []}], **turn})
        if 'error' in result:
            self.call('thread/unload', {'threadId': thread})
            return {'case': label, 'turnError': result['error'], 'modelRequests': len(SEEN) - before}
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            message = self.receive(max(0.1, deadline - time.monotonic()))
            if message.get('method') == 'turn/completed' and message.get('params', {}).get('threadId') == thread:
                status = message['params']['turn']['status']
                break
        else:
            raise TimeoutError(label)
        self.call('thread/unload', {'threadId': thread})
        requests = SEEN[before:]
        assert status == 'completed' and len(requests) == 1, (label, status, len(requests))
        captured = requests[0]
        return {'case': label, 'status': status, 'modelRequests': 1,
                'upstreamEffort': captured.get('reasoning', {}).get('effort'),
                'upstreamMaxOutputTokens': captured.get('max_output_tokens'),
                'upstreamHasLimitField': 'max_output_tokens' in captured}


def main():
    model = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
    threading.Thread(target=model.serve_forever, daemon=True).start()
    report = {'nativeVersion': INSTALL['appServerVersion'], 'realModelRequests': 0, 'cases': []}
    with tempfile.TemporaryDirectory(prefix='native-limit-probe-', dir=ROOT / '.cache') as temporary:
        root = Path(temporary).resolve()
        assert root.is_relative_to((ROOT / '.cache').resolve())
        native = Native(root, model.server_port)
        try:
            initialized = native.call('initialize', {'clientInfo': INSTALL['nativeClientInfo'],
                'capabilities': {'experimentalApi': True}})
            assert 'result' in initialized, initialized
            native.send({'method': 'initialized'})
            cases = [
                ('effort_control', {}, {'effort': 'high'}),
                ('turn_max_output_tokens', {}, {'max_output_tokens': 64}),
                ('turn_maxOutputTokens', {}, {'maxOutputTokens': 64}),
                ('config_max_output_tokens', {'max_output_tokens': 64}, {}),
                ('config_model_max_output_tokens', {'model_max_output_tokens': 64}, {}),
                ('provider_max_output_tokens', {'model_providers.synthetic.max_output_tokens': 64}, {}),
                ('provider_extra_body', {'model_providers.synthetic.extra_body': {'max_output_tokens': 64}}, {}),
            ]
            for label, config, turn in cases:
                result = native.probe(label, config, turn)
                report['cases'].append(result)
                print(json.dumps(result), flush=True)
        finally:
            native.close()
            model.shutdown()
            model.server_close()
    report['syntheticModelRequests'] = len(SEEN)
    report['limitReachedUpstream'] = any(c.get('upstreamHasLimitField') for c in report['cases'])
    output = ROOT / '.cache/native-output-limit-probe.json'
    output.write_text(json.dumps(report, indent=2), encoding='utf-8')
    print(json.dumps({'report': str(output), 'limitReachedUpstream': report['limitReachedUpstream']}))


if __name__ == '__main__':
    main()
