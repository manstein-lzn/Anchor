"""`anchor` command line: the agent-facing surface for the Anchor kernel.

Every command prints JSON by default so a coding agent can consume it directly,
uses deterministic exit codes (0 ok, 1 API error, 2 usage error), and delegates
all logic to :class:`anchor.client.AnchorClient`. The same operation layer backs
the MCP server, so the two surfaces cannot drift.
"""

from __future__ import annotations

import argparse
import json
import sys
from typing import Any

from anchor.client import AnchorApiError, AnchorClient


def _parse_pairs(values: list[str] | None) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for item in values or []:
        if "=" not in item:
            raise SystemExit(f"expected key=value, got {item!r}")
        key, _, raw = item.partition("=")
        try:
            result[key] = json.loads(raw)
        except json.JSONDecodeError:
            result[key] = raw
    return result


def _emit(value: Any, *, stream: Any = None) -> None:
    stream = stream or sys.stdout
    if isinstance(value, str):
        print(value, file=stream)
    else:
        print(json.dumps(value, ensure_ascii=False, indent=2, sort_keys=False), file=stream)


def _load_json(path: str) -> Any:
    if path == "-":
        return json.load(sys.stdin)
    with open(path, encoding="utf-8") as handle:
        return json.load(handle)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="anchor",
                                     description="Drive the Anchor kernel from an agent.")
    parser.add_argument("--api-url", default="http://127.0.0.1:8090")
    parser.add_argument("--token-file", default=None)
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("health", help="readiness of the API and its services")
    sub.add_parser("capabilities", help="available model/agent/tool/verifier refs")
    sub.add_parser("ir", help="Graph IR authoring reference (schema, DSL, template)")
    sub.add_parser("graphs", help="list drafts")
    sub.add_parser("waits", help="list nodes waiting for a human")

    graph = sub.add_parser("graph", help="author and publish a graph")
    graph_sub = graph.add_subparsers(dest="graph_command", required=True)
    show = graph_sub.add_parser("show"); show.add_argument("graph_id")
    validate = graph_sub.add_parser("validate"); validate.add_argument("--file", required=True)
    install = graph_sub.add_parser("install")
    install.add_argument("--file", required=True)
    install.add_argument("--no-publish", action="store_true")
    publish = graph_sub.add_parser("publish")
    publish.add_argument("graph_id"); publish.add_argument("--revision", type=int, required=True)
    bundle = graph_sub.add_parser("bundle")
    bundle.add_argument("version_id"); bundle.add_argument("--out")
    import_ = graph_sub.add_parser("import")
    import_.add_argument("--file", required=True)
    import_.add_argument("--expected-revision", type=int, default=0)
    import_.add_argument("--no-publish", action="store_true")

    trigger = sub.add_parser("trigger", help="register or toggle a trigger")
    trigger_sub = trigger.add_subparsers(dest="trigger_command", required=True)
    add = trigger_sub.add_parser("add")
    add.add_argument("--version", required=True)
    add.add_argument("--type", default="manual")
    add.add_argument("--id")
    add.add_argument("--field", action="append", metavar="KEY=VALUE")
    toggle = trigger_sub.add_parser("toggle")
    toggle.add_argument("trigger_id"); toggle.add_argument("--enabled", choices=("true", "false"))

    run = sub.add_parser("run", help="start, observe and control runs")
    run_sub = run.add_subparsers(dest="run_command", required=True)
    start = run_sub.add_parser("start")
    start.add_argument("--trigger", required=True)
    start.add_argument("--objective", required=True)
    start.add_argument("--input", action="append", metavar="KEY=VALUE")
    start.add_argument("--idempotency-key")
    for name in ("show", "nodes", "decisions", "verifications", "operations",
                 "diagnostics", "progress"):
        item = run_sub.add_parser(name); item.add_argument("run_id")
    events = run_sub.add_parser("events")
    events.add_argument("run_id"); events.add_argument("--after", type=int, default=0)
    events.add_argument("--limit", type=int, default=100)
    watch = run_sub.add_parser("watch")
    watch.add_argument("run_id"); watch.add_argument("--timeout", type=float, default=300.0)
    watch.add_argument("--interval", type=float, default=2.0)
    for name in ("pause", "resume"):
        item = run_sub.add_parser(name)
        item.add_argument("run_id"); item.add_argument("--reason", required=True)
        item.add_argument("--actor", default="agent")
    stop = run_sub.add_parser("stop")
    stop.add_argument("run_id"); stop.add_argument("--reason", required=True)
    for name in ("archive", "unarchive"):
        item = run_sub.add_parser(name); item.add_argument("run_id")
    runs = run_sub.add_parser("list")
    runs.add_argument("--graph"); runs.add_argument("--status", action="append")
    runs.add_argument("--include-archived", action="store_true")

    wait = sub.add_parser("wait", help="human decisions on a parked node")
    wait_sub = wait.add_subparsers(dest="wait_command", required=True)
    for name in ("approve", "reject"):
        item = wait_sub.add_parser(name)
        item.add_argument("node_run_id"); item.add_argument("--reason", required=True)
        item.add_argument("--actor", default="operator")
    resume = wait_sub.add_parser("resume")
    resume.add_argument("node_run_id"); resume.add_argument("--event-type", required=True)
    resume.add_argument("--payload", action="append", metavar="KEY=VALUE")

    operation = sub.add_parser("operation", help="reconcile an unknown side effect")
    operation_sub = operation.add_subparsers(dest="operation_command", required=True)
    reconcile = operation_sub.add_parser("reconcile")
    reconcile.add_argument("operation_id")
    reconcile.add_argument("--status", choices=("succeeded", "failed"), required=True)
    reconcile.add_argument("--reconciliation-ref", required=True)
    reconcile.add_argument("--result-ref")
    reconcile.add_argument("--error-code")
    reconcile.add_argument("--reason", default="operator reconciled external side effect")

    artifact = sub.add_parser("artifact", help="read a content-addressed artifact")
    artifact.add_argument("ref"); artifact.add_argument("--out")

    sub.add_parser("storage", help="footprint report")
    budget = sub.add_parser("budget", help="read or adjust storage budgets")
    budget.add_argument("--global", dest="global_bytes", type=int)
    budget.add_argument("--graph", action="append", metavar="GRAPH_ID=BYTES")

    retention = sub.add_parser("retention", help="rolling retention")
    retention_sub = retention.add_subparsers(dest="retention_command", required=True)
    retention_sub.add_parser("preview")
    retention_sub.add_parser("sweep")
    retention_sub.add_parser("audit")
    return parser


