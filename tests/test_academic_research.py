import asyncio
import json
from pathlib import Path
from uuid import uuid4

import pytest

from anchor.domain.admission import RunRequest
from anchor.domain.conditions import build_condition_context
from anchor.domain.graph import GraphDefinition, GraphVersion, Trigger
from anchor.runtime.academic import SECTIONS, register_academic_behaviors, validate_manuscript
from anchor.runtime.behaviors import BehaviorRegistry
from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.capabilities import AgentCapability, CapabilityRegistry, ToolCapability
from anchor.runtime.control_worker import ControlNodeWorker
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.receiver import DurableExecutionReceiver
from anchor.runtime.resolution import resolve_node_context
from anchor.runtime.sinks import ArtifactCheckpointSink
from anchor.runtime.tool_gateway import SubprocessBackend, ToolGateway
from conftest import make_store


ROOT = Path(__file__).resolve().parents[1]


def setup(tmp_path, metadata=None):
    store = make_store(tmp_path)
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    definition = GraphDefinition.model_validate_json((ROOT / "examples/graphs/academic-research.json").read_text())
    if metadata:
        definition = definition.model_copy(update={"metadata": {**definition.metadata, **metadata}})
    version = store.publish_graph(GraphVersion.publish(definition, 1))
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id, type="manual"))
    receipt = store.admit_run(RunRequest(trigger_id=trigger.id, idempotency_key="academic-test",
        objective="Academic test", inputs={"topic": "Test", "minimum_sources": 1, "minimum_reads": 0}))
    asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
    behaviors = BehaviorRegistry()
    register_academic_behaviors(behaviors)
    worker = ControlNodeWorker(store, artifacts, ArtifactCheckpointSink(store, artifacts, "control"),
                               behaviors=behaviors)
    return store, artifacts, receipt, worker


def complete(store, artifacts, lease, output):
    context = resolve_node_context(store, lease.run_id, lease.node_id, artifacts)
    text = json.dumps(output)
    store.complete_node_and_propagate(lease.claim_id, "agent", output_ref=artifacts.put_text(text),
        input_snapshot=context.snapshot, condition_context=build_condition_context(text, context.snapshot))


def claim(store, node_id):
    lease = store.claim_ready_agent_node("agent", uuid4())
    assert lease is not None and lease.node_id == node_id
    return lease


def source_evidence(store, artifacts, lease, monkeypatch):
    from anchor.runtime import tool_gateway
    result = {"papers": [{"id": "doi:10.1234/test", "title": "Real metadata title", "authors": ["A. Researcher"],
               "year": 2024, "venue": "Test Journal", "url": "https://doi.org/10.1234/test", "evidence_level": "abstract"}],
              "retrieved_at": "2026-09-07T00:00:00Z"}
    calls = []
    def search(*args, **kwargs):
        calls.append(1)
        return json.dumps(result)
    monkeypatch.setattr(tool_gateway, "execute_research", search)
    registry = CapabilityRegistry(agents=[AgentCapability(ref="research", model_ref="m", tool_refs=["scholarly.search"])],
                                 tools=[ToolCapability(ref="scholarly.search")])
    gateway = ToolGateway(store, registry, artifacts, SubprocessBackend())
    operation = uuid4()
    first = gateway.execute(lease, agent_ref="research", tool_ref="scholarly.search", arguments={"query": "test"}, operation_id=operation)
    again = gateway.execute(lease, agent_ref="research", tool_ref="scholarly.search", arguments={"query": "test"}, operation_id=operation)
    assert first == again and len(calls) == 1
    return {"citation": 1, "id": "doi:10.1234/test", "evidence_ref": first.result_ref}


def manuscript():
    return "# A literature review\n\n" + "\n\n".join(
        "## " + section + "\n\n" + ("Supported analysis [1]. " * 45) for section in SECTIONS)


def test_revision_loop_keeps_full_manuscript_and_exports_verified_markdown(tmp_path, monkeypatch):
    store, artifacts, receipt, worker = setup(tmp_path)
    try:
        assert asyncio.run(worker.execute_once(worker_id="control")).node_id == "start"
        complete(store, artifacts, claim(store, "plan"), {"round": 1})
        lease = claim(store, "research")
        source = source_evidence(store, artifacts, lease, monkeypatch)
        work = {"manuscript": manuscript().replace("## Methods", "## Missing"), "sources": [source]}
        complete(store, artifacts, lease, work)
        review_lease = claim(store, "review")
        resolved = resolve_node_context(store, receipt.run_id, "review", artifacts)
        assert resolved.snapshot["research"] == work
        assert len(resolved.snapshot["research"]["manuscript"]) > 4000
        complete(store, artifacts, review_lease, {"verdict": "pass", "issues": []})
        asyncio.run(worker.execute_once(worker_id="control"))
        planner = claim(store, "plan")
        feedback = resolve_node_context(store, receipt.run_id, "plan", artifacts).snapshot["feedback"]
        assert feedback["review"]["verdict"] == "revise"
        assert "Missing paper section: Methods" in feedback["review"]["mechanical_issues"]
        assert feedback["research"] == work
        complete(store, artifacts, planner, {"round": 2, "previous_work": work})
        revised = {"manuscript": manuscript(), "sources": [source]}
        complete(store, artifacts, claim(store, "research"), revised)
        complete(store, artifacts, claim(store, "review"), {"verdict": "pass", "issues": []})
        assert asyncio.run(worker.execute_once(worker_id="control")).node_id == "check"
        report = asyncio.run(worker.execute_once(worker_id="control"))
        assert report.node_id == "report"
        assert store.get_run(receipt.run_id).status.value == "completed"
        text = artifacts.get_text(report.output_ref)
        assert text.startswith(revised["manuscript"].rstrip())
        assert "## References" in text and "Real metadata title" in text
        assert "## Retrieval Evidence" in text and source["evidence_ref"] in text
        assert (artifacts.root / "reports" / str(receipt.run_id) / "report.md").read_text() == text
        assert len([n for n in store.list_node_runs(receipt.run_id) if n.node_id == "plan"]) == 2
    finally:
        store.close()


