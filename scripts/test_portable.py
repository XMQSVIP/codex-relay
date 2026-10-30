"""Smoke-test an actual portable archive in an isolated directory, without inference."""
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
archive = Path(sys.argv[1]).resolve()
port = int(sys.argv[2]) if len(sys.argv) > 2 else 4327
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def get(path, **headers):
    try:
        response = opener.open(urllib.request.Request(f'http://127.0.0.1:{port}{path}', headers=headers), timeout=5)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        return response.status, response.read()


with tempfile.TemporaryDirectory(prefix='portable smoke 空格 ', dir=ROOT/'.cache') as temporary:
    folder = Path(temporary)
    assert folder.resolve().is_relative_to((ROOT/'.cache').resolve())
    with zipfile.ZipFile(archive) as bundle:
        for name in bundle.namelist():
            relative = Path(name)
            assert not relative.is_absolute() and '..' not in relative.parts and ':' not in name
            assert not {'.cache', 'data', 'node_modules', 'target'} & set(relative.parts)
        bundle.extractall(folder)
    package, = folder.iterdir()
    manifest = json.loads((package/'manifest.json').read_text(encoding='utf-8'))
    assert (package/'使用说明.md').is_file()
    assert (package/'relay.config.json').is_file()
    assert not (package/'docs').exists()
    assert not (package/'examples').exists()
    assert {p.name for p in (package/'scripts').iterdir()} == {'Open-Relay.ps1'}
    for entry in manifest['files']:
        file = package/entry['path']
        assert file.stat().st_size == entry['size']
        assert hashlib.sha256(file.read_bytes()).hexdigest() == entry['sha256']
    packages = json.loads((package/'third-party/dependencies.json').read_text(encoding='utf-8'))
    assert packages
    assert {p.name for p in (package/'third-party').iterdir()} == {'dependencies.json', 'LICENSES.txt'}
    notices = (package/'third-party/LICENSES.txt').read_bytes()
    for dependency in packages:
        for original in dependency['licenseFiles']:
            data = notices[original['offset']:original['offset'] + original['size']]
            assert len(data) == original['size']
            assert hashlib.sha256(data).hexdigest() == original['sha256']
    (package/'relay.config.json').write_text(json.dumps({'port': port}), encoding='utf-8')
    command = ['powershell.exe', '-NoLogo', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', str(package/'scripts/Open-Relay.ps1'), '-NoBrowser']
    def launch(timeout):
        # Background children can inherit unused pipe handles on Windows. Files
        # let us wait for the launcher process without waiting for service exit.
        with (folder/'launcher-test.log').open('wb') as log:
            result = subprocess.run(command, cwd=folder, stdout=log, stderr=log, timeout=timeout)
        assert result.returncode == 0, (folder/'launcher-test.log').read_text(errors='replace')
    owned = None
    try:
        launch(45)
        owned = int((package/'.cache/rust-admin.pid').read_text().strip())
        status, body = get('/api/session')
        session = json.loads(body)
        assert status == 200 and session['requiresToken'] is False
        assert session['passwordEnabled'] is False and session['authenticated'] is True
        status, body = get('/api/overview')
        overview = json.loads(body)
        assert status == 200 and overview['accounts'] == [] and overview['installation'] is None
        assert json.loads(get('/api/api-keys')[1])['keys'] == []
        request = urllib.request.Request(f'http://127.0.0.1:{port}/api/api-keys',
            data=json.dumps({'name':'portable test', 'groupId':overview['operations']['groups'][0]['id']}).encode(),
            headers={'Content-Type':'application/json'})
        with opener.open(request, timeout=5) as response:
            created = json.load(response)
            assert response.status == 201
            assert created['key']['models'] == [] and created['key']['expiresAt'] is None
        assert json.loads(get('/api/api-keys')[1])['keys'][0]['secret'] == created['secret']
        assert get('/v1/models', Authorization='Bearer '+created['secret'])[0] == 200
        assert get('/v1/usage')[0] == 401
        usage = json.loads(get('/v1/usage', Authorization='Bearer '+created['secret'])[1])
        assert usage['keyId'] == created['key']['id'] and usage['summary']['requests'] == 0
        assert json.loads(get('/api/capabilities')[1])['accounts'] == []
        storage = overview['operations']['responseBindings']
        assert storage['persistent'] is True
        assert storage['maxBytes'] == 1024 * 1024 * 1024
        assert storage['ttlSeconds'] == 30 * 24 * 3600
        assert storage['bodiesStored'] is False
        assert get('/v1/models')[0] == 401
        # Verify the shipped binary contains the new HTTP Responses adapter,
        # while rejecting malformed supported parameters before native dispatch.
        for token, expected in [(None, 401), (created['secret'], 400)]:
            headers = {'Content-Type': 'application/json'}
            if token:
                headers['Authorization'] = 'Bearer ' + token
            request = urllib.request.Request(f'http://127.0.0.1:{port}/v1/responses',
                data=json.dumps({'model':'portable-test','input':'test','stream':'yes'}).encode(),
                headers=headers)
            try:
                response = opener.open(request, timeout=5)
            except urllib.error.HTTPError as error:
                response = error
            with response:
                error_body = json.load(response)
                assert response.status == expected, (response.status, error_body)
                assert 'error' in error_body
        # No routes are configured: an ignored output limit reaches routing
        # instead of failing parameter validation, for both JSON and SSE.
        for stream in [False, True]:
            request = urllib.request.Request(f'http://127.0.0.1:{port}/v1/responses',
                data=json.dumps({'model':'portable-test','input':'test','stream':stream,'max_output_tokens':1}).encode(),
                headers={'Content-Type':'application/json','Authorization':'Bearer '+created['secret']})
            try:
                response = opener.open(request, timeout=5)
            except urllib.error.HTTPError as error:
                response = error
            with response:
                assert response.status == 503
                assert json.load(response)['error']['code'] == 'INFERENCE_DISABLED'
        assert get('/v1/responses/resp_missing', Authorization='Bearer '+created['secret'])[0] == 404
        # No native installation in this isolated package: start must surface a failure,
        # without launching or touching the user's desktop. Stop remains idempotent.
        request = urllib.request.Request(f'http://127.0.0.1:{port}/api/desktop/start', data=b'{}',
            headers={'Content-Type':'application/json'})
        with opener.open(request, timeout=5) as response:
            assert response.status == 200
        import time
        for _ in range(50):
            desktop = json.loads(get('/api/overview')[1])['desktop']
            if desktop['state'] == 'failed':
                break
            time.sleep(0.1)
        assert desktop['state'] == 'failed' and desktop['error']['code']
        request = urllib.request.Request(f'http://127.0.0.1:{port}/api/desktop/stop', data=b'{}')
        with opener.open(request, timeout=5) as response:
            assert response.status == 200

        # Create/delete both profile sources through the shipped API. No native
        # process is started; duplicate display names must remain distinguishable.
        account_ids = []
        for native in [True, False]:
            request = urllib.request.Request(f'http://127.0.0.1:{port}/api/accounts',
                data=json.dumps({'name':'duplicate test', 'nativeConfig':native}).encode(),
                headers={'Content-Type':'application/json'})
            with opener.open(request, timeout=5) as response:
                account_ids.append(json.load(response)['id'])
        for account_id in account_ids:
            revision = json.loads(get('/api/operations')[1])['revision']
            request = urllib.request.Request(f'http://127.0.0.1:{port}/api/accounts/{account_id}',
                method='DELETE', data=json.dumps({'revision':revision}).encode(),
                headers={'Content-Type':'application/json'})
            with opener.open(request, timeout=5) as response:
                assert json.load(response)['ok']
            assert not (package/'data/accounts'/account_id).exists()
        assert json.loads(get('/api/overview')[1])['accounts'] == []
        assert json.loads(get('/api/operations')[1])['accounts'] == []

        assert get('/api/overview', Origin='https://evil.invalid')[0] == 403
        status, body = get('/')
        assert status == 200 and b'<div id="root"></div>' in body
        launch(20)
        assert int((package/'.cache/rust-admin.pid').read_text().strip()) == owned
        operations = json.loads(get('/api/operations')[1])
        request = urllib.request.Request(f'http://127.0.0.1:{port}/api/groups', data=json.dumps({'revision':operations['revision'],'name':'test group','enabled':True}).encode(), headers={'Content-Type':'application/json'})
        with opener.open(request, timeout=5) as response:
            target = json.load(response)['id']
        key_path = f"http://127.0.0.1:{port}/api/api-keys/{created['key']['id']}"
        request = urllib.request.Request(key_path+'/group', data=json.dumps({'revision':1,'groupId':target}).encode(), headers={'Content-Type':'application/json'})
        with opener.open(request, timeout=5) as response:
            assert json.load(response)['groupId'] == target
        request = urllib.request.Request(key_path, data=b'{"revision":2,"state":"revoked"}', headers={'Content-Type':'application/json'})
        with opener.open(request, timeout=5) as response:
            assert response.status == 200
        assert json.loads(get('/api/api-keys')[1])['keys'] == []
        request = urllib.request.Request(key_path, method='DELETE', data=b'{"revision":3}', headers={'Content-Type':'application/json'})
        with opener.open(request, timeout=5) as response:
            assert json.load(response)['deleted']
        assert get('/v1/models', Authorization='Bearer '+created['secret'])[0] == 401
        request = urllib.request.Request(f'http://127.0.0.1:{port}/api/service/stop', data=b'{}')
        with opener.open(request, timeout=5) as response:
            assert response.status == 202 and json.load(response)['state'] == 'stopping'
        env = dict(os.environ, BRIDGE_TEST_PID=str(owned), BRIDGE_TEST_EXE=str(package/'codex-relay.exe'))
        shutdown_check = subprocess.run(['powershell.exe', '-NoProfile', '-Command', "$ErrorActionPreference = 'Stop'; $p = Get-Process -Id ([int]$env:BRIDGE_TEST_PID) -ErrorAction SilentlyContinue; if ($p) { if ($p.Path -and $p.Path -ne $env:BRIDGE_TEST_EXE) { throw 'Process mismatch' }; if (-not $p.WaitForExit(15000)) { throw 'Graceful shutdown timed out' } }; exit 0"], env=env, timeout=20, capture_output=True)
        assert shutdown_check.returncode == 0, shutdown_check.stderr.decode(errors='replace')
        try:
            get('/healthz')
        except (urllib.error.URLError, OSError):
            pass
        else:
            raise AssertionError('Listener remained available after shutdown')
        report = {'archive': archive.name, 'verifiedFiles': len(manifest['files']), 'emptyAccounts': True, 'emptyKeysOnFirstStart': True, 'optionalModelsAndPermanentKey': True, 'anonymousLocalConsole': True, 'clientKeyRequired': True, 'repeatLaunchReusedPid': True, 'pathWithSpacesAndUnicode': True, 'modelRequests': 0, 'desktopControlsReportMissingInstallation': True, 'gracefulServiceStop': True, 'keyGroupMoveRevokeAndDelete': True, 'accountDeleteBothSources': True, 'responsesEndpointAuthAndValidation': True, 'minimalPackageLayout': True, 'portFromConfig': True, 'maxOutputTokensIgnored': True}
        (ROOT/'.cache/portable-validation.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
        print(json.dumps(report, indent=2))
    finally:
        pid_file = package/'.cache/rust-admin.pid'
        if owned is None and pid_file.exists():
            owned = int(pid_file.read_text().strip())
        if owned:
            env = dict(os.environ, BRIDGE_TEST_PID=str(owned), BRIDGE_TEST_EXE=str(package/'codex-relay.exe'))
            subprocess.run(['powershell.exe', '-NoProfile', '-Command', "$p = Get-Process -Id ([int]$env:BRIDGE_TEST_PID) -ErrorAction SilentlyContinue; if ($p) { if ($p.Path -ne $env:BRIDGE_TEST_EXE) { throw 'Process mismatch' }; Stop-Process -Id $p.Id; $p.WaitForExit(5000) | Out-Null }; exit 0"], env=env, check=True, timeout=15, capture_output=True)
