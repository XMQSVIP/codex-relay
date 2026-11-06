"""Release gates use isolated manifests and a fake GitHub API; never publish."""
import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest

from prepare_release import create_tag, prepare, tagged_commit


class PrepareReleaseTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        for relative, content in {
            "package.json": '{"version":"1.2.3"}',
            "web/package.json": '{"version":"1.2.3"}',
            "crates/relay/Cargo.toml": '[package]\nname="codex-relay"\nversion="1.2.3"\n',
            "Cargo.lock": 'version=4\n[[package]]\nname="codex-relay"\nversion="1.2.3"\n',
        }.items():
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8")
        self.sha = "a" * 40
        self.head = self.sha
        self.release = None
        self.tag = None
        self.runs = [self.run_record()]
        self.calls = []

    def run_record(self, **changes):
        return dict(id=5, head_sha=self.sha, head_branch="main", event="push",
                    status="completed", conclusion="success") | changes

    def api(self, path, method="GET", data=None):
        self.calls.append((path, method, data))
        if path == "git/ref/heads/main":
            return {"object": {"sha": self.head}}
        if path == "releases/tags/v1.2.3":
            return self.release
        if path == "git/ref/tags/v1.2.3":
            return None if self.tag is None else {"object": {"type": "commit", "sha": self.tag}}
        if path.startswith("actions/workflows/checks.yml/runs?"):
            self.assertIn("head_sha=" + self.sha, path)
            return {"workflow_runs": self.runs}
        if path == "git/refs" and method == "POST":
            self.tag = data["sha"]
            return {"ref": data["ref"]}
        self.fail(f"Unexpected API request: {path}, {method}")

    def prepare(self, **kwargs):
        with contextlib.redirect_stdout(io.StringIO()):
            return prepare(self.root, "refs/heads/main", self.sha, self.api, **kwargs)

    def test_success_requires_checks_for_exact_main_commit_and_does_not_create_tag(self):
        self.runs.extend([self.run_record(id=99, head_sha="b" * 40, conclusion="failure"),
                          self.run_record(id=100, head_branch="feature", conclusion="failure"),
                          self.run_record(id=101, event="pull_request", conclusion="failure")])
        self.assertEqual(self.prepare(), "v1.2.3")
        self.assertIsNone(self.tag)
        self.assertTrue(all(method == "GET" for _, method, _ in self.calls))

    def test_existing_release_or_another_tag_commit_is_never_overwritten(self):
        self.release = {"id": 1}
        self.assertIsNone(self.prepare())
        self.release = None
        self.tag = "b" * 40
        self.assertIsNone(self.prepare())
        self.assertFalse(any(path.startswith("actions/") for path, _, _ in self.calls))

    def test_unpublished_tag_on_same_commit_can_resume(self):
        self.tag = self.sha
        self.assertEqual(self.prepare(), "v1.2.3")

    def test_latest_check_attempt_must_pass(self):
        for conclusion in ("failure", "cancelled", "timed_out", "skipped"):
            with self.subTest(conclusion=conclusion):
                self.runs = [self.run_record(id=4), self.run_record(conclusion=conclusion)]
                with self.assertRaisesRegex(RuntimeError, "did not pass"):
                    self.prepare()
                self.assertIsNone(self.tag)

    def test_pending_or_missing_checks_wait_and_timeout_without_tagging(self):
        for runs in ([], [self.run_record(status="in_progress", conclusion=None)]):
            self.runs = runs
            ticks = iter((0, 0, 1801))
            sleeps = []
            with self.assertRaisesRegex(RuntimeError, "Timed out"):
                self.prepare(sleep=sleeps.append, clock=lambda: next(ticks))
            self.assertEqual(sleeps, [15])
            self.assertIsNone(self.tag)

    def test_pending_checks_can_complete_but_a_new_main_commit_is_skipped(self):
        self.runs = [self.run_record(status="in_progress", conclusion=None)]
        def complete(_):
            self.runs = [self.run_record()]
        self.assertEqual(self.prepare(sleep=complete), "v1.2.3")
        self.head = "b" * 40
        self.assertIsNone(self.prepare())
        self.head = self.sha
        self.runs = [self.run_record(status="in_progress", conclusion=None)]
        def advance(_):
            complete(0)
            self.head = "b" * 40
        self.assertIsNone(self.prepare(sleep=advance))

    def test_manual_tags_keep_version_guard_and_other_branches_cannot_publish(self):
        self.assertEqual(prepare(self.root, "refs/tags/v1.2.3", self.sha, self.api), "v1.2.3")
        self.assertIsNone(prepare(self.root, "refs/heads/feature", self.sha, self.api))
        self.assertEqual(self.calls, [])
        with self.assertRaises(ValueError):
            prepare(self.root, "refs/tags/v1.2.4", self.sha, self.api)

    def test_version_mismatch_and_output_injection_are_rejected(self):
        (self.root / "package.json").write_text('{"version":"1.2.4"}', encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "do not match"):
            self.prepare()
        for path in ("package.json", "web/package.json"):
            (self.root / path).write_text(json.dumps({"version": "1.2.3\npublish=true"}), encoding="utf-8")
        for path in ("crates/relay/Cargo.toml", "Cargo.lock"):
            text = (self.root / path).read_text(encoding="utf-8")
            (self.root / path).write_text(text.replace('"1.2.3"', '"1.2.3\\npublish=true"'), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "semantic version"):
            self.prepare()

    def test_tag_creation_is_idempotent_and_rejects_stale_or_conflicting_commits(self):
        create_tag("v1.2.3", self.sha, self.api)
        create_tag("v1.2.3", self.sha, self.api)
        self.assertEqual(sum(method == "POST" for _, method, _ in self.calls), 1)
        self.tag = "b" * 40
        with self.assertRaisesRegex(RuntimeError, "another commit"):
            create_tag("v1.2.3", self.sha, self.api)
        self.head = "b" * 40
        with self.assertRaisesRegex(RuntimeError, "main changed"):
            create_tag("v1.2.3", self.sha, self.api)

    def test_annotated_tag_resolves_to_its_commit(self):
        records = {"git/ref/tags/v1.2.3": {"object": {"type": "tag", "sha": "annotated"}},
                   "git/tags/annotated": {"object": {"type": "commit", "sha": self.sha}}}
        self.assertEqual(tagged_commit(records.get, "v1.2.3"), self.sha)
        records["git/tags/annotated"]["object"]["type"] = "tree"
        with self.assertRaisesRegex(RuntimeError, "does not resolve"):
            tagged_commit(records.get, "v1.2.3")


if __name__ == "__main__":
    unittest.main()
