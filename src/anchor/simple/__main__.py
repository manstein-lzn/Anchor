"""python -m anchor.simple graph.json --objective "..." [--work DIR] [--config FILE]"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from anchor.simple import run as runner

ROOT = Path(__file__).resolve().parents[3]


def main() -> None:
    parser = argparse.ArgumentParser(prog="anchor.simple", description="Run an agent graph.")
    parser.add_argument("graph", help="the graph JSON file")
    parser.add_argument("--objective", default=None,
                        help="the task; defaults to the graph's own objective")
    parser.add_argument("--work", default=None, help="where the run's directories go")
    parser.add_argument("--config", default=str(ROOT / ".local" / "runtime.json"),
                        help="runtime config, for model profiles and the secret file")
    args = parser.parse_args()

    work = Path(args.work) if args.work else runner.new_work_dir(ROOT / ".local" / "runs")
    results = runner.run(args.graph, args.objective, work=work, config_path=args.config)
    print(json.dumps({"work": str(work), "nodes": [item.node_id for item in results]}))


if __name__ == "__main__":
    main()
