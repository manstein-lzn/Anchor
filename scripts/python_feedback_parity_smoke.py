"""Run the feedback Graph with the existing Python runtime and real provider."""

from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from anchor.runtime.secrets import load_dotenv

ROOT = Path(__file__).resolve().parents[1]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--graph", type=Path, default=ROOT / "examples/graphs/revise-loop.json")
    parser.add_argument("--observe-only", action="store_true", help="Record baseline behavior without asserting revisits")
    args = parser.parse_args()
    load_dotenv(ROOT / ".env")
    required = ("ANCHOR_MODEL_URL", "ANCHOR_MODEL_API_KEY", "ANCHOR_MODEL_NAME")
    if any(not os.environ.get(name) for name in required):
        raise SystemExit("Missing model configuration; no provider request made")
    proof = Path(tempfile.mkdtemp(prefix="python-feedback-parity-", dir=ROOT / ".local"))
    print(f"Evidence directory: {proof}", flush=True)
    workspace = proof / "workspace"
    workspace.mkdir()
    shutil.copyfile(args.graph, workspace / "graph.json")
    config = proof / "runtime.json"
    config.write_text('{"models":[]}\n')
    env = {name: os.environ[name] for name in required}
    env.update({"PATH": "/usr/bin:/bin", "PYTHONPATH": str(ROOT / "src"),
                "ANCHOR_MODEL_ALIASES": json.dumps({"models.academic": os.environ["ANCHOR_MODEL_NAME"]})})
    evidence = {"status": "failed", "provider": "real configured provider", "wire_api": "chat",
                "graph_sha256": hashlib.sha256(args.graph.read_bytes()).hexdigest(),
                "scope": "Existing Python feedback Graph; fresh isolated workspace"}
    try:
        result = subprocess.run([sys.executable, "-m", "anchor.simple", str(workspace), "--config", str(config)],
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=360, check=False)
        (proof / "host.stdout").write_bytes(result.stdout)
        (proof / "host.stderr").write_bytes(result.stderr)
        result.check_returncode()
        paths = list((workspace / "runs").glob("*/run.json"))
        assert len(paths) == 1, paths
        record = json.loads(paths[0].read_text())
        counts = dict(Counter(record["executed"]))
        reviews = sorted((item for item in record["history"].values() if item["node_id"] == "review"),
                         key=lambda item: item["pass_number"])
        matched = counts == {"draft": 2, "review": 2, "done": 1}
        evidence.update(runtime_status=record["status"], node_passes=counts,
                        review_routes=[item["route"] for item in reviews], feedback_matched=matched,
                        run_record=str(paths[0].relative_to(proof)))
        assert record["status"] == "finished", record.get("error") or record["status"]
        final = paths[0].parent / "done/final.md"
        assert final.read_text().strip()
        if not args.observe_only:
            assert matched, counts
            assert evidence["review_routes"] == ["draft", "done"]
        evidence["status"] = "observed" if args.observe_only else "passed"
    except Exception as error:
        evidence["failure"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        (proof / "evidence.json").write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n")
        print(json.dumps({"evidence": str(proof / "evidence.json"), **evidence}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
