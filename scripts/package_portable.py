"""Build-machine-only packaging; recipients need no Python or development tools."""
import hashlib
import json
import shutil
import struct
import tempfile
import zipfile
from datetime import datetime, timezone
from pathlib import Path

from portable_contents import SHARED_FILES, collect_notices, prune_previous_packages

ROOT = Path(__file__).resolve().parent.parent


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def imports(path):
    data = path.read_bytes()
    pe = struct.unpack_from('<I', data, 0x3c)[0]
    assert data[pe:pe+4] == b'PE\0\0', 'Not a PE executable'
    machine, count = struct.unpack_from('<HH', data, pe+4)
    assert machine == 0x8664, 'Expected Windows x64 binary'
    opt_size = struct.unpack_from('<H', data, pe+20)[0]
    opt = pe+24
    assert struct.unpack_from('<H', data, opt)[0] == 0x20b
    sections = []
    for i in range(count):
        size, addr, rawsize, raw = struct.unpack_from('<IIII', data, opt+opt_size+i*40+8)
        sections.append((addr, max(size, rawsize), raw))
    def offset(rva):
        for addr, size, raw in sections:
            if addr <= rva < addr+size:
                return raw+rva-addr
        raise ValueError('Invalid PE RVA')
    rva = struct.unpack_from('<I', data, opt+120)[0]
    names = []
    pos = offset(rva)
    while any(data[pos:pos+20]):
        name = offset(struct.unpack_from('<I', data, pos+12)[0])
        names.append(data[name:data.index(b'\0', name)].decode('ascii'))
        pos += 20
    assert not any(n.lower().startswith(('vcruntime', 'msvcp', 'ucrtbase')) for n in names), 'Rebuild with static CRT'
    return sorted(set(names))


def main():
    binary = ROOT/'target/release/codex-relay.exe'
    assert binary.is_file() and (ROOT/'web/dist/index.html').is_file(), 'Build first'
    dlls = imports(binary)
    version = json.loads((ROOT/'package.json').read_text())['version']
    stamp = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
    name = f'CodexRelay-{version}-windows-x64-{stamp}'
    destination = ROOT/'dist'
    destination.mkdir(exist_ok=True)
    archive = destination/f'{name}.zip'
    pending_archive = destination/f'{name}.zip.partial'
    try:
        with tempfile.TemporaryDirectory(prefix='.windows-package-', dir=destination) as temporary:
            stage = Path(temporary)/name
            stage.mkdir()
            dependencies = stage_package(stage, binary, version, stamp, dlls)
            with zipfile.ZipFile(pending_archive, 'x', zipfile.ZIP_DEFLATED, compresslevel=9, strict_timestamps=False) as output:
                for path in sorted(stage.rglob('*')):
                    if path.is_file():
                        output.write(path, (Path(name)/path.relative_to(stage)).as_posix())
        pending_archive.rename(archive)
    finally:
        pending_archive.unlink(missing_ok=True)
    digest = sha(archive)
    archive.with_suffix('.zip.sha256').write_text(f'{digest}  {archive.name}\n', encoding='ascii')
    prune_previous_packages(destination, archive, 'windows-x64')
    print(json.dumps({'archive': str(archive), 'bytes': archive.stat().st_size, 'sha256': digest, 'dependencies': dependencies}, indent=2))


def stage_package(stage, binary, version, stamp, dlls):
    shutil.copy2(binary, stage/'codex-relay.exe')
    shutil.copytree(ROOT/'web/dist', stage/'web/dist')
    for relative in (*SHARED_FILES, '启动中转站.cmd', 'scripts/Open-Relay.ps1'):
        dest = stage/relative
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT/relative, dest)
    shutil.copy2(ROOT/'docs/PORTABLE-README.md', stage/'使用说明.md')
    dependencies = collect_notices(ROOT, stage, 'x86_64-pc-windows-msvc')
    # Use a fresh allowlisted staging folder, never copy data/ or .cache/.
    files = sorted(p for p in stage.rglob('*') if p.is_file())
    assert not (stage/'data').exists() and not (stage/'.cache').exists()
    manifest = {'version': version, 'target': 'x86_64-pc-windows-msvc', 'buildUtc': stamp,
                'staticCrt': True, 'imports': dlls,
                'files': [{'path': p.relative_to(stage).as_posix(), 'sha256': sha(p), 'size': p.stat().st_size} for p in files]}
    (stage/'manifest.json').write_text(json.dumps(manifest, ensure_ascii=False, indent=2), encoding='utf-8')
    return dependencies


if __name__ == '__main__':
    main()
