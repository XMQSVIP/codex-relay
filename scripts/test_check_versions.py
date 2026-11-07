"""Version-check regressions use isolated manifests and never modify the checkout."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

import check_versions


class CheckVersionsTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.write("package.json", json.dumps({"version": "0.2.2"}))
        self.write("web/package.json", json.dumps({"version": "0.2.2"}))
        self.write("crates/relay/Cargo.toml", '[package]\nname = "codex-relay"\nversion = "0.2.2"\n')
        self.write("Cargo.lock", 'version = 4\n\n[[package]]\nname = "codex-relay"\nversion = "0.2.2"\n')

    def write(self, relative, content):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    def test_matching_versions_accept_release_branch_and_pull_request_refs(self):
        before = {path: path.read_bytes() for path in self.root.rglob("*") if path.is_file()}
        for ref in ("", "refs/tags/v0.2.2", "refs/heads/main", "refs/heads/feature", "refs/pull/42/merge"):
            with self.subTest(ref=ref):
                self.assertEqual(check_versions.check_versions(self.root, ref), "0.2.2")
        self.assertEqual(before, {path: path.read_bytes() for path in self.root.rglob("*") if path.is_file()})

    def test_each_manifest_and_lock_mismatch_identifies_all_versions(self):
        for relative in ("package.json", "web/package.json", "crates/relay/Cargo.toml", "Cargo.lock"):
            with self.subTest(relative=relative):
                path = self.root / relative
                original = path.read_text(encoding="utf-8")
                path.write_text(original.replace("0.2.2", "0.2.3"), encoding="utf-8")
                with self.assertRaisesRegex(ValueError, "Project versions do not match") as failure:
                    check_versions.check_versions(self.root)
                self.assertIn(relative + ": 0.2.3", str(failure.exception))
                self.assertIn("0.2.2", str(failure.exception))
                path.write_text(original, encoding="utf-8")

    def test_lock_requires_exactly_one_relay_package(self):
        entry = '[[package]]\nname = "codex-relay"\nversion = "0.2.2"\n'
        for count in (0, 2):
            with self.subTest(count=count):
                self.write("Cargo.lock", 'version = 4\n' + entry * count)
                with self.assertRaisesRegex(ValueError, f"exactly one codex-relay package, found {count}"):
                    check_versions.check_versions(self.root)

    def test_release_tags_require_exact_prefix_and_version(self):
        for ref in ("refs/tags/v0.2.1", "refs/tags/0.2.2", "refs/tags/v0.2.2-extra", "refs/tags/development"):
            with self.subTest(ref=ref):
                with self.assertRaisesRegex(ValueError, "expected 'refs/tags/v0.2.2'"):
                    check_versions.check_versions(self.root, ref)

    def test_missing_invalid_or_empty_manifests_report_the_file(self):
        for relative, content in (("package.json", "[]"), ("web/package.json", '{"version":""}'),
                                  ("crates/relay/Cargo.toml", "not valid TOML"),
                                  ("Cargo.lock", None)):
            with self.subTest(relative=relative):
                path = self.root / relative
                original = path.read_text(encoding="utf-8")
                if content is None:
                    path.unlink()
                else:
                    path.write_text(content, encoding="utf-8")
                with self.assertRaises(ValueError) as failure:
                    check_versions.check_versions(self.root)
                self.assertIn(relative, str(failure.exception))
                path.write_text(original, encoding="utf-8")

    def test_cargo_manifest_must_name_the_relay_package(self):
        self.write("crates/relay/Cargo.toml", '[package]\nname = "different-package"\nversion = "0.2.2"\n')
        with self.assertRaisesRegex(ValueError, "expected a \\[package\\] named codex-relay"):
            check_versions.check_versions(self.root)

    def test_cli_uses_its_checkout_not_cwd_and_ref_argument_overrides_environment(self):
        script = self.root / "scripts/check_versions.py"
        script.parent.mkdir()
        shutil.copyfile(check_versions.__file__, script)
        environment = {**os.environ, "GITHUB_REF": "refs/tags/v0.2.1"}
        command = [sys.executable, str(script)]
        failed = subprocess.run(command, cwd=self.root.parent, env=environment,
                                capture_output=True, text=True)
        self.assertEqual(failed.returncode, 1)
        self.assertIn("Version check failed", failed.stderr)
        self.assertNotIn("Traceback", failed.stderr)
        passed = subprocess.run(command + ["--ref", "refs/tags/v0.2.2"], cwd=self.root.parent,
                                env=environment, capture_output=True, text=True)
        self.assertEqual(passed.returncode, 0, passed.stderr)
        self.assertIn("Project versions agree: 0.2.2", passed.stdout)


if __name__ == "__main__":
    unittest.main()
