"""Run a repeatable installed-client roundtrip matrix using synthetic localhost models."""
import argparse
import json
from pathlib import Path
import subprocess
import sys
import uuid

from native_cache_audit_cases import CASES

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--cases", nargs="+", choices=CASES, default=CASES)
    args = parser.parse_args()
    folder = ROOT / ".cache" / ("native-cache-audit-" + uuid.uuid4().hex)
    folder.mkdir()
    report = {"real_model_calls": 0, "cases": []}
    for case in args.cases:
        result = subprocess.run([sys.executable, str(ROOT / "scripts/test_account_concurrency.py"),
            "--binary", str(args.binary.resolve()), "--desktop-cache-only", "--desktop-case", case],
            cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding="utf-8",
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        (folder / (case + ".log")).write_text(result.stdout, encoding="utf-8")
        entry = {"case": case, "passed": result.returncode == 0}
        for line in result.stdout.splitlines():
            if line.startswith('{"real_model_calls"'):
                entry["evidence"] = json.loads(line)["evidence"]
        if not entry["passed"]:
            entry["failure"] = result.stdout[-2500:]
        report["cases"].append(entry)
        (folder / "result.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(json.dumps(entry), flush=True)
    print(json.dumps({"report": str(folder / "result.json"), "passed": sum(c["passed"] for c in report["cases"]),
                      "total": len(report["cases"])}), flush=True)
    return 0 if all(case["passed"] for case in report["cases"]) else 1


if __name__ == "__main__":
    sys.exit(main())
