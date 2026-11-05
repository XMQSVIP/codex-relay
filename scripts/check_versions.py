"""Check project versions and require release tags to match the packaged version."""
import argparse
import json
import os
from pathlib import Path
import sys
import tomllib


ROOT = Path(__file__).resolve().parent.parent


def load_document(root, relative, parser):
    try:
        document = parser((root / relative).read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise ValueError(f"{relative}: cannot read a valid manifest: {error}") from error
    if not isinstance(document, dict):
        raise ValueError(f"{relative}: manifest must contain an object or table")
    return document


def version_value(value, source):
    if not isinstance(value, str) or not value or value != value.strip():
        raise ValueError(f"{source}: version must be a non-empty string without surrounding whitespace")
    return value


def check_versions(root, ref=""):
    versions = {}
    for relative in ("package.json", "web/package.json"):
        document = load_document(root, relative, json.loads)
        versions[relative] = version_value(document.get("version"), relative)

    relative = "crates/relay/Cargo.toml"
    manifest = load_document(root, relative, tomllib.loads)
    package = manifest.get("package")
    if not isinstance(package, dict) or package.get("name") != "codex-relay":
        raise ValueError(f"{relative}: expected a [package] named codex-relay")
    versions[relative] = version_value(package.get("version"), relative)

    lock = load_document(root, "Cargo.lock", tomllib.loads)
    packages = lock.get("package", [])
    if not isinstance(packages, list):
        raise ValueError("Cargo.lock: expected a [[package]] array")
    relay = [package for package in packages
             if isinstance(package, dict) and package.get("name") == "codex-relay"]
    if len(relay) != 1:
        raise ValueError(f"Cargo.lock: expected exactly one codex-relay package, found {len(relay)}")
    versions["Cargo.lock"] = version_value(relay[0].get("version"), "Cargo.lock / codex-relay")

    if len(set(versions.values())) != 1:
        details = "\n".join(f"  {source}: {version}" for source, version in versions.items())
        raise ValueError(f"Project versions do not match:\n{details}")
    version = versions["package.json"]
    if ref.startswith("refs/tags/") and ref != f"refs/tags/v{version}":
        raise ValueError(f"Release tag {ref!r} does not match project version; expected 'refs/tags/v{version}'")
    return version


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ref", default=os.environ.get("GITHUB_REF", ""),
                        help="Git ref to validate (defaults to GITHUB_REF)")
    args = parser.parse_args(argv)
    try:
        version = check_versions(ROOT, args.ref)
    except ValueError as error:
        print(f"Version check failed: {error}", file=sys.stderr)
        return 1
    print(f"Project versions agree: {version}" + (f" ({args.ref})" if args.ref else ""))
    return 0


if __name__ == "__main__":
    sys.exit(main())
