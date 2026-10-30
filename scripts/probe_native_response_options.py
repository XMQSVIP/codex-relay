"""Probe Responses options through the installed native transport, without credentials.

Uses a synthetic localhost model; records the actual upstream body rather than
assuming that accepting an app-server parameter means the model receives it.
"""
import http.server
import json
import tempfile
import threading
from pathlib import Path

from probe_native_output_limit import INSTALL, Model, Native, ROOT, SEEN


def main():
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    report = {'nativeVersion': INSTALL['appServerVersion'], 'realModelRequests': 0, 'cases': []}
    try:
        with tempfile.TemporaryDirectory(prefix='native-options-', dir=ROOT / '.cache') as directory:
            for label, config, turn in [
                    ('baseline', {}, {}),
                    ('verbosity', {'model_verbosity': 'high'}, {}),
                    ('service_tier', {}, {'serviceTier': 'priority'}),
                    ('service_tier_default', {}, {'serviceTier': 'default'}),
                    ('service_tier_flex', {}, {'serviceTier': 'flex'}),
                    ('json_schema', {}, {'outputSchema': {'type': 'object', 'properties': {
                        'ok': {'type': 'boolean'}}, 'required': ['ok'], 'additionalProperties': False}}),
                    ('json_object_config', {'text': {'format': {'type': 'json_object'}}}, {}),
                    ('parallel_tools', {'parallel_tool_calls': False}, {}),
                    ('tool_choice', {'tool_choice': 'required'}, {}),
                    ('temperature', {'temperature': 0.5}, {}),
                    ('top_p', {'top_p': 0.8}, {}),
                ]:
                case_dir = Path(directory) / label
                case_dir.mkdir()
                # Native config overrides can survive thread unload. Isolate each
                # case in a new process/home so a previous probe cannot affect it.
                native = Native(case_dir, server.server_port)
                try:
                    initialized = native.call('initialize', {
                        'clientInfo': INSTALL['nativeClientInfo'],
                        'capabilities': {'experimentalApi': True}})
                    assert 'result' in initialized, initialized
                    native.send({'method': 'initialized'})
                    before = len(SEEN)
                    result = native.probe(label, config, turn)
                    if len(SEEN) > before:
                        body = SEEN[-1]
                        result['upstream'] = {key: body[key] for key in [
                            'text', 'service_tier', 'parallel_tool_calls', 'tool_choice',
                            'temperature', 'top_p'] if key in body}
                    report['cases'].append(result)
                    print(json.dumps(result), flush=True)
                finally:
                    native.close()
    finally:
        server.shutdown()
        server.server_close()
    (ROOT / '.cache/native-response-options.json').write_text(
        json.dumps(report, indent=2), encoding='utf-8')


if __name__ == '__main__':
    main()
