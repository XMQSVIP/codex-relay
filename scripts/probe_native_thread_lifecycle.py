"""Probe ephemeral thread cleanup in an isolated installed app-server, without inference."""
import json
import tempfile
import time
from pathlib import Path

from probe_native_output_limit import INSTALL, Native, ROOT


def main():
    report = {'nativeVersion': INSTALL['appServerVersion'], 'realModelRequests': 0}
    with tempfile.TemporaryDirectory(prefix='thread-lifecycle-', dir=ROOT / '.cache') as directory:
        native = Native(Path(directory), 1)
        try:
            assert 'result' in native.call('initialize', {'clientInfo': INSTALL['nativeClientInfo'],
                'capabilities': {'experimentalApi': True}})
            native.send({'method': 'initialized'})
            threads = []
            for _ in range(2):
                result = native.call('thread/start', {'model': 'gpt-6.1-sol', 'modelProvider': 'synthetic',
                    'cwd': str(native.cwd), 'ephemeral': True, 'approvalPolicy': 'never',
                    'sandbox': 'read-only', 'baseInstructions': '', 'developerInstructions': '',
                    'environments': [], 'runtimeWorkspaceRoots': [], 'selectedCapabilityRoots': []})
                assert 'result' in result, result
                threads.append(result['result']['thread']['id'])
            report['loadedBefore'] = native.call('thread/loaded/list', {})
            report['unsubscribe'] = native.call('thread/unsubscribe', {'threadId': threads[0]})
            for _ in range(20):
                loaded = native.call('thread/loaded/list', {})
                if threads[0] not in loaded.get('result', {}).get('data', []):
                    break
                time.sleep(0.1)
            report['loadedAfter'] = loaded
            report['unloadedWithinTwoSeconds'] = threads[0] not in loaded.get('result', {}).get('data', [])
            report['observationLimit'] = 'A short probe does not test the documented 30-minute no-subscriber inactivity grace period.'
            report['otherThreadPreserved'] = threads[1] in loaded.get('result', {}).get('data', [])
            unavailable = native.call('thread/unload', {'threadId': threads[0]})
            report['unloadMethodErrorCode'] = unavailable.get('error', {}).get('code')
        finally:
            native.close()
    (ROOT / '.cache/native-thread-lifecycle.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
    print(json.dumps(report, indent=2))


if __name__ == '__main__':
    main()
