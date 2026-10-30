"""Build and validate a native macOS portable ZIP; never copy runtime data."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import stat
import struct
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
import zipfile

from portable_contents import SHARED_FILES, collect_notices, prune_previous_packages

ROOT = Path(__file__).resolve().parent.parent
PLATFORM_FILES = ("启动中转站.command",)


def sha(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def run(command, cwd=ROOT, capture=False):
    return subprocess.run(command, cwd=cwd, check=True, text=True,
                          stdout=subprocess.PIPE if capture else None).stdout


def copy_file(source, destination, executable=False):
    if source.is_symlink() or not source.is_file():
        raise RuntimeError(f"打包来源必须是普通文件：{source}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    destination.chmod(0o755 if executable else 0o644)


def binary_details(binary):
    with binary.open("rb") as stream:
        magic, cpu = struct.unpack("<II", stream.read(8))
    architectures = {0x0100000C: ("arm64", "aarch64-apple-darwin"),
                     0x01000007: ("x64", "x86_64-apple-darwin")}
    if magic != 0xFEEDFACF or cpu not in architectures:
        raise RuntimeError("需要本机架构的 64 位 macOS Mach-O release 程序。")
    architecture, target = architectures[cpu]
    expected = "arm64" if platform.machine() == "arm64" else "x64"
    if architecture != expected:
        raise RuntimeError("release 程序与本机架构不一致，请重新构建。")
    libraries = []
    for line in run(["/usr/bin/otool", "-L", str(binary)], capture=True).splitlines()[1:]:
        library = line.strip().split(" (", 1)[0]
        if not library.startswith(("/System/Library/", "/usr/lib/")):
            raise RuntimeError(f"程序依赖非系统动态库，无法直接分享：{library}")
        libraries.append(library)
    commands = run(["/usr/bin/otool", "-l", str(binary)], capture=True)
    minimum = re.search(r"\bminos\s+([0-9.]+)", commands)
    if minimum is None:
        minimum = re.search(r"LC_VERSION_MIN_MACOSX\s+cmdsize\s+\d+\s+version\s+([0-9.]+)", commands)
    if minimum is None:
        raise RuntimeError("无法确认程序声明的最低 macOS 版本。")
    run(["/usr/bin/codesign", "--verify", "--strict", str(binary)])
    return architecture, target, minimum.group(1), libraries


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--no-reveal", action="store_true")
    args = parser.parse_args()
    if sys.platform != "darwin":
        raise RuntimeError("请在 macOS 上运行；Windows 使用 scripts/Package-Portable.ps1。")
    if sys.version_info < (3, 9):
        raise RuntimeError("打包需要 Python 3.9 或以上版本。")
    if not shutil.which("cargo"):
        raise RuntimeError("缺少 Rust/Cargo，请先安装本机 Rust 工具链。")
    if not args.skip_build:
        node = shutil.which("node")
        if not node or not run([node, "--version"], capture=True).strip().startswith("v24."):
            raise RuntimeError("重新构建需要 Node.js 24。")
        compiler = ROOT / "web/node_modules/typescript/bin/tsc"
        vite = ROOT / "web/node_modules/vite/bin/vite.js"
        if not compiler.is_file() or not vite.is_file():
            raise RuntimeError("缺少前端依赖，请先执行 pnpm install --frozen-lockfile。")
        print("正在重新构建管理界面…", flush=True)
        run([node, str(compiler), "-b"], cwd=ROOT / "web")
        run([node, str(vite), "build"], cwd=ROOT / "web")
        print("正在构建本机 release 程序，首次可能需要几分钟…", flush=True)
        run([node, str(ROOT / "scripts/cargo-with-memory.mjs"), "build", "--release", "--locked", "-j2"])
    binary = ROOT / "target/release/codex-relay"
    if not binary.is_file() or not (ROOT / "web/dist/index.html").is_file():
        raise RuntimeError("缺少 release 程序或前端构建，请重新运行打包器，不加 --skip-build。")
    architecture, target, minimum, libraries = binary_details(binary)
    version = json.loads((ROOT / "package.json").read_text(encoding="utf-8"))["version"]
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
    name = f"CodexRelay-{version}-macos-{architecture}-{stamp}"
    destination = ROOT / "dist"
    destination.mkdir(exist_ok=True)
    archive = destination / f"{name}.zip"
    pending = destination / f"{name}.zip.partial"
    try:
        with tempfile.TemporaryDirectory(prefix=".macos-package-", dir=destination) as temporary:
            stage = Path(temporary) / name
            stage.mkdir()
            copy_file(binary, stage / "codex-relay", executable=True)
            for relative in (*SHARED_FILES, *PLATFORM_FILES):
                copy_file(ROOT / relative, stage / relative, relative.endswith((".command", ".sh")))
            copy_file(ROOT / "docs/MACOS-PORTABLE.md", stage / "使用说明.md")
            web = ROOT / "web/dist"
            for source in sorted(web.rglob("*")):
                if source.is_symlink():
                    raise RuntimeError(f"管理界面不能包含符号链接：{source}")
                if source.is_file():
                    copy_file(source, stage / "web/dist" / source.relative_to(web))
            print("正在整理第三方许可和文件校验清单…", flush=True)
            dependencies = collect_notices(ROOT, stage, target)
            files = sorted(p for p in stage.rglob("*") if p.is_file())
            manifest = {"format": 1, "version": version, "target": target,
                        "architecture": architecture, "minimumMacOS": minimum,
                        "buildUtc": stamp, "configuration": "release", "systemLibraries": libraries,
                        "developerIdSigned": False, "notarized": False,
                        "files": [{"path": p.relative_to(stage).as_posix(), "sha256": sha(p),
                                   "size": p.stat().st_size, "mode": oct(stat.S_IMODE(p.stat().st_mode))}
                                  for p in files]}
            (stage / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2), encoding="utf-8")
            with zipfile.ZipFile(pending, "x", zipfile.ZIP_DEFLATED, compresslevel=9) as output:
                for path in sorted(stage.rglob("*")):
                    if path.is_file():
                        # Write only data and Unix mode; do not copy macOS extended attributes.
                        info = zipfile.ZipInfo.from_file(path, f"{name}/{path.relative_to(stage).as_posix()}")
                        info.create_system = 3
                        info.compress_type = zipfile.ZIP_DEFLATED
                        info.external_attr = (stat.S_IFREG | stat.S_IMODE(path.stat().st_mode)) << 16
                        with path.open("rb") as stream, output.open(info, "w", force_zip64=True) as target_stream:
                            shutil.copyfileobj(stream, target_stream)
        print("正在解压验证便携包（不登录、不调用模型）…", flush=True)
        from test_macos_portable import verify_archive
        validation = verify_archive(pending)
        pending.rename(archive)
    finally:
        pending.unlink(missing_ok=True)
    digest = sha(archive)
    archive.with_suffix(".zip.sha256").write_text(f"{digest}  {archive.name}\n", encoding="utf-8")
    validation.update(archive=archive.name, sha256=digest)
    archive.with_suffix(".zip.validation.json").write_text(json.dumps(validation, ensure_ascii=False, indent=2), encoding="utf-8")
    prune_previous_packages(destination, archive, f'macos-{architecture}')
    print(f"\n便携包已生成：{archive}\n大小：{archive.stat().st_size / 1024 / 1024:.1f} MB"
          f"\n架构：{architecture}；二进制最低 macOS：{minimum}"
          f"\n文件验证和启动验证通过；第三方依赖清单：{dependencies} 项。"
          "\n只需分享 ZIP；对方使用自己的 Codex 登录。", flush=True)
    if not args.no_reveal:
        subprocess.run(["/usr/bin/open", "-R", str(archive)], check=False)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError, zipfile.BadZipFile) as error:
        print(f"打包失败：{error}", file=sys.stderr)
        sys.exit(1)
