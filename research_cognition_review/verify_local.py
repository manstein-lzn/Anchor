"""Read-only evidence audit; --live additionally runs two paid harness calls."""

from __future__ import annotations

import argparse
from collections import Counter
from copy import deepcopy
from datetime import datetime
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
OUT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / "src"))

from anchor.domain.context import canonical_json
from anchor.runtime.academic import citation_numbers, craft_errors, structure_errors
from anchor.runtime.node_prompt import PromptParts, assemble_prompt

RUN = "f4eb8ac7-3418-4e8d-8be5-8eaede46e02a"


def digest(value):
    return hashlib.sha256(canonical_json(value).encode()).hexdigest()


def save(name, value):
    (OUT / name).write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--live", action="store_true")
    args = parser.parse_args()
    c = sqlite3.connect(f"file:{ROOT}/.local/api.sqlite?mode=ro", uri=True)
    c.row_factory = sqlite3.Row
    rows = c.execute("select * from node_runs where run_id=? order by id", (RUN,)).fetchall()
    before = digest([dict(row) for row in rows])
    run = c.execute("select * from runs where id=?", (RUN,)).fetchone()
    task = c.execute("select * from tasks where id=?", (run["task_id"],)).fetchone()
    graph = json.loads(c.execute("select definition from graph_versions where graph_version_id=?",
                                (run["graph_version_id"],)).fetchone()[0])
    writer = next(node for node in graph["nodes"] if node["id"] == "write")
    snapshots = {}
    for row in rows:
        if row["node_id"] == "write":
            snapshots[row["attempt"]] = json.loads(c.execute(
                "select snapshot from context_snapshots where node_run_id=?", (row["id"],)
            ).fetchone()[0])
    original = snapshots[1]
    render = lambda s: assemble_prompt(PromptParts(
        objective=task["objective"], node_name=writer["name"], snapshot=s))
    prompt = render(original)
    duplicate_fields = [key for key in original if key != "feedback"
                        and original["feedback"].get(key) == original[key]]
    dedup = deepcopy(original)
    for key in duplicate_fields:
        del dedup["feedback"][key]
    slim = deepcopy(dedup)
    del slim["feedback"]["verified_sources"]
    sources = original["evidence"]["sources"]
    verified = original["feedback"]["verified_sources"]
    shared_keys = set(sources[0]) & set(verified[0])
    source_by_id = {source["id"]: source for source in sources}
    shared_values_equal = all(
        all(source_by_id[v["id"]][key] == v[key] for key in shared_keys)
        for v in verified)
    usage = [json.loads(row[0]) for row in c.execute(
        "select payload from events where stream_id=? and event_type='model.usage'", (RUN,))]
    reviews = []
    for row in rows:
        if row["node_id"] != "review" or not row["output_ref"]:
            continue
        artifact = ROOT / ".local/artifacts" / row["output_ref"].rsplit("/", 1)[1]
        text = artifact.read_text()
        assert hashlib.sha256(text.encode()).hexdigest() == artifact.name
        reviews.append({"attempt": row["attempt"], "output": json.loads(text)})
    evidence_text = canonical_json(original["evidence"])
    audit = {
        "run_id": RUN, "node_run_count": len(rows), "node_runs_digest": before,
        "source_snapshot_digest": digest(original),
        "prompt_chars": {str(k): len(render(s)) for k, s in snapshots.items()},
        "prompt_sha256": hashlib.sha256(prompt.encode()).hexdigest(),
        "prompt_utf8_bytes": len(prompt.encode()),
        "evidence_chars": len(evidence_text), "evidence_utf8_bytes": len(evidence_text.encode()),
        "evidence_exact_occurrences_in_prompt": prompt.count(evidence_text),
        "evidence_fraction": len(evidence_text) / len(prompt),
        "duplicate_fields": duplicate_fields,
        "duplicate_payload_chars": sum(len(canonical_json(original[k])) for k in duplicate_fields),
        "dedup_prompt_chars": len(render(dedup)),
        "dedup_saved_chars": len(prompt) - len(render(dedup)),
        "dedup_saved_fraction": 1 - len(render(dedup)) / len(prompt),
        "slim_prompt_chars": len(render(slim)),
        "slim_saved_fraction": 1 - len(render(slim)) / len(prompt),
        "slim_warning": "Also removes unique verified metadata; NOT a lossless deduplication",
        "source_count": len(sources), "verified_source_count": len(verified),
        "source_ids_equal": {s["id"] for s in sources} == {s["id"] for s in verified},
        "source_shared_fields": sorted(shared_keys),
        "source_shared_values_equal": shared_values_equal,
        "verified_unique_fields": sorted(set(verified[0]) - set(sources[0])),
        "evidence_sources_chars": len(canonical_json(sources)),
        "verified_sources_chars": len(canonical_json(verified)),
        "whitepaper_60_numerator": len(evidence_text) + len(canonical_json(verified)),
        "write_usage": [u for u in usage if u.get("node_id") == "write"],
        "model_call_recordings": c.execute(
            "select count(*) from events where stream_id=? and event_type='model.call'", (RUN,)
        ).fetchone()[0],
        "run_elapsed_seconds": (datetime.fromisoformat(run["updated_at"]) -
                                datetime.fromisoformat(run["created_at"])).total_seconds(),
        "task_fields": {k: task[k] for k in ("objective", "constraints", "success_criteria")},
        "frozen_feedback_edges": [e for e in graph["edges"]
                                  if e["source"] == "check" and e["target"] == "write"],
        "reviews": reviews,
    }
    review_anomalies = []
    for row in c.execute("select run_id,attempt,output_ref from node_runs "
                         "where node_id='review' and output_ref is not null"):
        path = ROOT / ".local/artifacts" / row["output_ref"].rsplit("/", 1)[1]
        text = path.read_text()
        assert hashlib.sha256(text.encode()).hexdigest() == path.name
        review = json.loads(text)
        counts = Counter(i.get("severity") for i in review.get("issues", [])
                         if isinstance(i, dict))
        if review.get("verdict") == "revise" and not counts["major"]:
            review_anomalies.append({**dict(row), "issue_counts": dict(counts)})
    audit["all_run_review_anomalies"] = review_anomalies
    save("local_audit.json", audit)
    save("snapshot_dedup.json", dedup)
    print(json.dumps({k: v for k, v in audit.items() if k not in
                      ("reviews", "frozen_feedback_edges", "task_fields")}, indent=2), flush=True)
    if args.live:
        env = dict(os.environ, ANCHOR_DATABASE_URL=f"sqlite:///{ROOT}/.local/api.sqlite")
        results = {}
        for label, extra in (("baseline", []),
                             ("dedup", ["--snapshot", str(OUT / "snapshot_dedup.json")])):
            command = [sys.executable, "scripts/node_harness.py", "run", RUN, "write",
                       "--attempt", "1", *extra]
            print(f"Starting live {label}", flush=True)
            try:
                process = subprocess.run(command, cwd=ROOT, env=env, capture_output=True,
                                         text=True, timeout=300)
                result = json.loads(process.stdout)
                result["exit_code"] = process.returncode
                result["stderr"] = process.stderr
            except (subprocess.TimeoutExpired, json.JSONDecodeError) as exc:
                result = {"error": type(exc).__name__}
            if "text" in result:
                try:
                    body = json.loads(result["text"])
                    manuscript = body["manuscript"]
                    expected = {s["citation"] for s in sources}
                    cited = citation_numbers(manuscript)
                    result["partial_checks"] = {
                        "scope": "Structure, craft, citation IDs ONLY; not quality or full verification",
                        "structure_errors": structure_errors(manuscript),
                        "craft_errors": craft_errors(manuscript),
                        "missing_citations": sorted(expected - cited),
                        "unknown_citations": sorted(cited - expected),
                    }
                except (ValueError, KeyError, TypeError) as exc:
                    result["partial_checks"] = {"parse_error": str(exc)}
            save(f"live_{label}.json", result)
            results[label] = {k: v for k, v in result.items() if k != "text"}
            print(json.dumps(results[label], ensure_ascii=False), flush=True)
        after_rows = c.execute("select * from node_runs where run_id=? order by id", (RUN,)).fetchall()
        results["source_node_runs_unchanged"] = before == digest([dict(row) for row in after_rows])
        results["source_snapshot_unchanged"] = digest(original) == digest(json.loads(c.execute(
            "select s.snapshot from context_snapshots s join node_runs n on s.node_run_id=n.id "
            "where n.run_id=? and n.node_id='write' and n.attempt=1", (RUN,)).fetchone()[0]))
        save("live_summary.json", results)
        assert results["source_node_runs_unchanged"] and results["source_snapshot_unchanged"]
    c.close()


if __name__ == "__main__":
    main()
