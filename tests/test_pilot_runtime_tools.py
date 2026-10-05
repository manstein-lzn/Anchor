"""Pilot uses the public Runtime ports without a local Graph or Run store."""

from __future__ import annotations

import asyncio
import hashlib
import json

import pytest
from pydantic_ai import Agent, DeferredToolRequests
from pydantic_ai.messages import ModelRequest, ModelResponse, TextPart, ToolCallPart, ToolReturnPart
from pydantic_ai.models.function import FunctionModel

from anchor.pilot import PilotDeps, _register_tools, approval_precondition
from anchor.runtime_http import RuntimeHTTPError
from anchor.session import SessionStore


class _Library:
    def catalog(self):
        return [{"id": "existing", "available": True}]

    def detail(self, plugin):
        return {"id": plugin, "instructions": "Existing host instructions"}

    def attach(self, _plugins):
        raise AssertionError("Pilot must delegate Graph validation to the Runtime port")


class _RuntimePort:
    def __init__(self, sessions):
        self.sessions = sessions
        self.library = _Library()
        self.definitions = {"demo": {"entry": "module", "objective": "Remote objective",
                                     "nodes": [{"id": "module", "graph": "child"}], "edges": [],
                                     "graphs": {"child": {"entry": "work", "ops": {"work": {"run": "true"}},
                                                          "nodes": [{"id": "work", "op": "work"}], "edges": []}}}}
        self.run_records = {}
        self.active = {"demo": []}
        self.calls = []
        self.private_reads = []
        self.triggered = []
        self.controls = []
        self.deleted = []
        self.unavailable = False
        self.graph_response = None
        self.validation_response = ({"valid": True, "nodes": ["module/work"], "entry": "module/work"}, 200)

    def __getattr__(self, name):
        if name in {"root", "workspace", "workspaces", "running", "run_dir"}:
            self.private_reads.append(name)
            raise AssertionError(f"Pilot accessed the private Runtime attribute: {name}")
        raise AttributeError(name)

    def _check(self, method, *args):
        self.calls.append((method, args))
        if self.unavailable:
            raise RuntimeHTTPError("fixture Runtime unavailable", 503)

    def graphs(self):
        self._check("graphs")
        return {"graphs": [{"graph": graph, "running": None, "active_runs": self.active.get(graph, [])}
                           for graph in self.definitions]}

    def graph(self, graph):
        self._check("graph", graph)
        if self.graph_response is not None:
            value, status = self.graph_response
            return json.dumps(value), status
        if graph not in self.definitions:
            return json.dumps({"error": "no such graph"}), 404
        return json.dumps({"graph": graph, "definition": self.definitions[graph]}), 200

    def validate_graph(self, definition):
        self._check("validate_graph", definition)
        value, status = self.validation_response
        return json.dumps(value), status

    def create(self, graph, definition):
        self._check("create", graph, definition)
        self.definitions[graph] = definition
        return json.dumps({"graph": graph, "saved": True}), 201

    def save(self, graph, definition):
        self._check("save", graph, definition)
        self.definitions[graph] = definition
        return json.dumps({"graph": graph, "saved": True}), 200

    def delete_graph(self, graph):
        self._check("delete_graph", graph)
        self.deleted.append(graph)
        del self.definitions[graph]
        return json.dumps({"graph": graph, "deleted": True}), 200

    def trigger(self, graph, objective):
        self._check("trigger", graph, objective)
        self.triggered.append((graph, objective))
        run = f"remote-{len(self.triggered)}"
        self.run_records[run] = {"run": run, "graph": graph, "backend": "rust",
                                 "state": {"status": "running", "objective": objective},
                                 "nodes": ["module/work"], "traces": {}, "control_requested": None}
        self.active[graph] = [run]
        return json.dumps({"run": run, "graph": graph}), 202

    def runs(self):
        self._check("runs")
        return [{"run": run, "graph": value["graph"], "status": value["state"]["status"], "backend": "rust"}
                for run, value in self.run_records.items()]

    def run(self, graph, run):
        self._check("run", graph, run)
        assert graph == ""
        return self.run_records.get(run)

    def control_run(self, run, action):
        self._check("control_run", run, action)
        self.controls.append((run, action))
        self.run_records[run]["control_requested"] = action
        return json.dumps({"run": run, "asked": action}), 202

    def read_file(self, run, node, path):
        self._check("read_file", run, node, path)
        return json.dumps({"path": path, "text": "Remote evidence", "binary": False}), 200


