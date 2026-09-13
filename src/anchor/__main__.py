"""anchor-serve — run a deployed Anchor that listens for triggers."""

from __future__ import annotations

import argparse
from pathlib import Path

from anchor.serve import serve

ROOT = Path(__file__).resolve().parents[2]


def main() -> None:
    parser = argparse.ArgumentParser(prog="anchor-serve", description=__doc__)
    parser.add_argument("--root", default=str(Path.home() / ".anchor"),
                        help="where the workspaces live")
    parser.add_argument("--config", default=str(ROOT / ".local" / "runtime.json"))
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8077)
    args = parser.parse_args()
    serve(args.root, args.config, args.host, args.port)


if __name__ == "__main__":
    main()
