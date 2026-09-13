"""anchor-graph — run a graph, and keep the run.

    anchor-graph <workspace>
    anchor-graph <workspace> --objective "…"

A workspace holds `graph.json` and a `runs/` directory. Each run makes a new directory there, named
for the moment it started, and leaves it: the history is a record, not something a later run reads.
"""

from __future__ import annotations

import argparse
import json
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
    args = parser.parse_args()

    results = runner.run(args.workspace, objective=args.objective, config_path=args.config)
    print(json.dumps({"executed": [item.node_id for item in results]}, ensure_ascii=False))


if __name__ == "__main__":
    main()
