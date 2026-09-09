import asyncio
import json
from pathlib import Path
from uuid import uuid4

import pytest

from anchor.domain.admission import RunRequest
from anchor.domain.conditions import build_condition_context
from anchor.domain.graph import GraphDefinition, GraphVersion, Trigger
from anchor.runtime.academic import (REQUIRED_SECTIONS, VALIDITY_SECTIONS, craft_errors,
                                     register_academic_behaviors, structure_errors,
                                     unsupported_number_claims, validate_agent_output,
                                     validate_manuscript)
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
    registry = CapabilityRegistry(agents=[AgentCapability(ref="gather", model_ref="m", tool_refs=["scholarly.search"])],
                                 tools=[ToolCapability(ref="scholarly.search")])
    gateway = ToolGateway(store, registry, artifacts, SubprocessBackend())
    operation = uuid4()
    first = gateway.execute(lease, agent_ref="gather", tool_ref="scholarly.search", arguments={"query": "test"}, operation_id=operation)
    again = gateway.execute(lease, agent_ref="gather", tool_ref="scholarly.search", arguments={"query": "test"}, operation_id=operation)
    assert first == again and len(calls) == 1
    return {"citation": 1, "id": "doi:10.1234/test", "evidence_ref": first.result_ref}


def gather_output(store, artifacts, lease, monkeypatch):
    """A complete evidence ledger: sources, notes, coverage, tensions, gaps."""
    return {
        "sources": [source_evidence(store, artifacts, lease, monkeypatch)],
        "search_log": [{"query": "compiler cost model", "source": "crossref", "inclusion_decisions": "included"}],
        "evidence_notes": [{"citation": 1, "title": "Real metadata title", "year": 2024,
                            "venue_or_status": "Test Journal", "problem": "predicts cost",
                            "method": "learned model", "key_findings": ["beats the baseline"],
                            "numbers": [], "limitations": ["abstract only"],
                            "evidence_level": "abstract"}],
        "coverage": [{"question": "How did cost models evolve?", "evidence_ids": [1],
                      "answer": "From analytic to learned", "uncertainty": "abstract level"}],
        "tensions": [],
        "unresolved": [],
        "saturation": True,
    }


def settle_coverage(worker):
    """Run the deterministic coverage gate after a gather round."""
    outcome = asyncio.run(worker.execute_once(worker_id="control"))
    assert outcome is not None and outcome.node_id == "coverage", outcome
    return outcome


def ledger(sources=None):
    """A minimal well-formed evidence ledger for tests that do not exercise search."""
    return {"sources": sources or [], "search_log": [], "evidence_notes": [],
            "coverage": [], "tensions": [], "unresolved": []}


THEMES = ("Cost Models by Granularity", "Modeling Approaches", "Target Systems")


def manuscript():
    """The converged survey skeleton: fixed anchors plus a thematic body."""
    paragraph = "Supported analysis [1]. " * 40
    sections = ("Abstract", "Introduction", "Survey Methodology", *THEMES,
                "Comparative Analysis", "Threats to Validity", "Conclusion")
    return "# A literature review\n\n" + "\n\n".join(
        "## " + section + "\n\n" + paragraph for section in sections)


def write_output(text=None, thesis="Cost models moved from analytic to learned"):
    return {"manuscript": text if text is not None else manuscript(), "thesis": thesis}


