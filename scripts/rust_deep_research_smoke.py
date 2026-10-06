"""Run the unchanged deep academic research Graph with the configured provider.

The run uses disposable bundle, state, workspace, and local-input roots.  It
does not publish a page or send a channel message.  On timeout or provider
failure the durable Run, Harness records, and partial Artifacts are retained
in the evidence directory for review.
"""

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
from collections import Counter

from anchor.runtime.secrets import load_dotenv


ROOT = Path(__file__).resolve().parents[1]
SOURCE_GRAPH = ROOT / "examples/graphs/deep-academic-research.json"
SOURCE_PLUGIN = ROOT / "plugins/academic-research"


def _digest_plugin(plugin: Path) -> tuple[str, list[str]]:
    resources = sorted(path.relative_to(plugin).as_posix() for path in plugin.rglob("*") if path.is_file())
    digest = hashlib.sha256()
    for name in resources:
        digest.update(name.encode())
        digest.update(hashlib.sha256((plugin / name).read_bytes()).digest())
    return digest.hexdigest(), resources


def _records(state: Path) -> dict:
    runs = state / "runs/deep-academic-research.json"
    if not runs.exists():
        return {}
    return json.loads(runs.read_text())


def _provider_trace(state: Path) -> dict:
    stores = sorted((state / "io-harness/store").glob("*.sqlite3"))
    trace = {"stores": [str(path) for path in stores], "provider_calls": [], "tool_calls": [], "observations": []}
    for path in stores:
        with sqlite3.connect(path) as db:
            trace["provider_calls"].extend(
                {
                    "store": str(path),
                    "id": row[0],
                    "step": row[1],
                    "attempt": row[2],
                    "model": row[3],
                    "finish_reason": row[4],
                    "failure": row[5],
                }
                for row in db.execute(
                    "SELECT id, step, attempt, model, finish_reason, failure "
                    "FROM provider_calls ORDER BY id"
                )
            )
            for step, serialized in db.execute("SELECT step, calls FROM step_turns ORDER BY step"):
                for call in json.loads(serialized):
                    if call.get("name") in {"anchor_run", "final_result"}:
                        trace["tool_calls"].append({"store": str(path), "step": step, **call})
            for step, text in db.execute(
                "SELECT step, text FROM ledger_observations WHERE target='anchor_run' ORDER BY id"
            ):
                trace["observations"].append({"store": str(path), "step": step, "text": text})
    return trace


def _completed_checks(state: Path, record: dict, trace: dict) -> list[str]:
    failures = []
    results = record.get("results", {})
    required = ("frame", "investigate", "challenge", "feedback", "synthesize", "review", "review-gate", "report")
    missing = [node for node in required if not results.get(node)]
    if missing:
        failures.append(f"Missing committed nodes: {', '.join(missing)}")
        return failures

    def artifact(node: str, name: str) -> str:
        commit = results[node][-1]["commit"]["id"]
        return (state / "artifacts" / commit / "files" / name).read_text()

    try:
        paper = artifact("report", "paper.md")
        if paper != artifact("synthesize", "answer.md"):
            failures.append("The delivered paper differs from the last synthesized answer")
        for heading in ("Abstract", "Introduction", "Survey Methodology", "Comparative Analysis", "Open Problems",
                        "Threats to Validity", "Conclusion", "References"):
            if f"## {heading}" not in paper.splitlines():
                failures.append(f"Missing paper section: {heading}")
        if not any(line.startswith("# ") for line in paper.splitlines()):
            failures.append("Missing paper title")
        if "DECISION: pass" not in artifact("review", "review.md").splitlines():
            failures.append("The last independent review did not pass")
        if results["review-gate"][-1]["completion"].get("route") != "report":
            failures.append("The last review gate did not select report")
        for name in ("research.md", "sources.md"):
            if not artifact("investigate", name).strip():
                failures.append(f"Empty research artifact: {name}")
    except (OSError, KeyError, TypeError) as error:
        failures.append(f"Unreadable committed business artifact: {error}")
    failures.extend(_tool_checks(trace))
    return failures


