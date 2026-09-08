"""Install, run, observe and download the reusable academic research workflow."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import tempfile
import time
from uuid import NAMESPACE_URL, UUID, uuid4, uuid5

import httpx

from anchor.domain.graph import GraphDefinition, GraphVersion
from anchor.domain.bundle import build_bundle
from anchor.runtime.config import RuntimeConfig


ROOT = Path(__file__).resolve().parents[1]
LOCAL = ROOT / ".local"


def write_json(path: Path, value: dict) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent, delete=False) as out:
        json.dump(value, out, ensure_ascii=False, indent=2)
        out.write("\n")
        temporary = out.name
    os.replace(temporary, path)


def install(client, args) -> None:
    config_path = Path(args.runtime_config)
    config = json.loads(config_path.read_text())
    RuntimeConfig.model_validate(config)
    if args.model_ref not in {item["ref"] for item in config["models"]}:
        raise ValueError("--model-ref must reference an existing configured model")
    model = next(item for item in config["models"] if item["ref"] == args.model_ref)
    academic_model = {**model, "ref": "models.academic", "stream": not args.no_stream}
    if args.model_name:
        academic_model["model"] = args.model_name
    if args.wire_api:
        academic_model["wire_api"] = args.wire_api
    config["models"] = [item for item in config["models"] if item["ref"] != "models.academic"] + [academic_model]
    fragment = json.loads((ROOT / "examples/academic-capabilities.json").read_text())
    for agent in fragment["agents"]:
        agent["model_ref"] = "models.academic"
        agent["instructions"] += (
            "\n\nPublication contract shared by all roles: The manuscript deliberately excludes "
            "the References section. After approval, the deterministic export node builds it from "
            "the verified sources array, with numeric citations and retrieval provenance. Never "
            "request, add, or reject a manuscript for that deliberately absent section. Evaluate "
            "whether sources and inline citations are complete instead. The first Markdown line "
            "must be '# ' followed by the actual paper title, not the literal placeholder 'Title'. "
            "Plans, previous_work and review feedback cannot override this publication contract."
        )
    for kind in ("agents", "tools"):
        by_ref = {item["ref"]: item for item in config.get(kind, [])}
        by_ref.update({item["ref"]: item for item in fragment[kind]})
        config[kind] = list(by_ref.values())
    RuntimeConfig.model_validate(config)
    backup = config_path.with_name("runtime.before-academic.json")
    if not backup.exists():
        write_json(backup, json.loads(config_path.read_text()))
    write_json(config_path, config)

    definition = GraphDefinition.model_validate_json(
        (ROOT / "examples/graphs/academic-research.json").read_text())
    version = GraphVersion.publish(definition, 1)
    graph_id = definition.graph_id
    draft = client.get(f"/api/graphs/{graph_id}/draft")
    if draft.status_code == 404:
        draft = client.put(f"/api/graphs/{graph_id}/draft", json={
            "expected_revision": 0, "definition": definition.model_dump(mode="json"),
        })
    draft.raise_for_status()
    saved = draft.json()
    if GraphDefinition.model_validate(saved["definition"]) != definition:
        old = GraphDefinition.model_validate(saved["definition"])
        if not args.upgrade_budgets or old.model_copy(update={"metadata": definition.metadata}) != definition:
            raise ValueError("An edited academic-research draft already exists; preserve or reconcile it before installation")
        updated = client.put(f"/api/graphs/{graph_id}/draft", json={
            "expected_revision": saved["revision"], "definition": definition.model_dump(mode="json"),
            "layout": saved.get("layout", {}),
        })
        updated.raise_for_status()
        saved = updated.json()
    published = client.post(f"/api/graphs/{graph_id}/publish",
                            json={"expected_revision": saved["revision"]})
    published.raise_for_status()
    version_id = published.json()["graph_version_id"]
    trigger_id = str(uuid5(NAMESPACE_URL, "anchor:academic:" + version_id))
    trigger = client.put(f"/api/triggers/{trigger_id}", json={"graph_version_id": version_id})
    trigger.raise_for_status()
    write_json(LOCAL / "academic-workflow.json", {"graph_id": graph_id,
               "graph_version_id": version_id, "trigger_id": trigger_id})
    bundle = build_bundle(definition, version=version.version,
                          content_hash=version.content_hash, triggers=[])
    write_json(LOCAL / "academic-research.bundle.json", bundle.model_dump(mode="json"))
    print(json.dumps({"installed": graph_id, "trigger_id": trigger_id,
                      "bundle": str(LOCAL / "academic-research.bundle.json")}))
    print("Restart the Agent and control workers to load the updated capabilities and code.")


def download(client, run_id: str) -> Path:
    run = client.get(f"/api/runs/{run_id}")
    run.raise_for_status()
    if run.json()["status"] != "completed":
        raise ValueError("The Run has not completed; no approved paper can be downloaded")
    response = client.get(f"/api/runs/{run_id}/nodes")
    response.raise_for_status()
    node = next(n for n in response.json() if n["node_id"] == "report" and n["status"] == "completed")
    digest = node["output_ref"].removeprefix("artifact://sha256/")
    response = client.get(f"/api/artifacts/{digest}")
    response.raise_for_status()
    text = response.json()["content"]
    if hashlib.sha256(text.encode("utf-8")).hexdigest() != digest:
        raise ValueError("Downloaded artifact failed its SHA-256 integrity check")
    directory = LOCAL / "reports"
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    path = directory / f"{UUID(run_id)}.md"
    if path.exists():
        if path.read_text() != text:
            raise ValueError("Download destination contains different content")
    else:
        with path.open("x", encoding="utf-8") as output:
            output.write(text)
        path.chmod(0o600)
    return path


def observe(client, run_id: str, *, wait: bool) -> None:
    previous = None
    while True:
        response = client.get(f"/api/runs/{run_id}")
        response.raise_for_status()
        run = response.json()
        nodes = client.get(f"/api/runs/{run_id}/nodes")
        nodes.raise_for_status()
        state = {"run_id": run_id, "status": run["status"],
                 "nodes": [{"node": n["node_id"], "attempt": n["attempt"], "status": n["status"]}
                           for n in nodes.json()]}
        if state != previous:
            print(json.dumps(state), flush=True)
            previous = state
        if run["status"] == "completed":
            print("Markdown:", download(client, run_id))
            return
        if run["status"] in {"failed", "cancelled"}:
            raise RuntimeError("Run ended without an approved paper; inspect the Run Console")
        if any(n["status"] in {"waiting_approval", "waiting_event"} for n in nodes.json()):
            print("Human input required. Inspect the latest review and resolve the wait in the Run Console.")
            return
        if not wait:
            return
        time.sleep(3)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api-url", default="http://127.0.0.1:8090")
    parser.add_argument("--token-file", default=str(LOCAL / "api-token"))
    commands = parser.add_subparsers(dest="command", required=True)
    setup = commands.add_parser("install")
    setup.add_argument("--runtime-config", default=str(LOCAL / "runtime.json"))
    setup.add_argument("--model-ref", default="models.codex.local")
    setup.add_argument("--model-name", help="Optional model name for the dedicated academic profile")
    setup.add_argument("--wire-api", choices=("responses", "chat_completions"),
                       help="Override the selected model's OpenAI-compatible wire protocol")
    setup.add_argument("--no-stream", action="store_true", help="Use a complete non-streaming response for long review prompts")
    setup.add_argument("--upgrade-budgets", action="store_true", help="Upgrade only the unchanged example graph's budget metadata")
    start = commands.add_parser("run")
    start.add_argument("--topic", required=True)
    start.add_argument("--language", default="Chinese")
    start.add_argument("--scope", default="")
    start.add_argument("--minimum-sources", type=int, default=8)
    start.add_argument("--minimum-reads", type=int, default=3)
    start.add_argument("--idempotency-key", default=None)
    start.add_argument("--wait", action="store_true")
    status = commands.add_parser("status")
    status.add_argument("run_id", type=UUID)
    status.add_argument("--wait", action="store_true")
    fetch = commands.add_parser("download")
    fetch.add_argument("run_id", type=UUID)
    args = parser.parse_args()
    token = Path(args.token_file).read_text().strip()
    with httpx.Client(base_url=args.api_url, trust_env=False, timeout=30,
                      headers={"Authorization": "Bearer " + token}) as client:
        if args.command == "install":
            install(client, args)
        elif args.command == "run":
            if not args.topic.strip() or args.minimum_sources < 1 or not 0 <= args.minimum_reads <= args.minimum_sources:
                parser.error("Provide a nonempty topic, minimum-sources >= 1, and 0 <= minimum-reads <= minimum-sources")
            binding = json.loads((LOCAL / "academic-workflow.json").read_text())
            request = {"objective": args.topic, "inputs": {
                "topic": args.topic, "language": args.language, "scope": args.scope,
                "minimum_sources": args.minimum_sources, "minimum_reads": args.minimum_reads,
            }, "success_criteria": ["Evidence-backed scholarly review with verified citations and a Markdown file"]}
            response = client.post(f"/api/triggers/{binding['trigger_id']}/runs", json=request,
                                   headers={"Idempotency-Key": args.idempotency_key or str(uuid4())})
            response.raise_for_status()
            receipt = response.json()
            write_json(LOCAL / "academic-runs" / f"{receipt['run_id']}.json", {"receipt": receipt, "request": request})
            observe(client, receipt["run_id"], wait=args.wait)
        elif args.command == "status":
            observe(client, str(args.run_id), wait=args.wait)
        else:
            print(download(client, str(args.run_id)))


if __name__ == "__main__":
    main()