def test_craft_gate_routes_a_writing_defect_back_to_the_writer(tmp_path, monkeypatch):
    """A deterministic craft defect must route to `write`, not redo the search."""
    store, artifacts, receipt, worker = setup(tmp_path)
    try:
        assert asyncio.run(worker.execute_once(worker_id="control")).node_id == "start"
        complete(store, artifacts, claim(store, "plan"), {"round": 1})
        settle_coverage(worker)  # seed the campaign
        lease = claim(store, "gather")
        evidence = gather_output(store, artifacts, lease, monkeypatch)
        complete(store, artifacts, lease, evidence)
        settle_coverage(worker)

        write_lease = claim(store, "write")
        resolved = resolve_node_context(store, receipt.run_id, "write", artifacts)
        assert [s["id"] for s in resolved.snapshot["evidence"]["sources"]] == \
               [s["id"] for s in evidence["sources"]]
        complete(store, artifacts, write_lease, write_output(manuscript().replace("## Threats to Validity", "## Extra")))

        review_lease = claim(store, "review")
        resolved = resolve_node_context(store, receipt.run_id, "review", artifacts)
        assert resolved.snapshot["manuscript"]["manuscript"].startswith("# A literature review")
        complete(store, artifacts, review_lease, {"verdict": "pass", "target": "none", "issues": []})

        assert asyncio.run(worker.execute_once(worker_id="control")).node_id == "check"
        rewrite_lease = claim(store, "write")
        assert rewrite_lease.node_id == "write", "craft defect must return to the writer"
        feedback = resolve_node_context(store, receipt.run_id, "write", artifacts).snapshot["feedback"]
        assert feedback["review"]["verdict"] == "revise"
        assert feedback["review"]["target"] == "manuscript"
        assert "Missing a validity section" in feedback["review"]["mechanical_issues"][0] or any(
            "Missing a validity section" in issue
            for issue in feedback["review"]["mechanical_issues"])
        assert feedback["evidence"]["sources"] == evidence["sources"]

        complete(store, artifacts, rewrite_lease, write_output())
        complete(store, artifacts, claim(store, "review"), {"verdict": "pass", "target": "none", "issues": []})
        assert asyncio.run(worker.execute_once(worker_id="control")).node_id == "check"
        report = asyncio.run(worker.execute_once(worker_id="control"))
        assert report.node_id == "report"
        assert store.get_run(receipt.run_id).status.value == "completed"
        text = artifacts.get_text(report.output_ref)
        assert text.startswith(manuscript().rstrip())
        assert "## References" in text and "Real metadata title" in text
        assert "Retrieval Evidence" not in text, "provenance must not ship inside the paper"
        assert "artifact://" not in text
        report_dir = artifacts.root / "reports" / str(receipt.run_id)
        assert report_dir.joinpath("report.md").read_text() == text
        provenance = report_dir.joinpath("provenance.md").read_text()
        assert evidence["sources"][0]["evidence_ref"] in provenance
    finally:
        store.close()


def test_evidence_target_routes_back_to_the_gatherer(tmp_path, monkeypatch):
    """A missing-evidence verdict must route to `gather`, not to the writer."""
    store, artifacts, receipt, worker = setup(tmp_path)
    try:
        asyncio.run(worker.execute_once(worker_id="control"))
        complete(store, artifacts, claim(store, "plan"), {"round": 1})
        settle_coverage(worker)  # seed the campaign
        lease = claim(store, "gather")
        complete(store, artifacts, lease, gather_output(store, artifacts, lease, monkeypatch))
        settle_coverage(worker)
        complete(store, artifacts, claim(store, "write"), write_output())
        complete(store, artifacts, claim(store, "review"),
                 {"verdict": "revise", "target": "evidence", "issues": [
                     {"severity": "major", "location": "3.3", "problem": "unsupported claim",
                      "required_action": "find a source that measures it"}]})
        assert asyncio.run(worker.execute_once(worker_id="control")).node_id == "check"
        replan_lease = claim(store, "plan")
        assert replan_lease.node_id == "plan", "evidence defect returns to planning, then research"
        complete(store, artifacts, replan_lease, {"round": 2})
        settle_coverage(worker)
        assert claim(store, "gather").node_id == "gather"
    finally:
        store.close()


def test_blocked_review_parks_for_human_and_approval_resumes_planning(tmp_path, monkeypatch):
    store, artifacts, receipt, worker = setup(tmp_path)
    try:
        asyncio.run(worker.execute_once(worker_id="control"))
        complete(store, artifacts, claim(store, "plan"), {"round": 1})
        settle_coverage(worker)  # seed the campaign
        lease = claim(store, "gather")
        complete(store, artifacts, lease, gather_output(store, artifacts, lease, monkeypatch))
        settle_coverage(worker)
        complete(store, artifacts, claim(store, "write"), write_output())
        complete(store, artifacts, claim(store, "review"),
                 {"verdict": "blocked", "target": "none", "blockers": ["Access unavailable"]})
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
    errors, verified, target = validate_manuscript({"manuscript": manuscript(),
        "sources": [{"citation": 1, "id": "fake", "evidence_ref": ref}]},
        operations=[], artifacts=artifacts, minimum_sources=1, minimum_reads=0)
    assert not verified
    assert target == "evidence"
    assert any("successful scholarly search" in error for error in errors)