@pytest.fixture
def runtime(tmp_path):
    sessions = SessionStore(tmp_path)
    sessions.create("pilot")
    port = _RuntimePort(sessions)
    yield port
    assert not port.private_reads
    assert not (tmp_path / "workspaces").exists()


def invoke(runtime, name, args=None, *, history=None, deferred=None):
    def model(messages, _info):
        answered = any(isinstance(part, ToolReturnPart)
                       for message in messages if isinstance(message, ModelRequest)
                       for part in message.parts)
        return ModelResponse(parts=[TextPart(content="Handled")]) if answered else ModelResponse(parts=[
            ToolCallPart(tool_name=name, args=args or {}, tool_call_id=f"call-{name}")])

    agent = Agent(FunctionModel(model), output_type=[str, DeferredToolRequests])
    _register_tools(agent)
    kwargs = {"message_history": history} if history is not None else {}
    if deferred is not None:
        kwargs["deferred_tool_results"] = deferred
    return asyncio.run(agent.run(None if history else "Use the requested tool",
                                 deps=PilotDeps(runtime, "pilot"), **kwargs))


def returned(result):
    return next(part.content for message in result.all_messages() if isinstance(message, ModelRequest)
                for part in message.parts if isinstance(part, ToolReturnPart))


def delete_proposal(runtime, precondition=None):
    proposal = invoke(runtime, "graph_delete", {"graph": "demo"})
    assert isinstance(proposal.output, DeferredToolRequests)
    call = proposal.output.approvals[0]
    if precondition is None:
        precondition = approval_precondition(runtime, call.tool_name, call.args_as_dict())
    runtime.sessions.set_pending("pilot", [{"tool_call_id": call.tool_call_id, "action": call.tool_name,
                                            "target": "demo", "proposal": call.args_as_dict(),
                                            "precondition": precondition}])
    approved = proposal.output.build_results(approvals={call.tool_call_id: True})
    return proposal, approved


def test_graph_and_plugin_reads_and_validation_use_public_ports(runtime, monkeypatch):
    def no_python_parser(*_args, **_kwargs):
        raise AssertionError("Pilot must not parse a Runtime definition with the Python parser")

    monkeypatch.setattr("anchor.simple.graph.parse", no_python_parser)
    listing = returned(invoke(runtime, "graph_list"))
    assert listing == [{"graph": "demo", "running": None, "active_runs": [],
                        "objective": "Remote objective", "nodes": ["module"]}]
    assert returned(invoke(runtime, "graph_read", {"graph": "demo"}))["definition"] == runtime.definitions["demo"]
    validated = returned(invoke(runtime, "graph_validate", {"definition": runtime.definitions["demo"]}))
    assert validated == {"valid": True, "nodes": ["module/work"], "entry": "module/work", "http_status": 200}
    assert ("validate_graph", (runtime.definitions["demo"],)) in runtime.calls
    assert returned(invoke(runtime, "plugin_list")) == [{"id": "existing", "available": True}]
    assert returned(invoke(runtime, "plugin_read", {"plugin": "existing"}))["instructions"] == "Existing host instructions"


def test_graph_create_and_update_use_public_mutation_ports(runtime):
    definition = {**runtime.definitions["demo"], "objective": "New objective"}
    assert returned(invoke(runtime, "graph_create", {"name": "new", "definition": definition}))["http_status"] == 201
    changed = {**definition, "objective": "Updated objective"}
    assert returned(invoke(runtime, "graph_update", {"graph": "new", "definition": changed}))["http_status"] == 200
    assert runtime.definitions["new"] == changed
    assert ("create", ("new", definition)) in runtime.calls and ("save", ("new", changed)) in runtime.calls


