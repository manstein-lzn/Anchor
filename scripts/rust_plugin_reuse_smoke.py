"""Real-provider acceptance of unchanged Graph/Plugin and an existing Python tool.

Run with .venv/bin/python after cargo build -p anchor-runner-host. The Python
script prepares and verifies evidence; Graph/Agent execution is Rust-native.
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

from anchor.runtime.secrets import load_dotenv


ROOT = Path(__file__).resolve().parents[1]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "rust/target/debug/anchor-runner-host")
    parser.add_argument("--library", type=Path, default=ROOT / ".local/demo/library")
    args = parser.parse_args()
    load_dotenv(ROOT / ".env")
    required = ("ANCHOR_MODEL_API_KEY", "ANCHOR_MODEL_URL", "ANCHOR_MODEL_NAME")
    if any(not os.environ.get(name) for name in required):
        raise SystemExit("Missing ANCHOR_MODEL_API_KEY/URL/NAME; no model request made")
    (ROOT / ".local").mkdir(exist_ok=True)
    proof = Path(tempfile.mkdtemp(prefix="rust-plugin-reuse-", dir=ROOT / ".local"))
    print(f"Evidence directory: {proof}", flush=True)
    bundle = proof / "bundle"
    plugin = bundle / "plugins/academic-research"
    shutil.copytree(ROOT / "plugins/academic-research", plugin)
    shutil.copyfile(ROOT / "examples/graphs/plugin-research.json", bundle / "graph.json")
    resources = sorted(p.relative_to(plugin).as_posix() for p in plugin.rglob("*") if p.is_file())
    digest = hashlib.sha256()
    for name in resources:
        digest.update(name.encode())
        digest.update(hashlib.sha256((plugin / name).read_bytes()).digest())
    (bundle / "manifest.json").write_text(json.dumps({
        "format": 1, "graph": "graph.json", "plugins": [{
            "id": "academic-research", "digest": digest.hexdigest(),
            "resources": resources, "mcp_servers": [],
        }],
    }))
    state = proof / "state"
    env = {name: os.environ[name] for name in required}
    env.update({
        "PATH": "/usr/bin:/bin",
        "ANCHOR_MODEL_WIRE_API": os.environ.get("ANCHOR_MODEL_WIRE_API", "responses"),
        "ANCHOR_RUNNER_BUNDLE_ROOT": str(bundle),
        "ANCHOR_RUNNER_STATE_ROOT": str(state),
        "ANCHOR_RUNNER_WORKSPACE_ROOT": str(proof / "work"),
        "ANCHOR_RUNNER_LIBRARY_ROOT": str(args.library.resolve()),
        "ANCHOR_RUNNER_ALLOWED_COMMANDS": "sh,cat,find,printf",
    })
    request = json.dumps({
        "op": "start_bundle", "version": 1, "request_id": "plugin-reuse-acceptance",
        "run_id": "plugin-reuse", "input": {"task": (
            "本轮仅验收已有学术 Plugin 的真实检索接线，不做完整调研。"
            "先用 anchor_run 读取 /plugins/academic-research/skills/academic-research/SKILL.md。"
            "然后用 anchor_run 执行 sh -c '/tools/scholarly/run search "
            '--query "retrieval augmented generation" --source crossref --limit 1 > /workspace/sources.json' "'"
            "，必须保留工具原始 JSON 输出，不要编造或改写 sources.json。"
            "读取 sources.json，写简短中文 research.md，包含实际标题、DOI 和读取范围，"
            "明确这里只查询了元数据/摘要，没有阅读论文全文。随后完成。"
        )},
    }).encode()
    try:
        result = subprocess.run(
            [str(args.binary.resolve())], input=struct.pack(">I", len(request)) + request,
            capture_output=True, env=env, cwd=proof, timeout=240,
        )
    except subprocess.TimeoutExpired:
        (proof / "evidence.json").write_text(json.dumps({"status": "timeout", "seconds": 240}))
        raise
    (proof / "host.stderr").write_bytes(result.stderr)
    result.check_returncode()
    size = struct.unpack(">I", result.stdout[:4])[0]
    response = json.loads(result.stdout[4:4 + size])
    (proof / "response.json").write_text(json.dumps(response, indent=2))
    assert response.get("status") == "completed", f"Run failed; inspect {proof / 'response.json'}"
    record = json.loads((state / "runs/plugin-reuse.json").read_text())
    commit = record["results"]["research"][0]["commit"]["id"]
    artifacts = state / "artifacts" / commit / "files"
    sources = json.loads((artifacts / "sources.json").read_text())
    assert sources["source"] == "crossref" and sources["papers"]
    doi = sources["papers"][0]["doi"]
    assert doi and doi in (artifacts / "research.md").read_text()
    stores = list((state / "io-harness/store").glob("*.sqlite3"))
    assert len(stores) == 1
    with sqlite3.connect(stores[0]) as store:
        calls = [(step, call) for step, serialized in store.execute("SELECT step, calls FROM step_turns ORDER BY step")
                 for call in json.loads(serialized) if call["name"] == "anchor_run"]
        providers = store.execute("SELECT model FROM provider_calls ORDER BY id").fetchall()
        final_text = store.execute("SELECT text FROM step_turns ORDER BY step DESC LIMIT 1").fetchone()[0]
        observations = [(step, json.loads(text.split("[anchor_run]", 1)[1])) for step, text in store.execute(
            "SELECT step, text FROM ledger_observations WHERE target='anchor_run' ORDER BY id"
        )]
    completion = json.loads(final_text)
    completion_source = completion["_anchor_completion"]
    assert completion_source["status"] == "submitted"
    assert len(completion_source["calls"]) == 1
    assert completion_source["calls"][0]["name"] == "final_result"
    assert completion_source["calls"][0]["arguments"]["summary"] == completion["summary"]
    commands = [call["arguments"]["command"] for _, call in calls]
    search_steps = {step for step, call in calls
                    if "/tools/scholarly/run search " in " ".join(call["arguments"]["command"])}
    assert search_steps and any(step in search_steps and obs.get("exit_code") == 0
                               and obs.get("status") == "completed" for step, obs in observations)
    raw_sources = (artifacts / "sources.json").read_text()
    assert any(raw_sources in obs.get("stdout", "") for _, obs in observations), (
        "Raw tool result was not read back unchanged"
    )
    assert any("/plugins/academic-research/skills/academic-research/SKILL.md" in " ".join(command)
               for command in commands)
    assert providers, "Missing persisted provider evidence"
    assert (bundle / "graph.json").read_bytes() == (ROOT / "examples/graphs/plugin-research.json").read_bytes()
    assert all((plugin / name).read_bytes() == (ROOT / "plugins/academic-research" / name).read_bytes()
               for name in resources)
    evidence = {
        "status": "passed", "run": "plugin-reuse", "graph_unchanged": True,
        "plugin_unchanged": True, "runtime": "Rust io-harness", "tool": "existing Python anchor-scholarly",
        "library": str(args.library.resolve()), "provider": "real configured model",
        "provider_model_observed": sorted({row[0] for row in providers if row[0]}),
        "provider_requests": len(providers), "commands": commands,
        "completion_protocol": "native final_result tool",
        "completion_arguments": completion_source["calls"][0]["arguments"],
        "source": sources["source"], "doi": doi,
        "research_artifact": str((artifacts / "research.md").relative_to(proof)),
        "scope": "Plugin Skill + live Crossref metadata search; not a full academic review",
    }
    (proof / "evidence.json").write_text(json.dumps(evidence, indent=2, ensure_ascii=False))
    print(json.dumps({"evidence": str(proof / "evidence.json"),
                      **{key: value for key, value in evidence.items() if key != "commands"}},
                     indent=2, ensure_ascii=False))


if __name__ == "__main__":
    main()
