"""Verify native context-management behavior using only a synthetic local model.

Compares direct configuration with the native auto-compaction threshold. Records
whether injected history is compacted before the first request, whether a later
turn compacts, and whether the resulting compaction item reaches raw RPC events.
No personal credentials or real model requests are used.
"""
import http.server
import json
import tempfile
import threading
import time
from pathlib import Path

from probe_native_output_limit import INSTALL, Native, ROOT


class Model(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        if (self.path != '/v1/responses'
                or self.headers.get('Authorization') != 'Bearer SYNTHETIC_LIMIT_PROBE'):
            self.send_error(400)
            return
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        compact = any(item.get('type') == 'compaction_trigger' for item in body['input'])
        self.server.seen.append({
            'compactionRequest': compact,
            'contextManagement': body.get('context_management'),
            'hasCompactedInput': any(item.get('type') == 'compaction' for item in body['input']),
        })
        sequence = len(self.server.seen)
        # Fail a native retry/compaction loop instead of making the fixture unbounded.
        if sequence > 20:
            self.send_error(400, 'Synthetic request budget exceeded')
            return
        if compact:
            item = {'type': 'compaction', 'id': f'cmp_{sequence}',
                    'encrypted_content': 'SYNTHETIC_CONTEXT_COMPACTION'}
        else:
            item = {'type': 'message', 'id': f'm_{sequence}', 'role': 'assistant',
                    'status': 'completed', 'content': [{'type': 'output_text',
                        'text': 'Synthetic answer', 'annotations': []}]}
        input_tokens = 10 if compact or self.server.seen[-1]['hasCompactedInput'] else 10000
        events = [
            {'type': 'response.output_item.done', 'output_index': 0, 'item': item},
            {'type': 'response.completed', 'response': {
                'id': f'r_{sequence}', 'status': 'completed', 'output': [item],
                'usage': {'input_tokens': input_tokens, 'output_tokens': 2,
                          'total_tokens': input_tokens + 2}}},
        ]
        data = ''.join(f'data: {json.dumps(event)}\n\n' for event in events).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def finish_turn(native, thread):
    events = []
    deadline = time.monotonic() + 45
    while time.monotonic() < deadline:
        event = native.receive(max(0.1, deadline - time.monotonic()))
        if event.get('params', {}).get('threadId') != thread:
            continue
        events.append(event)
        if event.get('method') == 'turn/completed':
            assert event['params']['turn']['status'] == 'completed', event
            return events
    raise TimeoutError('Native context-management probe did not complete')


def probe(root, server, label, config):
    native = Native(root, server.server_port)
    try:
        initialized = native.call('initialize', {
            'clientInfo': INSTALL['nativeClientInfo'],
            'capabilities': {'experimentalApi': True}})
        assert 'result' in initialized, initialized
        native.send({'method': 'initialized'})
        started = native.call('thread/start', {
            'model': 'gpt-6.1-sol', 'modelProvider': 'synthetic', 'cwd': str(native.cwd),
            'ephemeral': False, 'historyMode': 'legacy', 'approvalPolicy': 'untrusted',
            'sandbox': 'read-only', 'dynamicTools': [], 'environments': [],
            'runtimeWorkspaceRoots': [], 'selectedCapabilityRoots': [],
            'baseInstructions': '', 'developerInstructions': '',
            'experimentalRawEvents': True,
            'config': {'model_providers.synthetic.capabilities.remote_compaction': 'v2', **config},
        })
        assert 'result' in started, started
        thread = started['result']['thread']['id']
        injected = native.call('thread/inject_items', {'threadId': thread, 'items': [
            {'type': 'message', 'role': 'user', 'content': [
                {'type': 'input_text', 'text': 'history content ' * 5000}]}]})
        assert 'result' in injected, injected
        before = len(server.seen)
        turn = native.call('turn/start', {'threadId': thread, 'input': []})
        assert 'result' in turn, turn
        events = finish_turn(native, thread)
        first_end = len(server.seen)
        turn = native.call('turn/start', {'threadId': thread, 'input': [
            {'type': 'text', 'text': 'Continue', 'text_elements': []}]})
        assert 'result' in turn, turn
        events.extend(finish_turn(native, thread))
        read = native.call('thread/read', {'threadId': thread, 'includeTurns': True})
        rollout = Path(read['result']['thread']['path']).resolve()
        assert rollout.is_relative_to(root.resolve()), 'Unexpected native rollout location'
        records = [json.loads(line) for line in rollout.read_text(encoding='utf-8').splitlines()]
        raw_items = [event['params']['item']['type'] for event in events
                     if event.get('method') == 'rawResponseItem/completed']
        return {
            'case': label,
            'firstTurnRequests': server.seen[before:first_end],
            'secondTurnRequests': server.seen[first_end:],
            'rawCompactionItemReceived': 'compaction' in raw_items,
            'nativeCompactionNotificationReceived': any(
                event.get('method') == 'item/completed'
                and event['params'].get('item', {}).get('type') == 'contextCompaction'
                for event in events),
            'compactionRecordedInOwnedRollout': any(
                record.get('type') == 'compacted'
                and any(item.get('type') == 'compaction'
                        for item in record['payload'].get('replacement_history', []))
                for record in records),
        }
    finally:
        native.close()


def main():
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
    server.seen = []
    threading.Thread(target=server.serve_forever, daemon=True).start()
    report = {'nativeVersion': INSTALL['appServerVersion'], 'realModelRequests': 0, 'cases': []}
    try:
        with tempfile.TemporaryDirectory(prefix='context-management-', dir=ROOT / '.cache') as directory:
            temporary = Path(directory).resolve()
            assert temporary.is_relative_to((ROOT / '.cache').resolve())
            for label, config in [
                    ('baseline', {}),
                    ('direct_context_management', {
                        'context_management': [{'type': 'compaction', 'compact_threshold': 1000}]}),
                    ('native_auto_compact_limit', {
                        'model_auto_compact_token_limit': 1000,
                        'model_auto_compact_token_limit_scope': 'total',
                        'model_post_turn_compact_threshold_percent': 0})]:
                root = temporary / label
                root.mkdir()
                result = probe(root, server, label, config)
                report['cases'].append(result)
                print(json.dumps(result), flush=True)
    finally:
        server.shutdown()
        server.server_close()
    report['syntheticModelRequests'] = len(server.seen)
    output = ROOT / '.cache/native-context-management-probe.json'
    output.write_text(json.dumps(report, indent=2), encoding='utf-8')
    print(json.dumps({'report': str(output), 'syntheticModelRequests': len(server.seen)}))


if __name__ == '__main__':
    main()
