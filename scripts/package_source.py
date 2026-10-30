"""Package current source for macOS/Windows development, with or without Git."""
import hashlib
import json
import os
import stat
import subprocess
import zipfile
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath, PureWindowsPath

ROOT = Path(__file__).resolve().parent.parent
SOURCE_DIRS = {"crates", "web", "scripts", "examples", "legacy", "docs", ".github", ".cargo"}
ROOT_FILES = {
    "relay.config.json",
    "AGENTS.md", "Cargo.toml", "Cargo.lock", "package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml",
    ".gitignore", ".gitattributes", ".npmrc", ".nvmrc", ".node-version", "rust-toolchain.toml",
    "启动中转站.cmd", "打包便携版.cmd", "启动中转站.command", "打包便携版.command",
}
EXCLUDED_DIRS = {
    ".git", ".cache", "data", "target", "node_modules", "dist", "dist-legacy", "__pycache__",
}
EXCLUDED_NAMES = {
    "auth.json", "bridge-auth.json", "bridge-token.json", "config.toml", "codex-relay", "codex-bridge",
    ".ds_store", "source-manifest.json", "source-origin-manifest.json",
}
EXCLUDED_SUFFIXES = {
    ".exe", ".dll", ".pdb", ".dylib", ".so", ".a", ".lib", ".o", ".obj",
    ".db", ".sqlite", ".sqlite3", ".pem", ".key", ".p12", ".pfx", ".dmp",
    ".log", ".pyc", ".pyo", ".tsbuildinfo", ".zip", ".gz", ".tgz", ".7z", ".bundle",
}


def eligible(name):
    path = PurePosixPath(name)
    return (
        not path.is_absolute()
        and ".." not in path.parts
        and not any(part.lower() in EXCLUDED_DIRS for part in path.parts)
        and path.name.lower() not in EXCLUDED_NAMES
        and (not path.name.startswith(".env") or path.name == ".env.example")
        and path.suffix.lower() not in EXCLUDED_SUFFIXES
        and not path.name.lower().endswith(("-wal", "-shm"))
    )


