"""anchor-done — how a node with one way out finishes.

    anchor-done --summary "what I did and what I could not do"

The alternative is what the agent loop expects by default: `echo` a forty-one character sentinel and
get every character exactly right, on the first line, on its own. A model that appends its answer, or
abbreviates the marker, produces output the loop does not recognise — and then it does the whole
thing again, which is not a mistake it can see or correct. Naming the act instead of the string was
the fix for routing, and it is the same fix here.

Prints the sentinel the loop recognises, with the summary on the lines after it.
"""

from __future__ import annotations

import argparse
import sys

SENTINEL = "COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT"


def main() -> int:
    parser = argparse.ArgumentParser(prog="anchor-done", description=__doc__)
    parser.add_argument("--summary", default="",
                        help="one or two lines: what you did, and what you could not do")
    args = parser.parse_args()
    print(SENTINEL)
    if args.summary:
        print(args.summary)
    return 0


if __name__ == "__main__":
    sys.exit(main())