def test_audit_language_in_the_body_is_a_defect_not_a_virtue():
    """Verification is a pipeline property, so narrating it is a writing defect."""
    assert not craft_errors(manuscript())
    for defect, expected in (
        ("正文提到（全文级）证据", "evidence-level tags"),
        ("如本轮所读", "process language"),
        ("证据见 artifact://sha256/abc", "evidence hashes"),
    ):
        errors = craft_errors(manuscript() + "\n\n" + defect)
        assert any(expected in error for error in errors), (defect, errors)
    wall = "# Title\n\n## Abstract\n\n" + ("x" * 1300)
    assert any("Paragraphs must stay under" in error for error in craft_errors(wall))
    long_methods = ("# Title\n\n## Survey Methodology\n\n" + ("search " * 500)
                    + "\n\n## Conclusion\n\nok")
    assert any("Survey Methodology must stay under" in error for error in craft_errors(long_methods))


def test_structure_follows_the_converged_survey_skeleton():
    """The body must be thematic, not one catch-all 'Literature Review' bucket."""
    assert not structure_errors(manuscript())
    catch_all = ("# Title\n\n## Abstract\n\nok\n\n## Introduction\n\nok\n\n"
                 "## Survey Methodology\n\nok\n\n## Literature Review\n\n" + ("x " * 200)
                 + "\n\n## Threats to Validity\n\nok\n\n## Conclusion\n\nok")
    errors = structure_errors(catch_all)
    assert any("catch-all" in error for error in errors), errors
    assert any("thematic sections" in error for error in errors), errors
    missing_validity = manuscript().replace("## Threats to Validity", "## Extra")
    assert any("validity section" in error for error in structure_errors(missing_validity))
    misordered = (manuscript().replace("## Threats to Validity", "## TEMP")
                  .replace("## Conclusion", "## Threats to Validity")
                  .replace("## TEMP", "## Conclusion"))
    assert any("precede the Conclusion" in error for error in structure_errors(misordered))
    for section in REQUIRED_SECTIONS:
        assert section in manuscript()
    assert VALIDITY_SECTIONS[0] in manuscript()


def test_coverage_gate_continues_then_stops_on_convergence(tmp_path, monkeypatch):
    """No round cap: research continues while rounds add evidence, and ends when a
    round adds nothing. Convergence is not a budget."""
    store, artifacts, receipt, worker = setup(tmp_path)
    try:
        asyncio.run(worker.execute_once(worker_id="control"))
        complete(store, artifacts, claim(store, "plan"), {"round": 1})
        settle_coverage(worker)  # seed the campaign
        lease = claim(store, "gather")
        first = gather_output(store, artifacts, lease, monkeypatch)
        first["saturation"] = False
        complete(store, artifacts, lease, first)
        output = json.loads(artifacts.get_text(settle_coverage(worker).output_ref))
        assert output["decision"] == "continue" and output["round"] == 1 and output["added"] == 1

        repeat = dict(first, saturation=False)
        complete(store, artifacts, claim(store, "gather"), repeat)
        output = json.loads(artifacts.get_text(settle_coverage(worker).output_ref))
        assert output["decision"] == "write", "a round that adds nothing ends research"
        assert output["round"] == 2 and output["added"] == 0
        assert len(output["ledger"]["sources"]) == len(first["sources"])
    finally:
        store.close()


def test_ledger_merge_renumbers_citations_across_rounds():
    from anchor.runtime.academic_rounds import merge_ledger
    first = {"sources": [{"id": "x", "citation": 1}, {"id": "y", "citation": 2}],
             "evidence_notes": [{"citation": 2, "title": "Y"}]}
    second = {"sources": [{"id": "y", "citation": 1}, {"id": "z", "citation": 2}],
              "evidence_notes": [{"citation": 2, "title": "Z"}]}
    merged = merge_ledger(first, second)
    assert [source["id"] for source in merged["sources"]] == ["x", "y", "z"]
    assert [source["citation"] for source in merged["sources"]] == [1, 2, 3]
    assert [(note["citation"], note["title"]) for note in merged["evidence_notes"]] == [(2, "Y"), (3, "Z")]