def dispatch(client: AnchorClient, args: argparse.Namespace) -> Any:
    if args.command == "health":
        return client.health()
    if args.command == "capabilities":
        return client.capabilities()
    if args.command == "ir":
        return client.ir()
    if args.command == "graphs":
        return client.list_graphs()
    if args.command == "waits":
        return client.list_waits()

    if args.command == "graph":
        if args.graph_command == "show":
            return client.get_draft(args.graph_id)
        if args.graph_command == "validate":
            definition = _load_json(args.file)
            return {"structural": client.validate(definition),
                    "capabilities": client.validate_capabilities(definition)}
        if args.graph_command == "install":
            return client.install(_load_json(args.file), publish=not args.no_publish)
        if args.graph_command == "publish":
            return client.publish(args.graph_id, expected_revision=args.revision)
        if args.graph_command == "bundle":
            bundle = client.export_bundle(args.version_id)
            if args.out:
                with open(args.out, "w", encoding="utf-8") as handle:
                    json.dump(bundle, handle, ensure_ascii=False, indent=2)
                return {"written": args.out}
            return bundle
        if args.graph_command == "import":
            return client.import_bundle(_load_json(args.file),
                                        expected_revision=args.expected_revision,
                                        publish=not args.no_publish)

    if args.command == "trigger":
        if args.trigger_command == "add":
            return client.register_trigger(args.version, trigger_id=args.id,
                                           type=args.type, **_parse_pairs(args.field))
        if args.trigger_command == "toggle":
            return client.set_trigger_enabled(args.trigger_id, args.enabled == "true")

    if args.command == "run":
        if args.run_command == "start":
            return client.start_run(args.trigger, objective=args.objective,
                                    inputs=_parse_pairs(args.input),
                                    idempotency_key=args.idempotency_key)
        if args.run_command == "list":
            return client.list_runs(graph_id=args.graph, status=args.status,
                                    include_archived=args.include_archived)
        if args.run_command == "show":
            return client.get_run(args.run_id)
        if args.run_command == "nodes":
            return client.run_nodes(args.run_id)
        if args.run_command == "events":
            return client.run_events(args.run_id, after=args.after, limit=args.limit)
        if args.run_command == "decisions":
            return client.run_decisions(args.run_id)
        if args.run_command == "verifications":
            return client.run_verifications(args.run_id)
        if args.run_command == "operations":
            return client.run_operations(args.run_id)
        if args.run_command == "diagnostics":
            return client.run_diagnostics(args.run_id)
        if args.run_command == "progress":
            return client.run_progress(args.run_id)
        if args.run_command == "watch":
            return client.wait_for_run(args.run_id, timeout=args.timeout,
                                       interval=args.interval)
        if args.run_command == "pause":
            return client.pause_run(args.run_id, reason=args.reason, actor=args.actor)
        if args.run_command == "resume":
            return client.resume_run(args.run_id, reason=args.reason, actor=args.actor)
        if args.run_command == "stop":
            return client.stop_run(args.run_id, reason=args.reason)
        if args.run_command == "archive":
            return client.archive_run(args.run_id)
        if args.run_command == "unarchive":
            return client.unarchive_run(args.run_id)

    if args.command == "wait":
        if args.wait_command == "approve":
            return client.approve_wait(args.node_run_id, reason=args.reason, actor=args.actor)
        if args.wait_command == "reject":
            return client.reject_wait(args.node_run_id, reason=args.reason, actor=args.actor)
        if args.wait_command == "resume":
            return client.resume_wait(args.node_run_id, event_type=args.event_type,
                                      payload=_parse_pairs(args.payload))

    if args.command == "operation" and args.operation_command == "reconcile":
        return client.reconcile_operation(
            args.operation_id, status=args.status,
            reconciliation_ref=args.reconciliation_ref, result_ref=args.result_ref,
            error_code=args.error_code, reason=args.reason)

    if args.command == "artifact":
        text = client.read_artifact(args.ref)
        if args.out:
            with open(args.out, "w", encoding="utf-8") as handle:
                handle.write(text)
            return {"written": args.out, "chars": len(text)}
        return text

    if args.command == "storage":
        return client.storage_report()

    if args.command == "budget":
        if args.global_bytes is None and not args.graph:
            return client.get_budget()
        graphs = {}
        for item in args.graph or []:
            graph_id, _, raw = item.partition("=")
            graphs[graph_id] = None if raw in ("", "none", "null") else int(raw)
        return client.set_budget(global_bytes=args.global_bytes, graphs=graphs)

    if args.command == "retention":
        if args.retention_command == "preview":
            return client.retention_preview()
        if args.retention_command == "sweep":
            return client.retention_sweep()
        if args.retention_command == "audit":
            return client.retention_audit()

    raise SystemExit(f"unhandled command: {args.command}")


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        with AnchorClient(args.api_url, token_file=args.token_file) as client:
            _emit(dispatch(client, args))
    except AnchorApiError as exc:
        _emit({"error": exc.as_dict()}, stream=sys.stderr)
        return 1
    except (OSError, RuntimeError, ValueError) as exc:
        _emit({"error": {"code": "client_error", "detail": str(exc)}}, stream=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
