"""Run the Rust-native parallel RSI bundle against a frozen real project snapshot.

This is an operator convenience script, not a Runtime dependency. It starts the
Rust evidence MCP and Rust Graph host, then checks the resulting report/evidence.
It neither edits the project nor registers/replaces any production schedule.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import select
import shutil
import struct
import subprocess
import tempfile
import shlex

from anchor.runtime.secrets import load_dotenv

ROOT = Path(__file__).resolve().parents[1]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=ROOT)
    parser.add_argument("--data", type=Path, default=ROOT / ".local/demo")
    parser.add_argument("--rust-state", type=Path)
    parser.add_argument("--previous", type=Path)
    parser.add_argument("--proof", type=Path)
    parser.add_argument("--revision-report", type=Path, help="Review/correct a prior published report without repeating the five audits")
    parser.add_argument("--feedback-file", type=Path, help="Operator acceptance feedback, supplied as task input")
    args = parser.parse_args()
    load_dotenv(ROOT / ".env")
    model_names = ["ANCHOR_MODEL_API_KEY", "ANCHOR_MODEL_URL", "ANCHOR_MODEL_NAME"]
    if any(not os.environ.get(key) for key in model_names):
        raise SystemExit("Missing configured model credentials; no model request made")
    (ROOT / ".local").mkdir(exist_ok=True)
    proof = args.proof.resolve() if args.proof else Path(tempfile.mkdtemp(prefix="rust-rsi-", dir=ROOT / ".local"))
    proof.mkdir(parents=True, exist_ok=True)
    if (proof / "state").exists():
        raise SystemExit("Use a new proof directory; this launcher does not replay old Runs")
    (proof / "bundle").mkdir()
    for name in ("graph.json", "manifest.json"):
        shutil.copy2(ROOT / "examples/rust-rsi" / name, proof / "bundle" / name)
    shutil.copytree(ROOT / "examples/rust-rsi/plugins", proof / "bundle/plugins")
    if bool(args.revision_report) != bool(args.feedback_file):
        raise SystemExit("--revision-report and --feedback-file must be provided together")
    feedback = None
    if args.revision_report:
        graph_path = proof / "bundle/graph.json"
        graph = json.loads(graph_path.read_text())
        analyst = graph["agents"]["analyst"]
        analyst["instructions"] = (
            "这是上一份RSI报告的独立纠错Run。先用rsi-anchor.rsi_rsi_index(domain=previous)和rsi-anchor.rsi_rsi_read读取上一报告的rsi-report.md、evolution.json、sources.md及review.json。"
            "按任务input.acceptance_feedback逐项核查并修正，不能仅删除异议而不核对原文。当前Run没有五个审查分支，不读取/in/audit-join或/in/*-audit。"
            "使用rsi-anchor.rsi_rsi_read核对源代码/冻结产品目标/采集投影的实际边界；必要时重新定位行号，不能沿用旧报告失效定位。"
            "写/workspace/rsi-report.md、evolution.json、sources.md；格式沿原报告，保留原来五领域覆盖局限，明确这次是纠错复核。"
            "若/in/review/review.json存在则先处理本Run最新反馈。不得自动实施提案。\n"
            + analyst["instructions"].split("证据与目标判据", 1)[-1]
        )
        graph["agents"] = {"analyst": analyst, "reviewer": graph["agents"]["reviewer"]}
        graph["entry"] = "analyze"
        graph["nodes"] = [node for node in graph["nodes"] if node["id"] in {"analyze", "review", "publish"}]
        graph["edges"] = [edge for edge in graph["edges"] if edge["from"] in {"analyze", "review"}]
        graph["ops"] = {"publish": {"run": shlex.join(["sh", "-c", "set -eu; jq -e '.passed == true' /in/review/review.json > /dev/null; jq -e '.proposals | type == \"array\"' /in/analyze/evolution.json > /dev/null; cp /in/analyze/rsi-report.md /in/analyze/evolution.json /in/analyze/sources.md /in/review/review.json /in/review/review.md /workspace/"])}}
        graph["objective"] = "按验收反馈纠正上一份Anchor RSI报告的事实和安全边界，保留可验证建议与未验证项。"
        graph_path.write_text(json.dumps(graph, ensure_ascii=False, indent=2))
        feedback = args.feedback_file.read_text()
        args.previous = args.revision_report
    base = {"PATH": os.environ.get("PATH", "/usr/bin:/bin")}
    plugin_env = {**base,
                  "ANCHOR_RSI_SOURCE_ROOT": str(args.source.resolve()),
                  "ANCHOR_RSI_DATA_ROOT": str(args.data.resolve()),
                  "ANCHOR_RSI_EVIDENCE_ROOT": str(proof / "evidence")}
    if args.rust_state:
        plugin_env["ANCHOR_RSI_RUST_STATE_ROOT"] = str(args.rust_state.resolve())
    if args.previous:
        plugin_env["ANCHOR_RSI_PREVIOUS_ROOT"] = str(args.previous.resolve())
    with (proof / "plugin-stderr.log").open("w") as errors:
        plugin = subprocess.Popen([str(ROOT / "rust/target/debug/anchor-rsi")],
                                  stdout=subprocess.PIPE, stderr=errors, text=True, env=plugin_env)
        host = None
        try:
            if not select.select([plugin.stdout], [], [], 60)[0]:
                raise RuntimeError(f"Evidence server not ready; inspect {proof / 'plugin-stderr.log'}")
            endpoint = plugin.stdout.readline().strip()
            if not endpoint.startswith("http://127.0.0.1:"):
                raise RuntimeError(f"Evidence server failed; inspect {proof / 'plugin-stderr.log'}")
            plugin_path = proof / "bundle/plugins/rsi/plugin.json"
            plugin_manifest = json.loads(plugin_path.read_text())
            plugin_manifest["mcpServers"]["anchor.rsi"] = {
                "type": "http", "url": endpoint,
            }
            plugin_bytes = json.dumps(plugin_manifest, ensure_ascii=False, indent=2).encode()
            plugin_path.write_bytes(plugin_bytes)
            digest = hashlib.sha256(
                b"plugin.json" + hashlib.sha256(plugin_bytes).digest()
            ).hexdigest()
            bundle_manifest = json.loads((proof / "bundle/manifest.json").read_text())
            bundle_manifest["plugins"][0]["digest"] = digest
            (proof / "bundle/manifest.json").write_text(json.dumps(bundle_manifest, indent=2))
            env = {**base, **{key: os.environ[key] for key in model_names},
                   "ANCHOR_MODEL_WIRE_API": os.environ.get("ANCHOR_MODEL_WIRE_API", "responses"),
                   "ANCHOR_RUNNER_STATE_ROOT": str(proof / "state"),
                   "ANCHOR_RUNNER_WORKSPACE_ROOT": str(proof / "work"),
                   "ANCHOR_RUNNER_BUNDLE_ROOT": str(proof / "bundle"),
                   "ANCHOR_RUNNER_ALLOWED_COMMANDS": "sh,cat,jq,cp,printf,test,ls,sed,head,wc,mkdir",
                   }
            request = json.dumps({"op":"start_bundle","version":1,"request_id":"rsi","run_id":"rsi-native","input":({"acceptance_feedback":feedback} if feedback else {})}).encode()
            host = subprocess.Popen([str(ROOT / "rust/target/debug/anchor-runner-host")],
                                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
            print(json.dumps({"proof": str(proof), "status": "running"}), flush=True)
            stdout, stderr = host.communicate(struct.pack(">I", len(request)) + request)
            (proof / "host-stderr.log").write_bytes(stderr)
            if host.returncode or len(stdout) < 4:
                raise RuntimeError(f"Host failed; inspect {proof / 'host-stderr.log'}")
            size = struct.unpack(">I", stdout[:4])[0]
            response = json.loads(stdout[4:4+size])
            (proof / "response.json").write_text(json.dumps(response, ensure_ascii=False, indent=2))
            if response.get("status") != "completed":
                raise RuntimeError(f"RSI did not complete; inspect {proof / 'response.json'}")
            record = json.loads((proof / "state/runs/rsi-native.json").read_text())
            results = record["results"]
            commit = results["publish"][-1]["commit"]["id"]
            output = proof / "state/artifacts" / commit / "files"
            evolution = json.loads((output / "evolution.json").read_text())
            review = json.loads((output / "review.json").read_text())
            assert review["passed"] is True
            assert isinstance(evolution["proposals"], list)
            assert (output / "rsi-report.md").stat().st_size > 0
            evidence = {"status":"graph_completed", "mechanical_checks":"passed", "owner_report_acceptance":"pending", "run":"rsi-native", "provider":"real configured model",
                        "plugin":"Rust RSI read-only evidence service, real source/data and public HTTP metadata",
                        "domains": ["runs", "code", "graphs", "plugins", "dependencies"],
                        "mode": "revision" if feedback else "parallel-audit",
                        "model_requests": sum(r["completion"]["model_requests"] for rs in results.values() for r in rs),
                        "proposals": len(evolution["proposals"]),
                        "report": str((output / "rsi-report.md").relative_to(proof)),
                        "limitations":"No automatic code changes, production scheduling or long-term improvement acceptance"}
            (proof / "acceptance.json").write_text(json.dumps(evidence, ensure_ascii=False, indent=2))
            print(json.dumps({"proof": str(proof), **evidence}, ensure_ascii=False, indent=2))
        finally:
            if host and host.poll() is None:
                host.terminate()
                try:
                    host.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    host.kill()
                    host.wait()
            plugin.terminate()
            try:
                plugin.wait(timeout=5)
            except subprocess.TimeoutExpired:
                plugin.kill()
                plugin.wait()


if __name__ == "__main__":
    main()
