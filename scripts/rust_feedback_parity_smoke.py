"""Real-provider acceptance of a repository feedback Graph copied verbatim to Rust."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import struct
import subprocess
import tempfile

from anchor.runtime.secrets import load_dotenv


ROOT = Path(__file__).resolve().parents[1]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "rust/target/release/anchor-runner-host")
    parser.add_argument("--graph", type=Path, default=ROOT / "examples/graphs/revise-loop.json")
    parser.add_argument("--wire-api", choices=("chat", "responses"))
    args = parser.parse_args()
    load_dotenv(ROOT / ".env")
    required = ("ANCHOR_MODEL_URL", "ANCHOR_MODEL_API_KEY", "ANCHOR_MODEL_NAME")
    if any(not os.environ.get(name) for name in required):
        raise SystemExit("Missing model configuration; no provider request made")
    (ROOT / ".local").mkdir(exist_ok=True)
    proof = Path(tempfile.mkdtemp(prefix="rust-feedback-parity-", dir=ROOT / ".local"))
    print(f"Evidence directory: {proof}", flush=True)
    source = args.graph
    bundle = proof / "bundle"
    bundle.mkdir()
    shutil.copyfile(source, bundle / "graph.json")
    (bundle / "manifest.json").write_text('{"format":1,"graph":"graph.json","plugins":[]}')
    state = proof / "state"
    env = {name: os.environ[name] for name in required}
    env.update({
        "PATH": "/usr/bin:/bin",
        "ANCHOR_MODEL_WIRE_API": args.wire_api or os.environ.get("ANCHOR_MODEL_WIRE_API", "responses"),
        "ANCHOR_MODEL_ALIASES": json.dumps({"models.academic": os.environ["ANCHOR_MODEL_NAME"]}),
        "ANCHOR_RUNNER_BUNDLE_ROOT": str(bundle),
        "ANCHOR_RUNNER_GRAPH_NAME": "revise-loop",
        "ANCHOR_RUNNER_STATE_ROOT": str(state),
        "ANCHOR_RUNNER_WORKSPACE_ROOT": str(proof / "workspaces"),
        "ANCHOR_RUNNER_ALLOWED_COMMANDS": "sh,cat,ls,find,sed,grep,head,tail,wc,cp,printf,git",
    })
    request = json.dumps({"op": "start_bundle", "version": 1, "request_id": "feedback",
                          "run_id": "feedback", "input": {}}).encode()
    evidence = {"status": "failed", "provider": "real configured provider", "graph_unchanged": True,
                "wire_api": env["ANCHOR_MODEL_WIRE_API"],
                "graph_sha256": hashlib.sha256(source.read_bytes()).hexdigest()}
    try:
        result = subprocess.run([str(args.binary.resolve())], input=struct.pack(">I", len(request)) + request,
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=360, check=False)
        (proof / "host.stderr").write_bytes(result.stderr)
        result.check_returncode()
        response = json.loads(result.stdout[4:])
        (proof / "response.json").write_text(json.dumps(response, indent=2))
        assert response.get("status") == "completed", response
        record = json.loads((state / "runs/feedback.json").read_text())
        counts = {node: len(results) for node, results in record["results"].items()}
        evidence.update(runtime_status=response.get("status"), node_passes=counts)
        assert counts == {"draft": 2, "review": 2, "done": 1}, counts
        reviews = record["results"]["review"]
        assert [item["completion"]["route"] for item in reviews] == ["draft", "done"]
        first_review = state / "artifacts" / reviews[0]["commit"]["id"] / "files/review.md"
        second_review = state / "artifacts" / reviews[1]["commit"]["id"] / "files/review.md"
        assert first_review.read_text().strip() and second_review.read_text().strip()
        final_commit = record["results"]["done"][0]["commit"]["id"]
        final = state / "artifacts" / final_commit / "files/final.md"
        assert final.read_text().strip()
        providers = []
        for database in (state / "io-harness/store").glob("*.sqlite3"):
            with sqlite3.connect(database) as store:
                providers.extend(store.execute("SELECT model FROM provider_calls").fetchall())
        assert providers
        assert (bundle / "graph.json").read_bytes() == source.read_bytes()
        evidence.update(status="passed", node_passes=counts, provider_requests=len(providers),
                        provider_models=sorted({row[0] for row in providers if row[0]}),
                        final_artifact=str(final.relative_to(proof)),
                        scope="Repository feedback Graph copied verbatim, real provider, revisit files, native completion and immutable artifacts; not full business workflow acceptance")
    except Exception as error:
        evidence["failure"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        (proof / "evidence.json").write_text(json.dumps(evidence, ensure_ascii=False, indent=2))
        print(json.dumps({"evidence": str(proof / "evidence.json"), **evidence}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
