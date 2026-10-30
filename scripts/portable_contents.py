"""Shared portable payload and compact, lossless third-party notices."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess


# Portable runtime only; full documentation and examples stay in the source tree.
SHARED_FILES = ("relay.config.json",)


def prune_previous_packages(destination, archive, platform):
    """Keep the new release and remove only recognized outputs for this platform."""
    destination = destination.resolve()
    pattern = re.compile(r"CodexRelay-.+-" + re.escape(platform)
                         + r"-\d{8}T\d{6}(?:\d{6})?Z(?:\.zip(?:\.(?:sha256|validation\.json))?)?")
    current = {archive.name, archive.name + '.sha256', archive.name + '.validation.json'}
    candidates = [path for path in destination.iterdir()
                  if pattern.fullmatch(path.name) and path.name not in current]
    # Validate every deletion before removing anything; never follow links out of dist.
    for path in candidates:
        if path.is_symlink() or path.resolve().parent != destination:
            raise RuntimeError(f"Refusing to remove package outside output directory: {path}")
    for path in candidates:
        # Python/Windows can enumerate a long cache path but fail to unlink it
        # without the extended-length prefix. Add it only after the safety checks.
        if os.name == 'nt':
            absolute = str(path)
            if not absolute.startswith('\\\\?\\'):
                absolute = ('\\\\?\\UNC\\' + absolute[2:] if absolute.startswith('\\\\')
                            else '\\\\?\\' + absolute)
            path = Path(absolute)
        if path.is_dir() and path.name.endswith('Z'):
            shutil.rmtree(path)
        elif path.is_file():
            path.unlink()


def collect_notices(root, stage, target):
    metadata = json.loads(subprocess.check_output([
        "cargo", "metadata", "--locked", "--offline", "--filter-platform", target,
        "--format-version", "1",
    ], cwd=root))
    sources = [("rust", package, Path(package["manifest_path"]).parent)
               for package in metadata["packages"] if package["source"]]
    store = root / "node_modules/.pnpm"
    if not store.is_dir():
        raise RuntimeError("Missing license sources; run pnpm install --frozen-lockfile first.")
    for pattern in ("*/node_modules/*/package.json", "*/node_modules/@*/*/package.json"):
        for manifest in sorted(store.glob(pattern)):
            sources.append(("javascript", json.loads(manifest.read_text(encoding="utf-8")), manifest.parent))
    return write_notices(stage, sources)


def write_notices(stage, sources):
    notices = stage / "third-party"
    notices.mkdir()
    packages, seen = [], set()
    # Preserve original bytes, including copyright notices and line endings.
    # Byte ranges in the inventory allow each original file to be verified/extracted.
    with (notices / "LICENSES.txt").open("wb") as output:
        output.write(b"Third-party notices. Inventory includes build/test dependencies.\n"
                     b"Original license files follow verbatim. Official Codex binaries are not included.\n")
        for ecosystem, package, directory in sorted(sources, key=lambda item: (
                item[0], item[1]["name"], item[1]["version"])):
            identity = (ecosystem, package["name"], package["version"])
            if identity in seen:
                continue
            seen.add(identity)
            candidates = {p for p in directory.iterdir() if p.is_file()
                          and p.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE"))}
            if ecosystem == "rust" and package.get("license_file"):
                candidates.add(directory / package["license_file"])
            originals = []
            for source in sorted(candidates):
                if not source.is_file():
                    continue
                data = source.read_bytes()
                heading = "\n\n===== " + " / ".join((*identity, source.name)) + " =====\n"
                output.write(heading.encode("utf-8"))
                originals.append({"name": source.name, "offset": output.tell(), "size": len(data),
                                  "sha256": hashlib.sha256(data).hexdigest()})
                output.write(data)
            packages.append({"ecosystem": ecosystem, "name": package["name"],
                             "version": package["version"], "license": package.get("license"),
                             "texts": ["third-party/LICENSES.txt"] if originals else [],
                             "licenseFiles": originals})
    (notices / "dependencies.json").write_text(
        json.dumps(packages, ensure_ascii=False, indent=2), encoding="utf-8")
    return len(packages)
