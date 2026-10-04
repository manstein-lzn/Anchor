"""Versioned, bounded local RPC facade for the existing Python GraphCalls service.

This is groundwork for a future host adapter. It is deliberately not a Rust Runner
transport and accepts no filesystem paths, credentials, or caller-selected Graphs.
"""
from __future__ import annotations

import hashlib
import json
import struct
from typing import BinaryIO

from anchor.simple import graph as graph_module
from anchor.simple import run as runner

PROTOCOL_VERSION = 1
MAX_FRAME_BYTES = 1_048_576
_HEADER = struct.Struct("!I")


class RpcError(ValueError):
    pass


def _canonical_digest(value: object) -> str:
    data = json.dumps(value, ensure_ascii=False, sort_keys=True,
                       separators=(",", ":"), allow_nan=False).encode("utf-8")
    return hashlib.sha256(data).hexdigest()


def _kernel_snapshot(snapshot: dict) -> dict:
    """Serialize the Python expanded graph with Rust GraphSnapshot serde defaults."""
    graph = graph_module.to_dict(graph_module.parse(snapshot))
    agents = {}
    for name, agent in graph.get("agents", {}).items():
        agents[name] = {"model": agent.get("model", ""),
                        "instructions": agent.get("instructions", ""),
                        "network": agent.get("network", False),
                        "max_steps": agent.get("max_steps"),
                        "wall_time_limit_seconds": agent.get("wall_time_limit_seconds"),
                        "reads": agent.get("reads", []), "writes": agent.get("writes", [])}
    nodes = []
    for node in graph["nodes"]:
        nodes.append({"id": node["id"], "agent": node.get("agent"), "op": node.get("op"),
                      "with": node.get("with"), "plugins": node.get("plugins", []),
                      "max_rounds": node.get("max_rounds")})
    return {"objective": graph.get("objective", ""), "input": graph.get("input", {}),
            "entry": graph["entry"], "agents": agents, "ops": graph.get("ops", {}),
            "nodes": nodes, "edges": graph.get("edges", []),
            "_module_rounds": graph.get("_module_rounds", {})}


def encode_frame(value: dict) -> bytes:
    payload = json.dumps(value, ensure_ascii=False, separators=(",", ":"),
                         allow_nan=False).encode("utf-8")
    if len(payload) > MAX_FRAME_BYTES:
        raise RpcError("RPC frame exceeds size limit")
    return _HEADER.pack(len(payload)) + payload


def _read_exact(stream: BinaryIO, count: int) -> bytes:
    chunks = bytearray()
    while len(chunks) < count:
        part = stream.read(count - len(chunks))
        if not part:
            raise RpcError("truncated RPC frame")
        chunks.extend(part)
    return bytes(chunks)


def read_frame(stream: BinaryIO) -> dict:
    size, = _HEADER.unpack(_read_exact(stream, _HEADER.size))
    if size == 0 or size > MAX_FRAME_BYTES:
        raise RpcError("invalid RPC frame size")
    try:
        value = json.loads(_read_exact(stream, size))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise RpcError("invalid RPC JSON") from exc
    if not isinstance(value, dict):
        raise RpcError("RPC request must be a JSON object")
    return value


def write_frame(stream: BinaryIO, value: dict) -> None:
    stream.write(encode_frame(value))
    stream.flush()


def _exact_fields(value: object, fields: set[str], label: str) -> dict:
    if not isinstance(value, dict) or set(value) != fields:
        raise RpcError(f"{label} has missing or unknown fields")
    return value