def test_run_association_artifacts_controls_and_reopened_session_observe_same_runtime_run(runtime, tmp_path):
    started = returned(invoke(runtime, "graph_run", {"graph": "demo", "objective": "Requested objective"}))
    assert started == {"run": "remote-1", "graph": "demo", "http_status": 202, "session": "pilot"}
    assert runtime.triggered == [("demo", "Requested objective")]
    assert runtime.sessions.get("pilot").run_ids == ["remote-1"]
    assert "run.started" in [event.kind for event in runtime.sessions.events("pilot")]
    assert returned(invoke(runtime, "run_list"))[0]["run"] == "remote-1"
    assert returned(invoke(runtime, "run_status", {"run": "remote-1"})) == runtime.run_records["remote-1"]
    artifact = returned(invoke(runtime, "artifact_read", {"run": "remote-1", "node": "module/work", "path": "report.md"}))
    assert artifact["text"] == "Remote evidence" and artifact["http_status"] == 200
    assert ("read_file", ("remote-1", "module/work", "report.md")) in runtime.calls
    for action in ["pause", "resume", "stop"]:
        reply = returned(invoke(runtime, f"run_{action}", {"run": "remote-1"}))
        assert reply["http_status"] == 202 and reply["asked"] == action
    assert runtime.controls == [("remote-1", action) for action in ["pause", "resume", "stop"]]
    runtime.sessions = SessionStore(tmp_path)
    waiting = returned(invoke(runtime, "session_wait"))
    assert waiting["session"]["run_ids"] == ["remote-1"]
    assert waiting["runs"] == [runtime.run_records["remote-1"]]
    assert waiting["runs"][0]["control_requested"] == "stop"
    assert waiting["runs"][0]["state"]["status"] == "running"


@pytest.mark.parametrize("active_field", ["running", "active_runs"])
def test_starting_run_fallback_uses_public_graph_activity(runtime, monkeypatch, active_field):
    record = {"graph": "demo", "running": "pending" if active_field == "running" else None,
              "active_runs": ["pending"] if active_field == "active_runs" else []}
    monkeypatch.setattr(runtime, "graphs", lambda: {"graphs": [record]})
    runtime.sessions.attach_run("pilot", "pending")
    expected = {"run": "pending", "graph": "demo", "status": "starting", "running": True}
    assert returned(invoke(runtime, "run_status", {"run": "pending"})) == expected
    assert returned(invoke(runtime, "session_wait"))["runs"] == [expected]
    assert returned(invoke(runtime, "run_status", {"run": "missing"}))["http_status"] == 404


@pytest.mark.parametrize("name,args", [
    ("graph_list", {}), ("graph_read", {"graph": "demo"}), ("graph_validate", {"definition": {}}),
    ("run_list", {}), ("run_status", {"run": "remote-1"}),
    ("artifact_read", {"run": "remote-1", "node": "module/work", "path": "report.md"}),
    ("session_wait", {}), ("graph_create", {"name": "new", "definition": {}}),
    ("graph_update", {"graph": "demo", "definition": {}}), ("graph_run", {"graph": "demo"}),
    ("run_stop", {"run": "remote-1"}),
])
def test_backend_outage_is_an_explicit_tool_error_and_does_not_claim_empty_or_success(runtime, name, args):
    runtime.sessions.attach_run("pilot", "remote-1")
    runtime.unavailable = True
    result = returned(invoke(runtime, name, args))
    assert result["http_status"] == 503 and result["error"] == "fixture Runtime unavailable"
    assert result.get("valid") is not False
    assert runtime.triggered == runtime.controls == runtime.deleted == []
    assert "new" not in runtime.definitions