def test_blocked_review_parks_for_human_and_approval_resumes_planning(tmp_path):
    store, artifacts, receipt, worker = setup(tmp_path)
    try:
        asyncio.run(worker.execute_once(worker_id="control"))
        complete(store, artifacts, claim(store, "plan"), {"round": 1})
        complete(store, artifacts, claim(store, "research"), {"manuscript": manuscript(), "sources": []})
        complete(store, artifacts, claim(store, "review"), {"verdict": "blocked", "blockers": ["Access unavailable"]})
        asyncio.run(worker.execute_once(worker_id="control"))
        waits = store.list_waiting_nodes(receipt.run_id)
        assert len(waits) == 1 and waits[0].node_id == "needs_input"
        assert not (artifacts.root / "reports").exists()
        store.decide_approval(waits[0].id, approved=True, reason="Use accessible sources", actor="operator",
                              output_ref=artifacts.put_text('{"reason":"Use accessible sources"}'))
        assert claim(store, "plan").node_id == "plan"
    finally:
        store.close()


def test_citations_cannot_use_artifacts_without_run_bound_network_operations(tmp_path):
    artifacts = LocalArtifactStore(tmp_path)
    ref = artifacts.put_text(json.dumps({"papers": [{"id": "fake"}]}))
    errors, verified = validate_manuscript({"manuscript": manuscript(),
        "sources": [{"citation": 1, "id": "fake", "evidence_ref": ref}]},
        operations=[], artifacts=artifacts, minimum_sources=1, minimum_reads=0)
    assert not verified
    assert any("successful scholarly search" in error for error in errors)


def test_identical_completed_revision_cycles_surface_to_supervisor_and_continue(tmp_path):
    store, artifacts, receipt, worker = setup(tmp_path)
    try:
        asyncio.run(worker.execute_once(worker_id="control"))
        for round_number in (1, 2):
            complete(store, artifacts, claim(store, "plan"), {"round": round_number})
            complete(store, artifacts, claim(store, "research"), {"manuscript": manuscript(), "sources": []})
            complete(store, artifacts, claim(store, "review"), {"verdict": "revise", "issues": []})
            asyncio.run(worker.execute_once(worker_id="control"))
        # The run is no longer auto-blocked by a round counter. Repeated
        # identical cycles surface mechanical issues and remain in revise
        # state so the supervisor/watchdog can diagnose them.
        waits = store.list_waiting_nodes(receipt.run_id)
        assert len(waits) == 0
        check_nodes = [n for n in store.list_node_runs(receipt.run_id) if n.node_id == "check"]
        latest_check = max(check_nodes, key=lambda n: n.attempt)
        output = json.loads(artifacts.get_text(latest_check.output_ref))
        assert output["review"]["verdict"] == "revise"
        assert any("same research" in issue for issue in output["review"]["mechanical_issues"])
    finally:
        store.close()


def test_export_is_idempotent_and_cannot_overwrite_or_escape(tmp_path):
    artifacts = LocalArtifactStore(tmp_path)
    run_id = uuid4()
    path = artifacts.export_markdown(run_id, "report", "# Original")
    assert artifacts.export_markdown(run_id, "report", "# Original") == path
    with pytest.raises(ValueError, match="different content"):
        artifacts.export_markdown(run_id, "report", "# Replaced")
    with pytest.raises(ValueError, match="invalid export"):
        artifacts.export_markdown(run_id, "../escape", "# Escape")
    assert path.read_text() == "# Original"


def test_retrieval_failure_is_audited_and_returned_to_the_model(tmp_path, monkeypatch):
    from anchor.runtime import tool_gateway
    from anchor.runtime.agent_tools import AgentToolLoop

    store, artifacts, receipt, worker = setup(tmp_path)
    try:
        asyncio.run(worker.execute_once(worker_id="control"))
        complete(store, artifacts, claim(store, "plan"), {"round": 1})
        lease = claim(store, "research")
        agent = AgentCapability(ref="research", model_ref="m", tool_refs=["scholarly.search"])
        registry = CapabilityRegistry(agents=[agent], tools=[ToolCapability(ref="scholarly.search", evidence_json=True)])
        def unavailable(*args, **kwargs):
            raise ValueError("HTTP 429: retry later")
        monkeypatch.setattr(tool_gateway, "execute_research", unavailable)
        gateway = ToolGateway(store, registry, artifacts, SubprocessBackend())
        function = AgentToolLoop(gateway, artifacts)._functions(lease, agent)[0]
        message = asyncio.run(function.call('{"query":"test"}'))
        assert message.startswith("TOOL FAILED") and "429" in message
        operation = store.list_tool_operations(receipt.run_id)[0]
        assert operation.status.value == "failed" and operation.result_ref
        assert json.loads(artifacts.get_text(operation.result_ref))["evidence_available"] is False
    finally:
        store.close()
