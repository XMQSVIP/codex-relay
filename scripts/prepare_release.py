"""Gate automatic main releases on matching versions and successful source checks."""
import argparse
import json
import os
from pathlib import Path
import re
import time
from urllib.error import HTTPError
from urllib.parse import quote, urlencode
from urllib.request import Request, urlopen

from check_versions import check_versions

ROOT = Path(__file__).resolve().parent.parent


def api(path, method="GET", data=None):
    request = Request(
        f"{os.environ.get('GITHUB_API_URL', 'https://api.github.com')}/repos/{os.environ['GH_REPO']}/{path}",
        data=json.dumps(data).encode() if data is not None else None,
        method=method,
        headers={"Authorization": f"Bearer {os.environ['GH_TOKEN']}",
                 "Accept": "application/vnd.github+json", "User-Agent": "codex-relay-release",
                 "Content-Type": "application/json"},
    )
    try:
        with urlopen(request, timeout=30) as response:
            return json.load(response)
    except HTTPError as error:
        if error.code == 404 and method == "GET":
            return None
        raise


def current_main(call, sha):
    branch = call("git/ref/heads/main")
    return branch is not None and branch["object"]["sha"] == sha


def tagged_commit(call, tag):
    ref = call(f"git/ref/tags/{quote(tag, safe='')}")
    if ref is None:
        return None
    obj = ref["object"]
    for _ in range(10):
        if obj["type"] == "commit":
            return obj["sha"]
        if obj["type"] != "tag":
            break
        obj = call(f"git/tags/{obj['sha']}")["object"]
    raise RuntimeError("Release tag does not resolve to a commit")


def prepare(root, ref, sha, call=api, sleep=time.sleep, clock=time.monotonic, timeout=1800):
    version = check_versions(root, ref)
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", version):
        raise ValueError("Release version must use semantic version syntax")
    tag = f"v{version}"
    if ref == f"refs/tags/{tag}":
        return tag
    if ref != "refs/heads/main" or not current_main(call, sha):
        return None
    if call(f"releases/tags/{quote(tag, safe='')}") is not None:
        return None
    existing = tagged_commit(call, tag)
    if existing is not None and existing != sha:
        # Released versions are immutable, including tags whose publication is in progress.
        return None
    query = urlencode({"branch": "main", "event": "push", "head_sha": sha, "per_page": 100})
    deadline = clock() + timeout
    while True:
        if not current_main(call, sha):
            return None
        document = call(f"actions/workflows/checks.yml/runs?{query}")
        if document is None:
            raise RuntimeError("Development checks workflow was not found")
        runs = [run for run in document["workflow_runs"]
                if run["head_sha"] == sha and run["head_branch"] == "main" and run["event"] == "push"]
        latest = max(runs, key=lambda run: run["id"], default=None)
        if latest is not None and latest["status"] == "completed":
            if latest["conclusion"] != "success":
                raise RuntimeError(f"Development checks did not pass: {latest['conclusion']}")
            break
        if clock() >= deadline:
            raise RuntimeError("Timed out waiting for Development checks on this commit")
        print("Waiting for Development checks on the packaged commit...", flush=True)
        sleep(15)
    return tag if current_main(call, sha) else None


def create_tag(tag, sha, call=api):
    if not current_main(call, sha):
        raise RuntimeError("main changed before publication; no tag was created")
    existing = tagged_commit(call, tag)
    if existing is not None:
        if existing != sha:
            raise RuntimeError("Release tag already belongs to another commit")
        return
    call("git/refs", "POST", {"ref": f"refs/tags/{tag}", "sha": sha})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--create-tag", action="store_true")
    args = parser.parse_args()
    ref, sha = os.environ["GITHUB_REF"], os.environ["SOURCE_SHA"]
    if args.create_tag:
        if ref != "refs/heads/main":
            raise RuntimeError("Only main publication can create an automatic tag")
        tag = f"v{check_versions(ROOT, ref)}"
        if tag != os.environ["REF_NAME"]:
            raise RuntimeError("Prepared release tag no longer matches project versions")
        create_tag(tag, sha)
    else:
        tag = prepare(ROOT, ref, sha)
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
            output.write(f"publish={'true' if tag else 'false'}\ntag={tag or ''}\n")
        print(f"Prepared {tag}" if tag else "No unpublished version for this main commit; skipping release")


if __name__ == "__main__":
    main()
