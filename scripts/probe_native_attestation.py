"""Inspect attestation negotiation and request structure without real credentials.

The installed desktop bundle is read as data. Two isolated official app-server
processes send requests to a synthetic localhost model. This does not capture a
live desktop request, obtain a device token, or verify upstream acceptance.
"""
import hashlib
import http.server
import json
from pathlib import Path
import queue
import re
import struct
import subprocess
import tempfile
import threading
import time

from probe_native_output_limit import INSTALL, Model, Native, ROOT, SEEN


class CaptureModel(Model):
    def do_POST(self):
        self.server.headers_seen.append({
            'names': sorted(k.lower() for k in self.headers),
            'userAgent': self.headers.get('User-Agent'),
            'originator': self.headers.get('originator'),
            'contentType': self.headers.get('Content-Type'),
            'contentEncoding': self.headers.get('Content-Encoding'),
            'attestationPresent': 'x-oai-attestation' in self.headers,
        })
        super().do_POST()


class AttestationProbe(Native):
    def __init__(self, root, port):
        self.callbacks = []
        super().__init__(root, port)

    def receive(self, timeout=30):
        deadline = time.monotonic() + timeout
        while True:
            message = self.messages.get(timeout=max(0.01, deadline - time.monotonic()))
            if message.get('process_exited'):
                raise RuntimeError('Native process exited')
            if 'id' in message and 'method' in message:
                method = message['method']
                self.callbacks.append(method)
                self.send({'id': message['id'], 'error': {
                    'code': -32001 if method == 'attestation/generate' else -32601,
                    'message': 'No proof provider in this isolated local probe.'}})
                # No fabricated token, device-check fallback, or desktop token.
                if time.monotonic() >= deadline:
                    raise queue.Empty
                continue
            return message

    def close(self):
        self.process.stdin.close()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            try:
                self.process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=3)
        self.process.stdout.close()


def desktop_sources():
    """Read only static build sources; never import or execute desktop code."""
    result = {}
    with Path(INSTALL['archive']).open('rb') as archive:
        prefix = archive.read(16)
        magic, header_size, _, json_size = struct.unpack('<IIII', prefix)
        assert magic == 4 and 8 <= header_size <= 32 * 1024 * 1024
        assert json_size <= header_size - 8
        header = json.loads(archive.read(json_size))

        def walk(node, path=''):
            for name, child in node.get('files', {}).items():
                assert '/' not in name and '\\' not in name and name != '..'
                child_path = f'{path}/{name}' if path else name
                if 'files' in child:
                    yield from walk(child, child_path)
                elif (child_path.startswith('.vite/build/')
                      and child_path.endswith('.js') and not child.get('unpacked')):
                    yield child_path, child

        entries = list(walk(header))
        package_entry = header['files']['package.json']
        archive.seek(8 + header_size + int(package_entry['offset']))
        package = json.loads(archive.read(package_entry['size']))
        assert package['version'] == INSTALL['nativeClientInfo']['version'], 'Desktop updated; run prepare again.'
        for path, entry in entries:
            assert entry['size'] <= 32 * 1024 * 1024
            archive.seek(8 + header_size + int(entry['offset']))
            raw = archive.read(entry['size'])
            source = raw.decode('utf-8')
            if 'requestAttestation' in source or 'attestation/generate' in source:
                result[path] = (source, hashlib.sha256(raw).hexdigest())
    return result


def inspect_desktop():
    sources = desktop_sources()
    init = [(path, source, digest) for path, (source, digest) in sources.items()
            if 'requestAttestation:e.supportsAttestationRequests()' in source]
    handlers = [(path, source, digest) for path, (source, digest) in sources.items()
                if 'methods:[`attestation/generate`]' in source]
    assert len(init) == 1 and len(handlers) == 1, 'Desktop implementation changed; inspect it again.'
    init_path, source, init_digest = init[0]
    offset = source.index('requestAttestation:e.supportsAttestationRequests()')
    template = source[max(0, offset - 700):offset + 250]
    schema = json.loads((ROOT / '.cache/native-protocol/v1/InitializeParams.json').read_text())
    properties = schema['definitions']['InitializeCapabilities']['properties']
    capability_names = sorted(name for name in properties if re.search(r'\b' + re.escape(name) + r':', template))
    handler_path, handler, handler_digest = handlers[0]
    return {
        'evidence': 'static installed desktop code; no live desktop capture',
        'initializeSource': {'path': init_path, 'sha256': init_digest},
        'handlerSource': {'path': handler_path, 'sha256': handler_digest},
        'capabilityNames': capability_names,
        'attestationCapability': 'conditional: supportsAttestationRequests()',
        'nativeProofProviderObserved': 'generateToken()' in handler,
        'deviceCheckObserved': 'DeviceCheck' in handler,
        'liveDesktopRequestCaptured': False,
    }