def git_output(*args):
    try:
        result = subprocess.run(["git", *args], cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        return result.stdout if result.returncode == 0 else None
    except FileNotFoundError:
        return None


def source_names():
    top = git_output("rev-parse", "--show-toplevel")
    if top and Path(os.fsdecode(top).strip()).resolve() == ROOT:
        names = git_output("ls-files", "--cached", "--others", "--exclude-standard", "-z")
        if names is None:
            raise RuntimeError("Cannot enumerate Git working-tree files")
        commit = git_output("rev-parse", "HEAD")
        return [os.fsdecode(name) for name in names.split(b"\0") if name], commit.decode().strip() if commit else None, "git-working-tree"
    # A handoff ZIP has no .git. Walk source folders without visiting runtime data.
    names = []
    for path in sorted(ROOT.iterdir()):
        if path.name in SOURCE_DIRS:
            if path.is_symlink():
                raise RuntimeError(f"Source directory must not be a symlink: {path.name}")
            if not path.is_dir():
                continue
            for current, directories, files in os.walk(path, followlinks=False):
                directories[:] = sorted(d for d in directories if d.lower() not in EXCLUDED_DIRS)
                for directory in directories:
                    if (Path(current) / directory).is_symlink():
                        raise RuntimeError(f"Source directory must not be a symlink: {Path(current) / directory}")
                names.extend((Path(current) / name).relative_to(ROOT).as_posix() for name in files)
        elif path.name in ROOT_FILES or path.name.upper().startswith(("README", "LICENSE", "LICENCE", "COPYING", "NOTICE")):
            names.append(path.name)
    return names, None, "filesystem-source-snapshot"


def windows_safe(name):
    path = PureWindowsPath(name)
    return (not path.is_absolute() and not path.drive and "\\" not in name
            and all(not any(c in part for c in '<>:"|?*') and not part.endswith((".", " "))
                    and not PureWindowsPath(part).is_reserved() for part in path.parts))


def main():
    names, commit, method = source_names()
    entries, seen = [], set()
    for name in sorted(set(names)):
        path = ROOT / name
        if not eligible(name) or not path.is_file():
            continue
        if path.is_symlink() or not path.resolve().is_relative_to(ROOT):
            raise RuntimeError(f"Source must be an ordinary workspace file: {name}")
        if not windows_safe(name) or name.casefold() in seen:
            raise RuntimeError(f"Source path is incompatible with Windows: {name}")
        seen.add(name.casefold())
        # Windows does not preserve Unix executable bits for macOS launchers.
        mode = 0o755 if path.suffix in {".command", ".sh"} or path.stat().st_mode & stat.S_IXUSR else 0o644
        entries.append((name, path.read_bytes(), mode))
    # Preserve provenance without overwriting the workspace's historical manifest.
    origin = ROOT / "SOURCE-ORIGIN-MANIFEST.json"
    if not origin.exists():
        origin = ROOT / "SOURCE-MANIFEST.json"
    if origin.is_file():
        if origin.is_symlink():
            raise RuntimeError("Origin manifest must not be a symlink")
        entries.append(("SOURCE-ORIGIN-MANIFEST.json", origin.read_bytes(), 0o644))
    required = {"Cargo.toml", "Cargo.lock", "package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml",
                "crates/relay/src/platform.rs", "crates/relay/src/platform/macos.rs",
                "web/src/App.tsx", "scripts/Start-Relay.ps1", "启动中转站.cmd",
                "docs/WINDOWS-DEVELOPMENT.md", "docs/MACOS-DEVELOPMENT.md",
                "docs/MACOS-PORTABLE.md", "打包便携版.command", "启动中转站.command",
                "scripts/package_macos.py", "scripts/test_macos_portable.py",
                "scripts/Start-Relay.sh", "scripts/Prepare-Relay.sh"}
    if not required.issubset({name for name, _, _ in entries}):
        raise RuntimeError("Required source, lockfiles or Windows/macOS build files are missing")
    created = datetime.now(timezone.utc)
    basename = f"CodexRelay-source-windows-macos-{created:%Y%m%dT%H%M%S%fZ}"
    destination = ROOT / "dist"
    destination.mkdir(exist_ok=True)
    archive_path = destination / f"{basename}.zip"
    pending_path = destination / f"{basename}.zip.partial"
    manifest = {
        "format": 2, "createdAt": created.isoformat(), "baseCommit": commit,
        "collectionMethod": method,
        "source": "Current source bytes, including uncommitted changes; no Git history",
        "buildsIncluded": False, "runtimeDataIncluded": False, "gitHistoryIncluded": False,
        "handoffGuide": "docs/WINDOWS-DEVELOPMENT.md",
        "windowsValidation": "Requires build and native acceptance on the receiving Windows computer",
        "files": [
            {"path": name, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest(), "mode": oct(mode)}
            for name, data, mode in entries
        ],
    }
    try:
        with zipfile.ZipFile(pending_path, "x", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
            generated = ("SOURCE-MANIFEST.json", json.dumps(manifest, ensure_ascii=False, indent=2).encode("utf-8"), 0o644)
            for name, data, mode in entries + [generated]:
                info = zipfile.ZipInfo(f"codex-relay/{name}", created.timetuple()[:6])
                info.create_system = 3
                info.external_attr = (stat.S_IFREG | mode) << 16
                archive.writestr(info, data, compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)
        with zipfile.ZipFile(pending_path) as archive:
            if archive.testzip() is not None:
                raise RuntimeError("Archive CRC verification failed")
            if len(archive.namelist()) != len(entries) + 1:
                raise RuntimeError("Unexpected archive contents")
            for entry in manifest["files"]:
                info = archive.getinfo(f"codex-relay/{entry['path']}")
                data = archive.read(info)
                if (len(data) != entry["bytes"] or hashlib.sha256(data).hexdigest() != entry["sha256"]
                        or stat.S_IMODE(info.external_attr >> 16) != int(entry["mode"], 8)):
                    raise RuntimeError(f"Archive manifest mismatch: {entry['path']}")
        pending_path.rename(archive_path)
    finally:
        pending_path.unlink(missing_ok=True)
    digest = hashlib.sha256(archive_path.read_bytes()).hexdigest()
    archive_path.with_suffix(".zip.sha256").write_text(f"{digest}  {archive_path.name}\n", encoding="utf-8")
    print(json.dumps({"archive": str(archive_path), "files": len(entries), "bytes": archive_path.stat().st_size,
                      "sha256": digest, "collectionMethod": method}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