def test_result_numbers_must_rest_on_a_full_text_reading():
    """An abstract reports a number without the detail that makes it checkable."""
    claim = "# T\n\n## Abstract\n\nThe method is 1.85x faster [1].\n"
    assert unsupported_number_claims(claim, set())
    assert not unsupported_number_claims(claim, {1})
    assert not unsupported_number_claims("# T\n\n## Abstract\n\nThe field grew after 2018 [1].", set())
    assert not unsupported_number_claims("# T\n\n## Abstract\n\nThe method is 1.85x faster.", set())


def test_abstract_page_reads_are_refused():
    """An arXiv /abs/ page is the abstract the search already returned."""
    from anchor.runtime.research_tools import ResearchRequest, ResearchToolError, read
    with pytest.raises(ResearchToolError, match="abstract page"):
        read(ResearchRequest(url="https://arxiv.org/abs/2104.04955v1"), timeout_seconds=5)


def test_chinese_headings_satisfy_the_skeleton():
    """A Chinese paper must pass: the check judges structure, not language."""
    chinese = ("# 标题\n\n## 摘要\n\n摘要内容 [1]。\n\n## 引言\n\n引言内容 [1]。\n\n"
               "## 综述方法\n\n检索方法 [1]。\n\n## 主题一\n\n内容 [1]。\n\n"
               "## 主题二\n\n内容 [1]。\n\n## 有效性威胁\n\n威胁 [1]。\n\n## 结论\n\n结论 [1]。")
    assert not structure_errors(chinese)
    english = chinese.replace("## 摘要", "## Abstract（摘要）").replace("## 引言", "## Introduction（引言）")
    assert not structure_errors(english)


def test_target_separates_evidence_gaps_from_writing_defects(tmp_path):
    artifacts = LocalArtifactStore(tmp_path)
    ref = artifacts.put_text(json.dumps({"papers": [{"id": "fake"}]}))
    _, _, evidence_target = validate_manuscript(
        {"manuscript": manuscript(), "sources": [{"citation": 1, "id": "fake", "evidence_ref": ref}]},
        operations=[], artifacts=artifacts, minimum_sources=1, minimum_reads=0)
    assert evidence_target == "evidence"
    _, _, craft_target = validate_manuscript(
        {"manuscript": manuscript().replace("## Threats to Validity", "## Extra"), "sources": []},
        operations=[], artifacts=artifacts, minimum_sources=0, minimum_reads=0)
    assert craft_target in ("evidence", "manuscript")


def test_role_output_schemas_reject_a_missing_paper_or_thesis():
    with pytest.raises(ValueError):
        validate_agent_output(json.dumps({"sources": [], "search_log": [], "evidence_notes": [],
                                          "coverage": [], "tensions": [], "unresolved": []}), "writer")
    validate_agent_output(json.dumps({"manuscript": "# T", "thesis": "t"}), "writer")
    validate_agent_output(json.dumps({"sources": [], "search_log": [], "evidence_notes": [],
                                      "coverage": [], "tensions": [], "unresolved": []}), "gatherer")
    review = validate_agent_output(json.dumps({"verdict": "revise", "target": "evidence",
                                               "summary": "s", "issues": [], "strengths": [],
                                               "blockers": []}), "reviewer")
    assert review["target"] == "evidence"


def test_identical_completed_revision_cycles_surface_to_supervisor_and_continue(tmp_path, monkeypatch):
    store, artifacts, receipt, worker = setup(tmp_path)
    try:
        asyncio.run(worker.execute_once(worker_id="control"))
        complete(store, artifacts, claim(store, "plan"), {"round": 1})
        settle_coverage(worker)  # seed the campaign
        lease = claim(store, "gather")
        complete(store, artifacts, lease, gather_output(store, artifacts, lease, monkeypatch))
        settle_coverage(worker)
        for _ in range(2):
            complete(store, artifacts, claim(store, "write"), write_output())
            complete(store, artifacts, claim(store, "review"),
                     {"verdict": "revise", "target": "manuscript", "issues": []})
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
        settle_coverage(worker)  # seed the campaign
        lease = claim(store, "gather")
        agent = AgentCapability(ref="gather", model_ref="m", tool_refs=["scholarly.search"])
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