def test_graph_listing_preserves_per_graph_read_failures(runtime):
    runtime.graph_response = ({"error": "Graph backend unavailable"}, 503)
    assert returned(invoke(runtime, "graph_list")) == [{"graph": "demo", "error": "Graph backend unavailable",
                                                       "http_status": 503}]


def test_validation_preserves_invalid_definition_result(runtime):
    runtime.validation_response = ({"valid": False, "error": "missing Plugin"}, 422)
    assert returned(invoke(runtime, "graph_validate", {"definition": {}})) == {
        "valid": False, "error": "missing Plugin", "http_status": 422,
    }


def test_delete_snapshot_is_stable_across_definition_object_key_order(runtime):
    proposal, approved = delete_proposal(runtime)
    definition = runtime.definitions["demo"]
    runtime.definitions["demo"] = {key: definition[key] for key in reversed(definition)}
    runtime.definitions["demo"]["nodes"] = [{"graph": "child", "id": "module"}]
    result = returned(invoke(runtime, "graph_delete", history=proposal.all_messages(), deferred=approved))
    assert result["deleted"] is True and result["http_status"] == 200
    assert runtime.deleted == ["demo"]


def test_delete_refuses_definition_changed_after_confirmation(runtime):
    proposal, approved = delete_proposal(runtime)
    runtime.definitions["demo"]["objective"] = "Changed after the request"
    result = returned(invoke(runtime, "graph_delete", history=proposal.all_messages(), deferred=approved))
    assert result["changed"] is True
    assert not runtime.deleted and not runtime.sessions.get("pilot").operations


@pytest.mark.parametrize("raise_error", [False, True])
def test_delete_refuses_backend_failure_without_capturing_absence_or_executing(runtime, raise_error):
    proposal, approved = delete_proposal(runtime)
    if raise_error:
        runtime.unavailable = True
    else:
        runtime.graph_response = ({"error": "Graph backend unavailable"}, 503)
    result = returned(invoke(runtime, "graph_delete", history=proposal.all_messages(), deferred=approved))
    assert result["http_status"] == 503 and "unavailable" in result["error"]
    assert not runtime.deleted and "demo" in runtime.definitions
    assert not runtime.sessions.get("pilot").operations


def test_old_raw_bytes_hash_requires_a_new_confirmation(runtime):
    raw = json.dumps(runtime.definitions["demo"], indent=2).encode()
    precondition = {"graph": "demo", "expected_sha256": hashlib.sha256(raw).hexdigest(), "existed": True}
    assert precondition != approval_precondition(runtime, "graph_delete", {"graph": "demo"})
    proposal, approved = delete_proposal(runtime, precondition)
    result = returned(invoke(runtime, "graph_delete", history=proposal.all_messages(), deferred=approved))
    assert result["changed"] is True and not runtime.deleted


def test_approval_snapshot_only_accepts_a_real_not_found_response_as_absence(runtime):
    assert approval_precondition(runtime, "graph_delete", {"graph": "missing"}) == {
        "graph": "missing", "expected_sha256": None, "existed": False,
    }
    runtime.graph_response = ({"error": "Graph backend unavailable"}, 503)
    with pytest.raises(RuntimeError, match="Graph backend unavailable"):
        approval_precondition(runtime, "graph_delete", {"graph": "demo"})
    runtime.graph_response = ({"graph": "demo"}, 200)
    assert returned(invoke(runtime, "graph_read", {"graph": "demo"}))["http_status"] == 502
    with pytest.raises(RuntimeError, match="definition response is invalid"):
        approval_precondition(runtime, "graph_delete", {"graph": "demo"})


def test_missing_run_with_unavailable_graph_listing_is_not_reported_as_nonexistent(runtime, monkeypatch):
    monkeypatch.setattr(runtime, "run", lambda _graph, _run: None)
    runtime.unavailable = True
    runtime.sessions.attach_run("pilot", "pending")
    assert returned(invoke(runtime, "run_status", {"run": "pending"}))["http_status"] == 503
    assert returned(invoke(runtime, "session_wait"))["http_status"] == 503
