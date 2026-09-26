"""anchor-graph — run a graph, and keep the run.

    anchor-graph <workspace>
    anchor-graph <workspace> --objective "…"

A workspace holds `graph.json` and a `runs/` directory. Each run makes a new directory there, named
for the moment it started, and leaves it: the history is a record, not something a later run reads.

`ANCHOR_MODEL_SCRIPT` names a JSON file mapping a node id to the commands it should be given, one per
turn. It replaces the model and nothing else — the loop, the sandbox, the mounts and the commits are
the real ones — so a run can be exercised end to end without a provider. Off unless set.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path

from anchor.simple import run as runner

ROOT = Path(__file__).resolve().parents[3]


def main() -> None:
    parser = argparse.ArgumentParser(prog="anchor-graph", description=__doc__)
    parser.add_argument("workspace", help="the directory holding graph.json")
    parser.add_argument("--objective", default=None,
                        help="the task; defaults to the graph's own objective")
    parser.add_argument("--config", default=str(ROOT / ".local" / "runtime.json"),
                        help="runtime config, for model profiles and the secret file")
    parser.add_argument("--resume", default=None, metavar="RUN_DIR",
                        help="pick up a run that a previous process left unfinished")
    parser.add_argument("--library", type=Path, help="shared library directory; inferred for service workspaces")
    args = parser.parse_args()

    written = os.environ.get("ANCHOR_MODEL_SCRIPT")
    model_script = json.loads(Path(written).read_text(encoding="utf-8")) if written else None

    state = runner.run(args.workspace, objective=args.objective, config_path=args.config,
                       resume=args.resume, model_script=model_script, library_root=args.library)
    print(json.dumps({"status": state.status, "executed": state.executed,
                      "skipped": state.skipped}, ensure_ascii=False))


if __name__ == "__main__":
    main()