def _tool_checks(trace: dict) -> list[str]:
    failures = []
    if not trace["provider_calls"]:
        failures.append("Missing persisted provider calls")
    commands = [" ".join(map(str, call.get("arguments", {}).get("command", []))) for call in trace["tool_calls"]]
    if not any("/plugins/academic-research/skills/academic-research/SKILL.md" in command for command in commands):
        failures.append("No trace of reading the original academic Skill")
    if not any("/tools/scholarly/run" in command and "search" in command for command in commands):
        failures.append("No trace of a scholarly search")
    if not any("/tools/scholarly/run" in command and "read" in command for command in commands):
        failures.append("No trace of reading primary literature")
    return failures


def collect(proof: Path, evidence: dict) -> dict:
    state = proof / "state"
    record = _records(state)
    trace = _provider_trace(state)
    evidence.update({
        "run_status": record.get("status"),
        "results": record.get("results", {}),
        "node_passes": {node: len(results) for node, results in record.get("results", {}).items()},
        "provider_trace": trace,
        "provider_requests": len(trace["provider_calls"]),
        "provider_model_observed": sorted({call["model"] for call in trace["provider_calls"] if call["model"]}),
        "tool_call_counts": dict(Counter(call["name"] for call in trace["tool_calls"])),
        "recordings": [str(path.relative_to(proof)) for path in sorted(state.glob("io-harness/store/*.recordings/*/recording.json"))],
        "artifacts": [str(path.relative_to(proof)) for path in sorted(state.glob("artifacts/*/files/*"))],
        "uncommitted_files": [str(path.relative_to(proof)) for path in sorted((proof / "workspace").rglob("*")) if path.is_file()],
        "recovery": "not attempted: interrupt facts and Harness records retained",
        "mcp": "not applicable: original academic Plugin declares no MCP servers; scholarly is an operator tool",
        "research_quality": "not accepted by this mechanical collector",
    })
    if evidence.get("status") == "completed":
        failures = _completed_checks(state, record, trace)
        evidence["completion_check_failures"] = failures
        evidence["runtime_closure"] = "passed" if not failures else "failed"
    else:
        evidence["runtime_closure"] = "not completed"
    (proof / "evidence.json").write_text(json.dumps(evidence, indent=2, ensure_ascii=False))
    return evidence


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "rust/target/release/anchor-runner-host")
    parser.add_argument("--timeout", type=int, default=900)
    parser.add_argument("--collect", type=Path, help="collect an existing proof without sending a model request")
    parser.add_argument("--status", default="unknown", help="recorded command status for --collect")
    args = parser.parse_args()
    if args.collect:
        proof = args.collect.resolve()
        old = proof / "evidence.json"
        evidence = json.loads(old.read_text()) if old.exists() else {"proof": str(proof)}
        evidence["status"] = args.status
        evidence = collect(proof, evidence)
        print(json.dumps({"evidence": str(proof / "evidence.json"), "status": evidence["status"],
                          "runtime_closure": evidence["runtime_closure"], "provider_requests": evidence["provider_requests"]}))
        return

    load_dotenv(ROOT / ".env")
    required = ("ANCHOR_MODEL_API_KEY", "ANCHOR_MODEL_URL", "ANCHOR_MODEL_NAME")
    missing = [name for name in required if not os.environ.get(name)]
    if missing:
        raise SystemExit(f"Missing configured provider variables: {', '.join(missing)}")

    (ROOT / ".local").mkdir(exist_ok=True)
    proof = Path(tempfile.mkdtemp(prefix="rust-deep-research-", dir=ROOT / ".local"))
    print(f"Evidence directory: {proof}", flush=True)
    bundle = proof / "bundle"
    plugin = bundle / "plugins/academic-research"
    bundle.mkdir()
    shutil.copytree(SOURCE_PLUGIN, plugin)
    shutil.copyfile(SOURCE_GRAPH, bundle / "graph.json")
    digest, resources = _digest_plugin(plugin)
    (bundle / "manifest.json").write_text(json.dumps({
        "format": 1,
        "graph": "graph.json",
        "plugins": [{"id": "academic-research", "digest": digest, "resources": resources, "mcp_servers": []}],
    }))

    # Keep an explicit, empty local-input grant root: the original graph does
    # not require operator inputs, but the root itself is part of the boundary.
    local_inputs = proof / "local-inputs/deep-academic-research"
    local_inputs.mkdir(parents=True)
    (local_inputs / "local-inputs.json").write_text("{}")
    state = proof / "state"
    env = {key: value for key, value in os.environ.items() if key.startswith("ANCHOR_MODEL_")}
    env.update({
        "PATH": "/usr/bin:/bin",
        "ANCHOR_RUNNER_BUNDLE_ROOT": str(bundle),
        "ANCHOR_RUNNER_GRAPH_NAME": "deep-academic-research",
        "ANCHOR_RUNNER_LOCAL_INPUTS_ROOT": str(local_inputs.parent),
        "ANCHOR_RUNNER_STATE_ROOT": str(state),
        "ANCHOR_RUNNER_WORKSPACE_ROOT": str(proof / "workspace"),
        "ANCHOR_RUNNER_LIBRARY_ROOT": str((ROOT / ".local/demo/library").resolve()),
        "ANCHOR_RUNNER_ALLOWED_COMMANDS": "sh,cat,cp,find,grep,printf,sed,git,head,tail,mkdir,pwd",
    })
    request = json.dumps({
        "op": "start_bundle",
        "version": 1,
        "request_id": "deep-academic-research-real",
        "run_id": "deep-academic-research",
        "input": {"objective": json.loads(SOURCE_GRAPH.read_text())["objective"]},
    }).encode()
    evidence: dict[str, object] = {
        "status": "started",
        "provider": "configured real provider",
        "provider_model_configured": env.get("ANCHOR_MODEL_NAME"),
        "provider_wire": env.get("ANCHOR_MODEL_WIRE_API", "responses"),
        "graph_sha256": hashlib.sha256(SOURCE_GRAPH.read_bytes()).hexdigest(),
        "graph_unchanged": (bundle / "graph.json").read_bytes() == SOURCE_GRAPH.read_bytes(),
        "plugin_unchanged": all((plugin / name).read_bytes() == (SOURCE_PLUGIN / name).read_bytes() for name in resources),
        "publishing": "disabled: no channel or Docmost endpoint configured",
        "proof": str(proof),
    }
    try:
        try:
            result = subprocess.run(
                [str(args.binary.resolve())],
                input=struct.pack(">I", len(request)) + request,
                capture_output=True,
                env=env,
                cwd=proof,
                timeout=args.timeout,
                check=False,
            )
            (proof / "host.stdout").write_bytes(result.stdout)
            (proof / "host.stderr").write_bytes(result.stderr)
            evidence["host_exit_code"] = result.returncode
            if len(result.stdout) >= 4:
                size = struct.unpack(">I", result.stdout[:4])[0]
                response = json.loads(result.stdout[4:4 + size])
                (proof / "response.json").write_text(json.dumps(response, indent=2, ensure_ascii=False))
                evidence["response"] = response
                evidence["status"] = response.get("status", "unknown")
            else:
                evidence["status"] = "no_framed_response"
        except subprocess.TimeoutExpired as error:
            evidence.update({"status": "timeout", "timeout_seconds": args.timeout, "error": str(error)})
            (proof / "host.stdout").write_bytes(error.stdout or b"")
            (proof / "host.stderr").write_bytes(error.stderr or b"")
    finally:
        evidence = collect(proof, evidence)
        print(json.dumps({key: value for key, value in evidence.items()
                          if key not in {"provider_trace", "results", "recordings", "uncommitted_files"}},
                         indent=2, ensure_ascii=False))
    if evidence["runtime_closure"] != "passed":
        raise SystemExit(1)


if __name__ == "__main__":
    main()
