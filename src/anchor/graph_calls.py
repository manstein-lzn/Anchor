"""Independent Graph calls, persisted at the existing Run/node control boundary."""
from __future__ import annotations

import hashlib
import json
import os
import shutil
import threading
import tempfile
import time
from pathlib import Path, PurePosixPath
from typing import TYPE_CHECKING

from anchor.library import record_bindings
from anchor.session import SessionStore
from anchor.simple import graph as graph_module
from anchor.simple import run as runner

if TYPE_CHECKING:
    from anchor.serve import Scheduler


def _write(path: Path, value: dict) -> None:
    SessionStore._atomic(path, json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def _relative(value: str) -> Path:
    if (not isinstance(value, str) or not value or "\\" in value or
            any(part in ("", ".", "..", ".git") for part in value.split("/")) or
            PurePosixPath(value).is_absolute()):
        raise ValueError(f"unsafe selected file path: {value!r}")
    return Path(value)


def _file(tree: Path, name: str) -> Path:
    relative = _relative(name)
    if tree.is_symlink():
        raise ValueError("selected file snapshot must not be a symlink")
    current = tree
    for part in relative.parts:
        current = current / part
        if current.is_symlink():
            raise ValueError(f"selected files must not traverse symlinks: {name}")
    if not current.is_file():
        raise ValueError(f"selected file does not exist: {name}")
    return current


def _pointer(value: dict, pointer: str):
    if pointer == "":
        return value
    if not isinstance(pointer, str) or not pointer.startswith("/"):
        raise ValueError("input_map values must be RFC6901 JSON pointers")
    import re
    for raw in pointer[1:].split("/"):
        if re.search(r"~(?![01])", raw):
            raise ValueError(f"invalid JSON pointer: {pointer}")
        key = raw.replace("~1", "/").replace("~0", "~")
        try:
            if isinstance(value, list):
                if not key.isascii() or not key.isdigit() or (key != "0" and key.startswith("0")):
                    raise KeyError(key)
                value = value[int(key)]
            else:
                value = value[key]
        except (KeyError, IndexError, TypeError):
            raise ValueError(f"missing input_map pointer: {pointer}") from None
    return value


class GraphCalls:
    def __init__(self, scheduler: Scheduler):
        self.scheduler = scheduler
        self.active: dict[str, str] = {}
        self.lock = threading.RLock()

    def factory(self, source_workspace: Path, source_run_id: str):
        def call(**kwargs):
            return self.invoke(source_workspace=source_workspace, source_run_id=source_run_id, **kwargs)
        return call

    def _ancestry(self, workspace: Path, run_id: str, target: str) -> str:
        graphs = {workspace.name}
        seen = set()
        root = run_id
        current = workspace / "runs" / run_id
        while (current / "run.json").exists():
            if str(current) in seen:
                raise ValueError("cyclic Graph call ancestry")
            seen.add(str(current))
            trigger = json.loads((current / "run.json").read_text()).get("trigger", {})
            if trigger.get("source") != "graph_call":
                break
            graphs.add(trigger["graph"])
            root = trigger.get("root_run", trigger["run"])
            parent = self.scheduler.workspace(trigger["graph"])
            if parent is None:
                raise ValueError("Graph call ancestor is missing")
            current = parent / "runs" / trigger["run"]
        if target in graphs:
            raise ValueError(f"recursive Graph call is not supported: {target}")
        return root

    def invoke(self, *, source_workspace: Path, source_run_id: str, spec: dict,
               node_id: str, invocation: int, directory: Path, control: Path,
               run_input: dict, inputs: tuple, cancelled):
        control, directory = Path(control), Path(directory)
        path = control / "graph-call.json"
        with self.lock, self.scheduler.lock:
            if path.exists():
                record = json.loads(path.read_text())
            else:
                identity = json.dumps([source_workspace.name, source_run_id, node_id, invocation])
                run_id = "call-" + hashlib.sha256(identity.encode()).hexdigest()[:32]
                workspace = self.scheduler.workspace(spec["graph"])
                if workspace is None:
                    raise ValueError(f"no such target graph: {spec['graph']}")
                child = workspace / "runs" / run_id
                admission = child / "admission.json"
                if admission.exists():
                    record = json.loads(admission.read_text())
                    expected = {"graph": source_workspace.name, "run": source_run_id,
                                "node": node_id, "invocation": invocation}
                    if any(record.get("trigger", {}).get(key) != value for key, value in expected.items()):
                        raise ValueError("Graph call admission belongs to a different source invocation")
                else:
                    record = self._prepare_record(source_workspace, source_run_id, node_id, invocation,
                                                  spec, run_input, inputs, cancelled, workspace, child, run_id)
                if not (child / "run.json").exists():
                    _write(child / "graph.json", record["definition"])
                    graph = graph_module.parse(record["definition"])
                    runner.RunState(objective=graph.objective, started=runner._now(),
                                    input=record["input"], trigger=record["trigger"]).save(child)
                _write(path, record)
            expected = {"graph": source_workspace.name, "run": source_run_id,
                        "node": node_id, "invocation": invocation}
            if any(record.get("trigger", {}).get(key) != value for key, value in expected.items()):
                raise ValueError("Graph call record belongs to a different source invocation")
            workspace = self.scheduler.workspace(record["graph"])
            if workspace is None:
                raise RuntimeError("accepted Graph call target is missing")
            child = workspace / "runs" / record["run"]
            if not (child / "run.json").exists():
                raise RuntimeError("accepted Graph call Run is missing; refusing replacement")
            cancellation = child / "call-cancellation.json"
            if cancellation.exists() and record["mode"] == "wait" and not cancelled():
                state = runner.RunState.load(child)
                if state.status == "stopped" and record["run"] not in self.active:
                    state.status = "running"
                    state.save(child)
                cancellation.unlink()
            self.start(workspace, record["run"])
        if record["mode"] == "detach":
            response = {key: record[key] for key in ("graph", "run", "mode")}
            response["status"] = "accepted"
        else:
            state = self._wait(record, child, source_run_id, node_id, invocation, cancelled)
            response = self._result(record, child, directory, state)
        _write(directory / "call.json", response)
        return response

    def _bundle(self, child: Path, spec: dict, inputs: tuple) -> None:
        bundle = child / "call-inputs"
        visible = {item["node"]: Path(item["tree"]) for item in inputs}
        with tempfile.TemporaryDirectory(prefix=".call-inputs-", dir=child) as temporary:
            staged = Path(temporary)
            destinations = set()
            for selection in spec.get("files", []):
                if selection["node"] not in visible:
                    raise ValueError(f"file source is not visible to caller: {selection['node']}")
                selected = _file(visible[selection["node"]], selection["path"])
                relative = _relative(selection["as"])
                if any(relative == other or relative in other.parents or other in relative.parents
                       for other in destinations):
                    raise ValueError(f"overlapping selected file destination: {relative}")
                destinations.add(relative)
                output = staged / relative
                output.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(selected, output)
                with output.open("rb") as stream:
                    os.fsync(stream.fileno())
            if bundle.exists():
                shutil.rmtree(bundle)  # this invocation has not been accepted yet
            staged.replace(bundle)

    def _prepare_record(self, source_workspace, source_run_id, node_id, invocation,
                        spec, run_input, inputs, cancelled, workspace, child, run_id) -> dict:
        admission = child / "admission.json"
        if cancelled():
            raise RuntimeError("Graph call cancelled before admission")
        session_context = None
        if spec.get("session"):
            try:
                from anchor.channel.background import validate
            except ImportError as exc:
                raise ValueError("Graph calls to channel sessions are not supported by this service") from exc
            session_context = validate(self.scheduler, source_workspace, source_run_id, spec)
        root = self._ancestry(source_workspace, source_run_id, spec["graph"])
        definition = json.loads((workspace / "graph.json").read_text())
        graph = graph_module.parse(definition)
        result = spec.get("result")
        if result and result["node"] not in graph.nodes:
            raise ValueError(f"no such result node: {result['node']}")
        values = dict(spec.get("input", {}))
        values.update({key: _pointer(run_input, pointer)
                       for key, pointer in spec.get("input_map", {}).items()})
        resolved = graph_module.merge_input(graph.input, values)
        record = {"graph": workspace.name, "run": run_id, "mode": spec["mode"],
                  "status": "accepted", "node": node_id, "invocation": invocation,
                  "input": resolved, "spec": spec,
                  "definition": graph_module.to_dict(graph),
                  "trigger": {"source": "graph_call", "graph": source_workspace.name,
                              "run": source_run_id, "node": node_id,
                              "invocation": invocation, "mode": spec["mode"],
                              "root_run": root}}
        if session_context is not None:
            record["session_pending"] = True
            record["session_context"] = session_context
            record["trigger"]["session"] = spec["session"]
        child.mkdir(parents=True, exist_ok=True)
        self._bundle(child, spec, inputs)
        bindings = {node.id: self.scheduler.library.attach(node.plugins)
                    for node in graph.nodes.values() if node.plugins}
        record_bindings(child, bindings, resume=False)
        local = runner._local_inputs(workspace, graph.nodes)
        if local:
            (child / "local-inputs.json").write_text(
                json.dumps(local, ensure_ascii=False, sort_keys=True, indent=2))
        _write(admission, record)
        return record

    def _wait(self, record, child, source_run_id, node_id, invocation, cancelled):
        while True:
            if cancelled():
                with self.scheduler.lock:
                    if (record["run"] in self.active and
                            self.scheduler.control.get(record["run"]) != "stopped"):
                        _write(child / "call-cancellation.json", {"parent": source_run_id,
                                                                 "node": node_id,
                                                                 "invocation": invocation})
                        self.scheduler.control_run(record["run"], "stop", cause="graph_call")
                while record["run"] in self.active:
                    time.sleep(0.05)
                raise RuntimeError("waiting Graph call cancelled")
            state = runner.RunState.load(child)
            if state.status != "running" and record["run"] not in self.active:
                break
            time.sleep(0.05)
        if state.status != "finished":
            raise RuntimeError(f"called Graph {record['graph']} Run {record['run']} "
                               f"{state.status}: {state.error or state.reason}")
        return state

    def _result(self, record, child, directory, state) -> dict:
        response = {key: record[key] for key in ("graph", "run", "mode")}
        response["status"] = state.status
        result = record["spec"].get("result")
        if result:
            output = state.result(result["node"])
            if output is None or not output.commit:
                raise ValueError(f"called Graph did not produce result node: {result['node']}")
            snapshot = runner._given(child, output)
            response["summary"] = output.submission
            files = result.get("files", [])
            for name in files:
                source = _file(snapshot.tree, name)
                destination = directory / "result" / _relative(name)
                # Prior invocations may leave files, but must never redirect writes.
                current = directory
                for part in destination.relative_to(directory).parts:
                    current = current / part
                    if current.is_symlink():
                        raise ValueError("result destination must not traverse symlinks")
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source, destination)
            response["result"] = {"node": output.node_id, "commit": output.commit, "files": files}
        return response

    def start(self, workspace: Path, run_id: str) -> None:
        """Schedule an already admitted native Run at most once in this process."""
        with self.scheduler.lock:
            if run_id in self.active:
                return
            child = workspace / "runs" / run_id
            admission = child / "admission.json"
            pending = admission.exists() and json.loads(admission.read_text()).get("session_pending")
            if runner.RunState.load(child).status != "running" and not pending:
                return
            self.active[run_id] = workspace.name
        threading.Thread(target=self.scheduler._run, args=(workspace, run_id, None, True),
                         daemon=True).start()

    def projections(self, run_dir: Path) -> list[dict]:
        calls = []
        for path in sorted((run_dir / "control").glob("**/graph-call.json")):
            record = json.loads(path.read_text())
            item = {key: record[key] for key in
                    ("node", "invocation", "graph", "run", "mode", "input") if key in record}
            child = self.scheduler.run_dir(record["run"])
            item["status"] = "missing"
            if child is not None:
                state = runner.RunState.load(child)
                item["status"] = state.status
                admission = child / "admission.json"
                pending = admission.exists() and json.loads(admission.read_text()).get("session_pending")
                if pending:
                    session_id = record.get("spec", {}).get("session")
                    item["status"] = ("running" if session_id in self.scheduler.session_background else "queued")
                result = record.get("spec", {}).get("result")
                output = state.result(result["node"]) if result else None
                if output is not None:
                    item["summary"] = output.submission
            calls.append(item)
        return sorted(calls, key=lambda item: (item["node"], item["invocation"]))

    def relations(self) -> dict:
        graphs, calls = [], []
        for workspace in self.scheduler.workspaces():
            graphs.append({"graph": workspace.name, "schedules": sum(
                item["graph"] == workspace.name for item in self.scheduler.schedules)})
            try:
                graph = graph_module.load(workspace / "graph.json")
            except (ValueError, OSError):
                continue
            for node in graph.nodes.values():
                op = graph.ops.get(node.op)
                spec = getattr(op, "call", None)
                if spec:
                    calls.append({"graph": workspace.name, "node": node.id, "op": node.op,
                                  "target": spec["graph"], "mode": spec["mode"]})
        return {"graphs": graphs, "calls": calls}

    def validate_targets(self, graph) -> None:
        for op in graph.ops.values():
            spec = getattr(op, "call", None)
            if not spec:
                continue
            workspace = self.scheduler.workspace(spec["graph"])
            if workspace is None:
                raise ValueError(f"no such target graph: {spec['graph']}")
            target = graph_module.load(workspace / "graph.json")
            if spec.get("result") and spec["result"]["node"] not in target.nodes:
                raise ValueError(f"no such target result node: {spec['result']['node']}")