def structure(body):
    return {
        'topLevelFields': sorted(body),
        'inputTypes': [item.get('type', 'message') for item in body.get('input', [])],
        'tools': [{'type': tool.get('type'), 'name': tool.get('name')}
                  for tool in body.get('tools', [])],
        'reasoning': body.get('reasoning'),
        'text': body.get('text'),
        'stream': body.get('stream'),
        'store': body.get('store'),
        'parallelToolCalls': body.get('parallel_tool_calls'),
        'hasInstructions': bool(body.get('instructions')),
    }


def run_case(root, server, enabled):
    native = AttestationProbe(root, server.server_port)
    try:
        capabilities = {'experimentalApi': True, 'requestAttestation': enabled}
        reply = native.call('initialize', {'clientInfo': INSTALL['nativeClientInfo'],
                                          'capabilities': capabilities})
        assert 'result' in reply, 'Native initialize failed'
        native.send({'method': 'initialized'})
        before = len(SEEN)
        result = native.probe('attestation-enabled' if enabled else 'native-default-capabilities', {}, {})
        assert len(SEEN) == before + 1, 'Expected exactly one local synthetic request'
        return {
            'case': 'opt-in-without-proof-provider' if enabled else 'native-default-capabilities',
            'initializeCapabilities': capabilities,
            'initializeAccepted': True,
            'userAgent': reply['result'].get('userAgent'),
            'status': result['status'],
            'callbackMethods': native.callbacks,
            'upstreamHeaders': server.headers_seen[-1],
            'upstreamBodyStructure': structure(SEEN[-1]),
        }
    finally:
        native.close()


def main():
    executable = Path(INSTALL['appServerExecutable'])
    with executable.open('rb') as native_file:
        digest = hashlib.file_digest(native_file, 'sha256').hexdigest()
    assert digest == INSTALL['appServerSha256'], 'Native installation changed; run prepare again.'
    desktop = inspect_desktop()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), CaptureModel)
    server.headers_seen = []
    threading.Thread(target=server.serve_forever, daemon=True).start()
    SEEN.clear()
    try:
        with tempfile.TemporaryDirectory(prefix='native-attestation-', dir=ROOT / '.cache') as directory:
            cases = []
            for enabled in [False, True]:
                root = Path(directory) / str(enabled).lower()
                root.mkdir()
                cases.append(run_case(root, server, enabled))
    finally:
        server.shutdown()
        server.server_close()
    report = {
        'nativeVersion': INSTALL['appServerVersion'],
        'desktopClientInfo': INSTALL['nativeClientInfo'],
        'trafficSource': 'isolated native fixture with gateway initialize capabilities; not the gateway HTTP endpoint',
        'realModelRequests': 0,
        'syntheticModelRequests': len(SEEN),
        'validAttestationTokenGenerated': False,
        'desktop': desktop,
        'cases': cases,
        'localComparison': {
            'sameHeadersSummary': cases[0]['upstreamHeaders'] == cases[1]['upstreamHeaders'],
            'sameBodyStructure': cases[0]['upstreamBodyStructure'] == cases[1]['upstreamBodyStructure'],
        },
        'limits': [
            'Local custom-provider traffic does not establish behavior on the OpenAI authenticated route.',
            'No desktop proof provider is loaded, invoked, copied, or replayed.',
            'Initialization capability differences are from static desktop code, not a live desktop session.',
            'No byte-for-byte HTTP/TLS/WebSocket comparison or upstream attestation verification.',
        ],
    }
    output = ROOT / '.cache/native-attestation-probe.json'
    output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    print(json.dumps({'report': str(output), 'syntheticModelRequests': len(SEEN),
                      'callbackCounts': [len(case['callbackMethods']) for case in cases],
                      'localComparison': report['localComparison']}, ensure_ascii=False))


if __name__ == '__main__':
    main()
