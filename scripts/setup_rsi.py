"""Install the RSI Graph and its weekly schedule into an Anchor data root."""

from __future__ import annotations

import argparse
from datetime import datetime
import json
from pathlib import Path
import shutil
from uuid import uuid4

from anchor.scheduling import next_after, validate


def install(root: Path, source: Path, *, weekday: int, time: str, analysis_model: str | None = None) -> dict:
    root = root.resolve()
    source = source.resolve()
    workspace = root / "workspaces" / "rsi"
    workspace.mkdir(parents=True, exist_ok=True)
    graph = workspace / "graph.json"
    if not graph.exists():
        if analysis_model:
            definition = json.loads((source / "examples/graphs/rsi.json").read_text())
            for role in ("analyst", "fact-review", "proposal-review"):
                definition["agents"][role]["model"] = analysis_model
            graph.write_text(json.dumps(definition, ensure_ascii=False, indent=2) + "\n")
        else:
            shutil.copy2(source / "examples/graphs/rsi.json", graph)
    local_inputs = workspace / "local-inputs.json"
    local_inputs.write_text(json.dumps({
        "collect": {"anchor": str(root), "source": str(source), "code": str(source),
                    "grants": str(local_inputs)},
        "research": {"code": str(source)},
        "review": {"code": str(source)},
        "gate": {"code": str(source)},
    }, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    state = root / "state"
    state.mkdir(parents=True, exist_ok=True)
    schedules_path = state / "schedules.json"
    try:
        schedules = json.loads(schedules_path.read_text(encoding="utf-8")) if schedules_path.exists() else []
    except ValueError as exc:
        raise ValueError(f"cannot parse {schedules_path}: {exc}") from exc
    rule = validate({"type": "weekly", "time": time, "weekdays": [weekday]}, datetime.now())
    existing = next((item for item in schedules if item.get("enabled") and item.get("graph") == "rsi"
                     and item.get("rule") == rule), None)
    created = False
    if existing is None:
        now = datetime.now()
        schedule = {"id": str(uuid4()), "graph": "rsi", "rule": rule, "input": {},
                    "created_at": now.isoformat(timespec="seconds"),
                    "next_at": next_after(rule, now).isoformat(timespec="seconds"), "enabled": True}
        schedules.append(schedule)
        existing = schedule
        created = True
    temporary = schedules_path.with_suffix(".tmp")
    temporary.write_text(json.dumps(schedules, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    temporary.replace(schedules_path)
    return {"workspace": str(workspace), "graph": str(graph), "local_inputs": str(local_inputs),
            "schedule": existing, "schedule_created": created}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(".local/demo"))
    parser.add_argument("--source", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--weekday", type=int, default=3, help="Monday=0; default Thursday")
    parser.add_argument("--time", default="09:00")
    parser.add_argument("--analysis-model", help="Configured model alias for synthesis and both reviews")
    args = parser.parse_args()
    if args.weekday < 0 or args.weekday > 6:
        parser.error("--weekday must be between 0 and 6")
    print(json.dumps(install(args.root, args.source, weekday=args.weekday, time=args.time,
                             analysis_model=args.analysis_model),
                     ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
