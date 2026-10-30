"""Packaging regressions: verbatim notices, payload boundaries and cleanup."""
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import package_portable
import test_macos_portable
from portable_contents import SHARED_FILES, write_notices


class PackageLayoutTests(unittest.TestCase):
    def test_macos_verifier_accepts_minimal_package_without_development_scripts(self):
        # Exercise the complete ZIP verifier on every CI platform. Only native
        # extraction/launch and HTTP are simulated; absent scripts must fail.
        with tempfile.TemporaryDirectory() as temporary:
            archive = Path(temporary) / 'macos.zip'
            payload = {
                'codex-relay': b'binary fixture',
                '启动中转站.command': b'launcher fixture',
                'relay.config.json': b'{"port":4317}',
                '使用说明.md': b'readme',
                'web/dist/index.html': b'<div id="root"></div>',
                'web/dist/assets/index.js': b'console.log("fixture")',
                'third-party/dependencies.json': b'[{"licenseFiles":[]}]',
                'third-party/LICENSES.txt': b'license fixture',
            }
            manifest = {'target': 'aarch64-apple-darwin', 'files': []}
            with zipfile.ZipFile(archive, 'w') as bundle:
                for name, data in payload.items():
                    mode = 0o755 if name in {'codex-relay', '启动中转站.command'} else 0o644
                    manifest['files'].append({'path': name, 'size': len(data),
                        'sha256': hashlib.sha256(data).hexdigest(), 'mode': oct(mode)})
                    info = zipfile.ZipInfo('CodexRelay/' + name)
                    info.create_system = 3
                    info.external_attr = (stat.S_IFREG | mode) << 16
                    bundle.writestr(info, data)
                bundle.writestr('CodexRelay/manifest.json', json.dumps(manifest))

            launches = []
            stopped = []

            def native_run(command, **kwargs):
                if command[0] == '/usr/bin/ditto':
                    with zipfile.ZipFile(command[3]) as bundle:
                        bundle.extractall(command[4])
                elif Path(command[0]).name == '启动中转站.command':
                    launcher = Path(command[0])
                    self.assertTrue(launcher.is_file())
                    self.assertFalse((launcher.parent / 'scripts').exists())
                    self.assertEqual(kwargs['env']['PATH'], '/usr/bin:/bin:/usr/sbin:/sbin')
                    port = json.loads((launcher.parent / 'relay.config.json').read_text())['port']
                    cache = launcher.parent / '.cache'
                    cache.mkdir(exist_ok=True)
                    (cache / f'launcher-{port}.pid').write_text('12345', encoding='ascii')
                    launches.append(command)
                else:
                    return subprocess.CompletedProcess(command, 1, '', 'Missing portable command')
                return subprocess.CompletedProcess(command, 0, '', '')

            def http_open(request, **kwargs):
                path = request.full_url.split('/', 3)[3]
                responses = {
                    'healthz': (200, b'{}'),
                    'api/overview': (200, json.dumps({'accounts': [], 'installation': None,
                        'operations': {'active': 0, 'queued': 0}, 'inferenceEnabled': False}).encode()),
                    'api/api-keys': (200, b'{"keys":[]}'),
                    'v1/models': (401, b'{}'),
                    '': (200, payload['web/dist/index.html']),
                    'assets/index.js': (200, payload['web/dist/assets/index.js']),
                    'api/service/stop': (202, b'{}'),
                }
                if path == 'api/service/stop':
                    stopped.append(request.data)
                status, body = responses[path]
                response = io.BytesIO(body)
                response.status = status
                return response

            with patch.object(test_macos_portable.subprocess, 'run', side_effect=native_run), \
                 patch.object(test_macos_portable.urllib.request, 'build_opener') as opener:
                opener.return_value.open.side_effect = http_open
                report = test_macos_portable.verify_archive(archive)
            self.assertEqual(len(launches), 2)
            self.assertEqual(stopped, [b'{}'])
            self.assertTrue(report['portableLauncher'])
            self.assertTrue(report['gracefulShutdown'])
            self.assertNotIn('portableShellScripts', report)

    def test_notices_preserve_original_bytes_and_attribution(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'dependency'
            source.mkdir()
            originals = {'LICENSE': b'Copyright Example\r\nMIT\r\n',
                         'NOTICE': '额外声明\n'.encode(),
                         'custom.txt': b'Custom license\xff\x00'}
            for name, data in originals.items():
                (source / name).write_bytes(data)
            (source / 'README.md').write_text('Not a license')
            package = {'name': 'sample', 'version': '1.0', 'license': 'MIT',
                       'license_file': 'custom.txt'}
            stage = root / 'stage'
            stage.mkdir()
            self.assertEqual(write_notices(stage, [('rust', package, source)] * 2), 1)
            self.assertEqual(len(list((stage / 'third-party').iterdir())), 2)
            inventory = json.loads((stage / 'third-party/dependencies.json').read_text())
            text = (stage / 'third-party/LICENSES.txt').read_bytes()
            self.assertEqual(len(inventory[0]['licenseFiles']), 3)
            for original in inventory[0]['licenseFiles']:
                data = text[original['offset']:original['offset'] + original['size']]
                self.assertEqual(data, originals[original['name']])
                self.assertEqual(hashlib.sha256(data).hexdigest(), original['sha256'])
                self.assertIn(('rust / sample / 1.0 / ' + original['name']).encode(), text)

    def make_root(self, root):
        files = (*SHARED_FILES, 'target/release/codex-relay.exe', 'web/dist/index.html',
                 '启动中转站.cmd', '首次准备.cmd', 'scripts/Open-Relay.ps1',
                 'scripts/Prepare-Relay.ps1', 'scripts/Start-Relay.ps1',
                 'docs/PORTABLE-README.md', 'examples/private-future-example.json',
                 'data/auth.json', '.cache/private.log', 'docs/GATEWAY-EVOLUTION.md')
        for relative in files:
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('fixture', encoding='utf-8')
        (root / 'package.json').write_text('{"version":"test"}')

    def run_package(self, root):
        with patch.object(package_portable, 'ROOT', root), \
             patch.object(package_portable, 'imports', return_value=[]), \
             patch.object(package_portable, 'collect_notices',
                          side_effect=lambda _, stage, target: write_notices(stage, [])), \
             contextlib.redirect_stdout(io.StringIO()):
            package_portable.main()

    def test_payload_allowlist_and_no_unpacked_copy(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.make_root(root)
            self.run_package(root)
            archive, = (root / 'dist').glob('*.zip')
            self.assertEqual(len(list((root / 'dist').iterdir())), 2)
            with zipfile.ZipFile(archive) as bundle:
                names = {'/'.join(Path(name).parts[1:]) for name in bundle.namelist()}
                self.assertTrue(set(SHARED_FILES) <= names)
                self.assertIn('使用说明.md', names)
                self.assertIn('relay.config.json', names)
                self.assertFalse(any(name.startswith(('docs/', 'examples/')) for name in names))
                self.assertEqual({name for name in names if name.startswith('scripts/')},
                                 {'scripts/Open-Relay.ps1'})
                self.assertTrue(names.isdisjoint({'docs/PORTABLE-README.md',
                    'docs/GATEWAY-EVOLUTION.md', 'examples/private-future-example.json',
                    'data/auth.json', '.cache/private.log', '首次准备.cmd',
                    'scripts/Prepare-Relay.ps1'}))

    def test_archive_failure_removes_partial_and_stage(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.make_root(root)
            destination = root / 'dist'
            destination.mkdir()
            previous = destination / 'CodexRelay-0.1.0-windows-x64-20260101T000000Z.zip'
            previous.write_bytes(b'previous release')
            with patch.object(zipfile.ZipFile, 'write', side_effect=OSError('disk full')):
                with self.assertRaisesRegex(OSError, 'disk full'):
                    self.run_package(root)
            self.assertEqual(list(destination.iterdir()), [previous])
            self.assertEqual(previous.read_bytes(), b'previous release')

    def test_success_removes_old_outputs_only_for_same_platform(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.make_root(root)
            destination = root / 'dist'
            destination.mkdir()
            stem = 'CodexRelay-0.1.0-windows-x64-20260101T000000Z'
            old = destination / stem
            old.mkdir()
            (old / 'codex-relay.exe').write_bytes(b'old')
            for suffix in ('.zip', '.zip.sha256', '.zip.validation.json'):
                (destination / (stem + suffix)).write_bytes(b'old')
            preserved = ('CodexRelay-0.1.0-macos-arm64-20260101T000000Z.zip',
                         'CodexRelay-source.zip', 'notes.txt')
            for name in preserved:
                (destination / name).write_bytes(b'keep')
            self.run_package(root)
            self.assertFalse(any(p.name.startswith(stem) for p in destination.iterdir()))
            self.assertEqual(len(list(destination.iterdir())), len(preserved) + 2)
            for name in preserved:
                self.assertEqual((destination / name).read_bytes(), b'keep')

    @unittest.skipUnless(os.name == 'nt', 'Windows extended-length paths')
    def test_success_removes_old_package_with_long_cache_paths(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.make_root(root)
            stem = 'CodexRelay-0.1.0-windows-x64-20260101T000000Z'
            old = root / 'dist' / stem
            # Extracted releases can contain nested native probe/skill caches.
            cached = (old / stem / '.cache/probes' / ('rust-shared-' + 'a' * 36)
                      / 'skills/.system/skill-installer/scripts/install-skill-from-github.py')
            self.assertGreater(len(str(cached)), 260)
            extended = Path('\\\\?\\' + str(cached))
            extended.parent.mkdir(parents=True)
            extended.write_text('cached skill', encoding='utf-8')
            try:
                self.run_package(root)
                self.assertFalse(old.exists())
                self.assertEqual(len(list((root / 'dist').iterdir())), 2)
            finally:
                # Keep a failing regression from leaving an undeletable fixture.
                self.assertEqual(old.resolve().parent, (root / 'dist').resolve())
                extended_old = Path('\\\\?\\' + str(old))
                if extended_old.exists():
                    shutil.rmtree(extended_old)


if __name__ == '__main__':
    unittest.main()
