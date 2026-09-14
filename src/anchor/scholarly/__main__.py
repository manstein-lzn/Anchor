"""anchor-scholarly — search and read the literature from a shell.

    anchor-scholarly search --query "learned cost models" [--source crossref|arxiv] [--limit 8]
    anchor-scholarly read --url https://arxiv.org/pdf/2401.00001
    anchor-scholarly read-many --urls u1,u2,u3
    anchor-scholarly citations --identifier 2401.00001 [--direction cited_by|cites]

The answer is JSON on stdout. A failure exits non-zero and says why on stderr, so an agent sees it
and can try something else rather than believing it succeeded.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from anchor.runtime.research_tools import ResearchRequest, execute_research

COMMANDS = {
    "search": "scholarly.search",
    "search-many": "scholarly.search_many",
    "read": "scholarly.read",
    "read-many": "scholarly.read_many",
    "citations": "scholarly.citations",
}


def _add_position(parser: argparse.ArgumentParser) -> None:
    """Where in the document to start.

    A long paper does not fit in one answer: `read` returns twenty-four thousand characters and a
    `next_offset` saying where the next one begins. Passing it back is how the rest of the paper is
    reached, and without these two flags there was no way to pass it back at all — which turned
    "read the full text" into "read the first part, four times".
    """
    parser.add_argument("--offset", type=int, default=0,
                        help="character offset to start from; pass a document's `next_offset` here")
    parser.add_argument("--page-start", type=int, default=0,
                        help="page to start from, for PDFs; pass `next_page_start` here")


def _request(args: argparse.Namespace) -> dict:
    if args.command == "search":
        return {"query": args.query, "source": args.source, "limit": args.limit,
                "offset": args.offset}
    if args.command == "read":
        return {"url": args.url, "offset": args.offset, "page_start": args.page_start}
    if args.command == "search-many":
        lines = Path(args.queries_file).read_text(encoding="utf-8").splitlines()
        return {"queries": [line.strip() for line in lines
                            if line.strip() and not line.strip().startswith("#")],
                "source": args.source, "limit": args.limit, "offset": args.offset}
    if args.command == "read-many":
        return {"urls": [item.strip() for item in args.urls.split(",") if item.strip()],
                "offset": args.offset, "page_start": args.page_start}
    return {"identifier": args.identifier, "direction": args.direction}


def main() -> int:
    parser = argparse.ArgumentParser(prog="anchor-scholarly", description=__doc__)
    subs = parser.add_subparsers(dest="command", required=True)

    search = subs.add_parser("search")
    search.add_argument("--query", required=True)
    search.add_argument("--source", default="crossref", choices=("crossref", "arxiv", "openalex"))
    search.add_argument("--limit", type=int, default=8)
    search.add_argument("--offset", type=int, default=0)

    many_searches = subs.add_parser("search-many", help="several queries in one call")
    many_searches.add_argument("--queries-file", required=True,
                               help="a file with one query per line; blank lines and # are ignored")
    many_searches.add_argument("--source", default="crossref",
                               choices=("crossref", "arxiv", "openalex"))
    many_searches.add_argument("--limit", type=int, default=8)
    many_searches.add_argument("--offset", type=int, default=0)

    read = subs.add_parser("read")
    read.add_argument("--url", required=True)
    _add_position(read)

    many = subs.add_parser("read-many")
    many.add_argument("--urls", required=True, help="comma-separated")
    _add_position(many)

    cites = subs.add_parser("citations")
    cites.add_argument("--identifier", required=True)
    cites.add_argument("--direction", default="cited_by", choices=("cited_by", "cites"))

    args = parser.parse_args()
    try:
        request = ResearchRequest(**{key: value for key, value in _request(args).items()
                                     if value is not None})
        print(execute_research(COMMANDS[args.command], request, timeout_seconds=60.0))
    except Exception as exc:  # noqa: BLE001 - every failure is the caller's to handle
        print(f"anchor-scholarly: {type(exc).__name__}: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
