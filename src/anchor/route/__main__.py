"""anchor-route — how a node picks which edge the graph follows next.

    anchor-route --to write --reason "three citations have no support"
    anchor-route --to report

A node with more than one way out cannot finish any other way. The runner tells the sandbox which
targets exist (`ANCHOR_ROUTES`), this validates against them, and printing the sentinel below is
what tells the agent loop the node is done. Submitting and routing are the same act, so a node
cannot leave without having chosen — the failure mode a separate "submit" command would have.

Exits non-zero with the allowed targets on stderr when the target is not one of them, so an agent
that guessed can correct itself in the next turn.
"""

from __future__ import annotations

import argparse
import os
import sys

SENTINEL = "ANCHOR_ROUTE"


def main() -> int:
    parser = argparse.ArgumentParser(prog="anchor-route", description=__doc__)
    parser.add_argument("--to", required=True, help="the node this run goes to next")
    parser.add_argument("--reason", default="", help="one line saying why; recorded, not parsed")
    args = parser.parse_args()

    allowed = [item for item in os.environ.get("ANCHOR_ROUTES", "").split(",") if item]
    if not allowed:
        print("anchor-route: this node has one way out and does not choose; use the normal "
              "completion command", file=sys.stderr)
        return 1
    if args.to not in allowed:
        print(f"anchor-route: {args.to!r} is not a way out of this node. "
              f"Choose one of: {', '.join(allowed)}", file=sys.stderr)
        return 1

    # The sentinel is an output prefix the agent loop recognises; the reason rides along on the
    # lines after it, exactly as the ordinary completion message does.
    print(f"{SENTINEL}: {args.to}")
    if args.reason:
        print(args.reason)
    return 0


if __name__ == "__main__":
    sys.exit(main())
