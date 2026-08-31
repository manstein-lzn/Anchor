from __future__ import annotations

import argparse
import json
import sqlite3
import sys
from typing import Any

from anchor_core.durable import ConflictError, DurableError, DurableStore


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Anchor State core")
    parser.add_argument("--workspace", help=argparse.SUPPRESS)
    parser.add_argument("--state-path", required=True, help="Explicit session-scoped SQLite State path")
    commands = parser.add_subparsers(dest="command", required=True)

    begin = commands.add_parser("begin")
    begin.add_argument("--session-id", required=True)
    begin.add_argument("--proposal-hash", required=True)
    begin.add_argument("--title", required=True)
    begin.add_argument("--contract-json", required=True)
    begin.add_argument("--checkpoint-json", required=True)

    recover = commands.add_parser("recover")
    recover.add_argument("--session-id", required=True)
    recover.add_argument("--task-id")

    update = commands.add_parser("update")
    update.add_argument("--session-id", required=True)
    update.add_argument("--task-id", required=True)
    update.add_argument("--expected-version", required=True, type=int)
    update.add_argument("--candidate-json", required=True)

    recall = commands.add_parser("recall")
    recall.add_argument("--session-id", required=True)
    recall.add_argument("--task-id", required=True)
    recall.add_argument("--locator", required=True)
    recall.add_argument("--content-hash")

    args = parser.parse_args(argv)
    store: DurableStore | None = None
    try:
        store = DurableStore(args.state_path, create=args.command == "begin")
        result = _dispatch(store, args)
        print(json.dumps(result, ensure_ascii=False, sort_keys=True, separators=(",", ":")))
        return 0
    except (ConflictError, DurableError, ValueError, OSError, sqlite3.Error, json.JSONDecodeError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    finally:
        if store is not None:
            store.close()


def _dispatch(store: DurableStore, args: argparse.Namespace) -> dict[str, Any]:
    if args.command == "begin":
        return store.begin(
            session_id=args.session_id,
            proposal_hash=args.proposal_hash,
            title=args.title,
            contract=json.loads(args.contract_json),
            candidate=json.loads(args.checkpoint_json),
        )
    if args.command == "recover":
        return store.recover(session_id=args.session_id, task_id=args.task_id)
    if args.command == "update":
        return store.update(
            session_id=args.session_id,
            task_id=args.task_id,
            expected_version=args.expected_version,
            candidate=json.loads(args.candidate_json),
        )
    if args.command == "recall":
        return store.recall(
            session_id=args.session_id,
            task_id=args.task_id,
            locator=args.locator,
            content_hash=args.content_hash,
        )
    raise ValueError("unsupported command")