class GraphCallRpc:
    """Dispatch only calls whose durable parent and call identity match local facts."""

    def __init__(self, scheduler):
        self.scheduler = scheduler

    def handle(self, request: object) -> dict:
        try:
            return {"version": PROTOCOL_VERSION, "ok": True, "result": self._handle(request)}
        except (RpcError, ValueError, OSError, KeyError, TypeError, RuntimeError) as exc:
            return {"version": PROTOCOL_VERSION, "ok": False,
                    "error": {"code": "rejected", "message": str(exc)}}

    def _handle(self, request: object) -> dict:  # noqa: C901 - identity checks deliberately stay together
        req = _exact_fields(request, {"version", "op", "identity", "spec", "input"}, "request")
        if req["version"] != PROTOCOL_VERSION:
            raise RpcError("unsupported RPC protocol version")
        if req["op"] not in ("invoke", "status", "cancel"):
            raise RpcError("unsupported RPC operation")
        ident = _exact_fields(req["identity"], {
            "parent_run_id", "parent_graph_digest", "node_id", "invocation", "call_spec_digest"
        }, "identity")
        if (not all(isinstance(ident[key], str) and ident[key] for key in
                    ("parent_run_id", "parent_graph_digest", "node_id", "call_spec_digest")) or
                type(ident["invocation"]) is not int or ident["invocation"] < 1):
            raise RpcError("invalid call identity")
        spec = req["spec"]
        if not isinstance(spec, dict) or set(spec) - {"graph", "mode", "input", "input_map", "files", "result", "session"}:
            raise RpcError("call spec has unknown fields")
        if not isinstance(spec, dict) or _canonical_digest(spec) != ident["call_spec_digest"]:
            raise RpcError("call spec digest mismatch")
        parent_dir = self.scheduler.run_dir(ident["parent_run_id"])
        if parent_dir is None:
            raise RpcError("parent Run does not exist")
        source_workspace = parent_dir.parent.parent
        if self.scheduler.workspace(source_workspace.name) != source_workspace:
            raise RpcError("parent Graph identity is unavailable")
        snapshot_path = parent_dir / "graph.json"
        if not snapshot_path.is_file():
            raise RpcError("parent Run has no frozen Graph snapshot")
        snapshot = json.loads(snapshot_path.read_text(encoding="utf-8"))
        normalized_snapshot = _kernel_snapshot(snapshot)
        if _canonical_digest(normalized_snapshot) != ident["parent_graph_digest"]:
            raise RpcError("parent Graph digest mismatch")
        graph = graph_module.parse(snapshot)
        node = graph.nodes.get(ident["node_id"])
        if node is None:
            raise RpcError("call node is absent from parent snapshot")
        op = graph.ops.get(node.op)
        frozen_spec = getattr(op, "call", None)
        if frozen_spec != spec:
            raise RpcError("call spec does not match parent Graph snapshot")
        if spec.get("files") or spec.get("result") or spec.get("session"):
            raise RpcError("file, result, and session artifacts are not supported by RPC v1")
        if not isinstance(req["input"], dict):
            raise RpcError("call input must be a JSON object")
        control = parent_dir / "control" / ".graph-calls" / hashlib.sha256(
            json.dumps([ident["node_id"], ident["invocation"]], separators=(",", ":")).encode()
        ).hexdigest()
        record_path = control / "graph-call.json"
        if req["op"] == "status":
            if not record_path.is_file():
                raise RpcError("call identity has not been admitted")
            record = json.loads(record_path.read_text(encoding="utf-8"))
            self._record_matches(record, ident, spec, req["input"])
            if record.get("input") != req["input"]:
                raise RpcError("call identity was already admitted with different input")
            child = self.scheduler.run_dir(record["run"])
            status = "missing" if child is None else runner.RunState.load(child).status
            return {"graph": record["graph"], "run": record["run"], "mode": record["mode"], "status": status}
        if req["op"] == "cancel":
            if not record_path.is_file():
                raise RpcError("call identity has not been admitted")
            record = json.loads(record_path.read_text(encoding="utf-8"))
            self._record_matches(record, ident, spec, req["input"])
            if record.get("input") != req["input"]:
                raise RpcError("call identity was already admitted with different input")
            if record["mode"] != "wait":
                raise RpcError("only wait calls can be cancelled")
            body, status = self.scheduler.control_run(record["run"], "stop", cause="graph_call")
            return {"accepted": status == 202, "response": json.loads(body)}
        # RPC input is already mapped by the caller. Do not accept arbitrary input paths or
        # silently re-evaluate input_map against a different Python Run input.
        if spec.get("input_map"):
            raise RpcError("input_map must be resolved by a trusted adapter before RPC v1")
        if record_path.is_file():
            prior = json.loads(record_path.read_text(encoding="utf-8"))
            self._record_matches(prior, ident, spec, req["input"])
            if prior.get("input") != req["input"]:
                raise RpcError("call identity was already admitted with different input")
        adapted = dict(spec)
        adapted["input"] = req["input"]
        return self.scheduler.graph_calls.invoke(
            source_workspace=source_workspace, source_run_id=ident["parent_run_id"],
            spec=adapted, node_id=ident["node_id"], invocation=ident["invocation"],
            directory=parent_dir / ident["node_id"], control=control,
            run_input={}, inputs=(), cancelled=lambda: False)

    @staticmethod
    def _record_matches(record: dict, identity: dict, spec: dict, input_value: dict) -> None:
        trigger = record.get("trigger", {})
        if (trigger.get("run"), trigger.get("node"), trigger.get("invocation")) != (
                identity["parent_run_id"], identity["node_id"], identity["invocation"]):
            raise RpcError("admitted call belongs to another identity")
        expected = dict(spec)
        expected["input"] = input_value
        if record.get("spec") != expected:
            raise RpcError("admitted call spec mismatch")


def serve_one(stream_in: BinaryIO, stream_out: BinaryIO, facade: GraphCallRpc) -> None:
    """Handle one bounded request/response frame on an already-controlled local stream."""
    try:
        response = facade.handle(read_frame(stream_in))
    except RpcError as exc:
        response = {"version": PROTOCOL_VERSION, "ok": False,
                    "error": {"code": "protocol_error", "message": str(exc)}}
    write_frame(stream_out, response)
