"""Build a static Linux server ZIP, then verify its extracted launcher and shutdown."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import platform
import shutil
import stat
import struct
import subprocess
import sys
import tempfile
import zipfile

from portable_contents import collect_notices, prune_previous_packages

ROOT = Path(__file__).resolve().parent.parent
TARGETS = {"x64": ("x86_64-unknown-linux-musl", 62),
           "arm64": ("aarch64-unknown-linux-musl", 183)}


def sha(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def check_binary(binary, architecture):
    # Inspect ELF itself, without executing it or depending on ldd/readelf.
    # Static executables must have neither a dynamic interpreter nor DT_NEEDED.
    with binary.open("rb") as stream:
        header = stream.read(64)
        if len(header) != 64 or header[:7] != b"\x7fELF\x02\x01\x01":
            raise RuntimeError("需要 64 位 little-endian Linux ELF 程序。")
        kind, machine = struct.unpack_from("<HH", header, 16)
        if kind not in (2, 3) or machine != TARGETS[architecture][1]:
            raise RuntimeError("Linux 程序架构与发行包不匹配。")
        offset = struct.unpack_from("<Q", header, 32)[0]
        size, count = struct.unpack_from("<HH", header, 54)
        if size != 56 or not 0 < count < 1024:
            raise RuntimeError("ELF program header 无效。")
        for index in range(count):
            stream.seek(offset + index * size)
            program = stream.read(size)
            if len(program) != size:
                raise RuntimeError("ELF program header 不完整。")
            segment, = struct.unpack_from("<I", program)
            if segment == 3:  # PT_INTERP
                raise RuntimeError("Linux 发行包必须静态链接；请使用 musl target 构建。")
            if segment == 2:  # PT_DYNAMIC
                start = struct.unpack_from("<Q", program, 8)[0]
                length = struct.unpack_from("<Q", program, 32)[0]
                if length > 1024 * 1024 or length % 16:
                    raise RuntimeError("ELF dynamic section 无效。")
                stream.seek(start)
                for _ in range(length // 16):
                    entry = stream.read(16)
                    if len(entry) != 16:
                        raise RuntimeError("ELF dynamic section 不完整。")
                    tag, _ = struct.unpack("<QQ", entry)
                    if tag == 1:  # DT_NEEDED
                        raise RuntimeError("Linux 发行包包含动态库依赖。")
                    if tag == 0:
                        break


def copy_file(source, destination, executable=False):
    if source.is_symlink() or not source.is_file():
        raise RuntimeError(f"打包来源必须是普通文件：{source}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    destination.chmod(0o755 if executable else 0o644)


def write_archive(root, binary, archive, name, architecture, stamp):
    check_binary(binary, architecture)
    with tempfile.TemporaryDirectory(prefix=".linux-package-", dir=archive.parent) as temporary:
        stage = Path(temporary) / name
        stage.mkdir()
        copy_file(binary, stage / "codex-relay", True)
        copy_file(root / "scripts/start-linux.sh", stage / "start-linux.sh", True)
        copy_file(root / "examples/codex-relay.service", stage / "codex-relay.service")
        copy_file(root / "docs/LINUX-SERVER.md", stage / "使用说明.md")
        # Server packages start with the project's default port, never local runtime settings.
        (stage / "relay.config.json").write_text('{"port":4317}\n', encoding="utf-8")
        web = root / "web/dist"
        if not (web / "index.html").is_file():
            raise RuntimeError("缺少前端构建，请先运行 pnpm build:web。")
        for source in sorted(web.rglob("*")):
            if source.is_symlink():
                raise RuntimeError(f"管理界面不能包含符号链接：{source}")
            if source.is_file():
                copy_file(source, stage / "web/dist" / source.relative_to(web))
        collect_notices(root, stage, TARGETS[architecture][0])
        for path in stage.rglob("*"):
            if path.is_file():
                path.chmod(0o755 if path.name in {"codex-relay", "start-linux.sh"} else 0o644)
        manifest = {"format": 1, "version": json.loads((root / "package.json").read_text())["version"],
                    "target": TARGETS[architecture][0], "architecture": architecture,
                    "staticLinked": True, "buildUtc": stamp, "configuration": "release",
                    "files": [{"path": p.relative_to(stage).as_posix(), "sha256": sha(p),
                               "size": p.stat().st_size, "mode": oct(stat.S_IMODE(p.stat().st_mode))}
                              for p in sorted(stage.rglob("*")) if p.is_file()]}
        (stage / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2),
                                             encoding="utf-8")
        (stage / "manifest.json").chmod(0o644)
        with zipfile.ZipFile(archive, "x", zipfile.ZIP_DEFLATED, compresslevel=9) as bundle:
            for path in sorted(stage.rglob("*")):
                if path.is_file():
                    info = zipfile.ZipInfo.from_file(path, f"{name}/{path.relative_to(stage).as_posix()}")
                    info.create_system = 3
                    info.compress_type = zipfile.ZIP_DEFLATED
                    info.external_attr = (stat.S_IFREG | stat.S_IMODE(path.stat().st_mode)) << 16
                    with path.open("rb") as source, bundle.open(info, "w", force_zip64=True) as output:
                        shutil.copyfileobj(source, output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-build", action="store_true")
    args = parser.parse_args()
    if sys.platform != "linux":
        raise RuntimeError("请在 Linux 上运行打包和启动验证。")
    architecture = {"x86_64": "x64", "aarch64": "arm64"}.get(platform.machine())
    if architecture is None:
        raise RuntimeError("Linux 发行包仅支持 x86_64 和 aarch64。")
    target = TARGETS[architecture][0]
    if not args.skip_build:
        subprocess.run(["pnpm", "build:web"], cwd=ROOT, check=True)
        subprocess.run(["node", "scripts/cargo-with-memory.mjs", "build", "--release", "--locked",
                        "--target", target], cwd=ROOT, check=True)
    binary = ROOT / "target" / target / "release/codex-relay"
    version = json.loads((ROOT / "package.json").read_text())["version"]
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
    name = f"CodexRelay-{version}-linux-{architecture}-{stamp}"
    destination = ROOT / "dist"
    destination.mkdir(exist_ok=True)
    archive = destination / f"{name}.zip"
    pending = destination / f"{name}.zip.partial"
    try:
        write_archive(ROOT, binary, pending, name, architecture, stamp)
        from test_linux_portable import verify_archive
        report = verify_archive(pending)
        pending.rename(archive)
    finally:
        pending.unlink(missing_ok=True)
    digest = sha(archive)
    archive.with_suffix(".zip.sha256").write_text(f"{digest}  {archive.name}\n", encoding="utf-8")
    prune_previous_packages(destination, archive, f"linux-{architecture}")
    print(json.dumps(dict(report, archive=str(archive), sha256=digest), ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
