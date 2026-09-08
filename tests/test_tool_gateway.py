"""ToolGateway v1: policy denials use the real ledger; isolation uses real bwrap.

Backend tests assert actual sandbox behavior (read-only root, no network,
private workspace). Gateway tests run tools end to end through the operation
ledger with store-level leases only — no worker wiring yet by design.
"""

import asyncio
import json
import shutil
from uuid import uuid4

import pytest

from anchor.domain.admission import RunRequest
from anchor.domain.graph import GraphDefinition, GraphNode, GraphVersion, Trigger
from anchor.domain.operations import OperationStatus
from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.capabilities import AgentCapability, CapabilityRegistry, ToolCapability
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.receiver import DurableExecutionReceiver
from anchor.runtime.tool_gateway import (
    BubblewrapBackend,
    SubprocessBackend,
    ToolDenied,
    ToolGateway,
)
from conftest import make_store

bwrap_only = pytest.mark.skipif(shutil.which("bwrap") is None, reason="bwrap not installed")


def registry():
    return CapabilityRegistry(
        models=[],
        agents=[AgentCapability(ref="agents.reader", model_ref="models.test",
                                tool_refs=["echo", "fs.read"]),
                AgentCapability(ref="agents.noscope", model_ref="models.test",
                                tool_refs=[]),
                AgentCapability(ref="agents.writer", model_ref="models.test",
                                tool_refs=["db.write"])],
        tools=[ToolCapability(ref="echo", description="emit args",
                              side_effect=False, operation_kind="read"),
               ToolCapability(ref="fs.read", description="read a file",
                              side_effect=False, operation_kind="read"),
               ToolCapability(ref="db.write", description="write rows",
                              side_effect=True, operation_kind="write")],
    )


def leased(tmp_path, name="tools.sqlite"):
    store = make_store(tmp_path, name)
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    definition = GraphDefinition(
        graph_id="tools", name="Tools",
        nodes=[GraphNode(id="a", type="agent", name="A", agent_ref="agents.reader")])
    version = store.publish_graph(GraphVersion.publish(definition, 1))
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id,
                                           type="manual"))
    receipt = store.admit_run(RunRequest(trigger_id=trigger.id,
                                         idempotency_key=f"tools-{uuid4().hex}",
                                         objective="tools", inputs={}))
    asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
    lease = store.claim_ready_agent_node("worker", uuid4())
    assert lease is not None
    return store, artifacts, receipt, lease


def gateway(store, artifacts, backend=None):
    return ToolGateway(store, registry(), artifacts,
                       backend or SubprocessBackend())


def test_echo_succeeds_and_result_is_ferried_as_artifact(tmp_path):
    store, artifacts, receipt, lease = leased(tmp_path)
    try:
        gw = gateway(store, artifacts)
        result = gw.execute(lease, agent_ref="agents.reader", tool_ref="echo",
                            arguments={"args": ["hello", "world"]}, operation_id=uuid4())
        assert result.status is OperationStatus.SUCCEEDED
        assert result.result_ref is not None
        assert artifacts.get_text(result.result_ref) == "hello world\n"
        operations = store.list_tool_operations(receipt.run_id)
        assert len(operations) == 1
        assert operations[0].status is OperationStatus.SUCCEEDED
        assert operations[0].result_ref == result.result_ref
    finally:
        store.close()


def test_replay_with_same_operation_id_does_not_rerun(tmp_path):
    store, artifacts, _, lease = leased(tmp_path)
    try:
        calls = []

        class Counting(SubprocessBackend):
            def run(self, argv, *, timeout_seconds):
                calls.append(argv)
                return super().run(argv, timeout_seconds=timeout_seconds)

        gw = ToolGateway(store, registry(), artifacts, Counting())
        operation_id = uuid4()
        first = gw.execute(lease, agent_ref="agents.reader", tool_ref="echo",
                           arguments={"args": ["once"]}, operation_id=operation_id)
        second = gw.execute(lease, agent_ref="agents.reader", tool_ref="echo",
                            arguments={"args": ["once"]}, operation_id=operation_id)
        assert first == second
        assert len(calls) == 1
    finally:
        store.close()


def test_unknown_tool_out_of_scope_and_side_effect_are_denied(tmp_path):
    store, artifacts, _, lease = leased(tmp_path)
    try:
        gw = gateway(store, artifacts)
        with pytest.raises(ToolDenied, match="unknown_tool"):
            gw.execute(lease, agent_ref="agents.reader", tool_ref="nope",
                       arguments={}, operation_id=uuid4())
        with pytest.raises(ToolDenied, match="out_of_scope"):
            gw.execute(lease, agent_ref="agents.noscope", tool_ref="echo",
                       arguments={"args": ["x"]}, operation_id=uuid4())
        with pytest.raises(ToolDenied, match="approval_required"):
            gw.execute(lease, agent_ref="agents.writer", tool_ref="db.write",
                       arguments={}, operation_id=uuid4())
        assert store.list_tool_operations(lease.run_id) == []
    finally:
        store.close()


def test_credentials_never_enter_arguments(tmp_path):
    store, artifacts, _, lease = leased(tmp_path)
    try:
        gw = gateway(store, artifacts)
        with pytest.raises(ToolDenied, match="credential_in_arguments"):
            gw.execute(lease, agent_ref="agents.reader", tool_ref="echo",
                       arguments={"args": ["x"], "api_token": "sekret"},
                       operation_id=uuid4())
    finally:
        store.close()


def test_fs_read_returns_bounded_content(tmp_path):
    store, artifacts, _, lease = leased(tmp_path)
    try:
        target = tmp_path / "note.txt"
        target.write_text("evidence-line")
        gw = gateway(store, artifacts)
        result = gw.execute(lease, agent_ref="agents.reader", tool_ref="fs.read",
                            arguments={"path": str(target)}, operation_id=uuid4())
        assert result.status is OperationStatus.SUCCEEDED
        assert artifacts.get_text(result.result_ref) == "evidence-line"
        with pytest.raises(ToolDenied, match="invalid_path"):
            gw.execute(lease, agent_ref="agents.reader", tool_ref="fs.read",
                       arguments={"path": "relative/nope"}, operation_id=uuid4())
    finally:
        store.close()


def test_timeout_fails_closed_without_success(tmp_path):
    from anchor.runtime.tool_gateway import SandboxResult
    store, artifacts, _, lease = leased(tmp_path)
    try:
        class Hanging(SubprocessBackend):
            def run(self, argv, *, timeout_seconds):
                return SandboxResult(returncode=124, stdout=b"", stderr=b"",
                                     timed_out=True)

        gw = ToolGateway(store, registry(), artifacts, Hanging())
        result = gw.execute(lease, agent_ref="agents.reader", tool_ref="echo",
                            arguments={"args": ["x"]}, operation_id=uuid4())
        assert result.status is OperationStatus.FAILED
        assert result.error_code == "timeout"
        assert result.result_ref is None
        assert store.list_tool_operations(lease.run_id)[0].status is OperationStatus.FAILED
    finally:
        store.close()


def test_research_failure_keeps_structured_error_code(tmp_path, monkeypatch):
    from anchor.runtime import tool_gateway as module
    from anchor.runtime.research_tools import ResearchToolError

    store, artifacts, _, lease = leased(tmp_path)
    try:
        scholarly_registry = CapabilityRegistry(
            agents=[AgentCapability(ref="agents.reader", model_ref="models.test",
                                    tool_refs=["scholarly.search"])],
            tools=[ToolCapability(ref="scholarly.search", side_effect=False,
                                  idempotent=True, operation_kind="read")],
        )
        monkeypatch.setattr(module, "execute_research", lambda *a, **kw: (_ for _ in ()).throw(
            ResearchToolError("source_rate_limited", "HTTP 429", retryable=True)))
        gw = ToolGateway(store, scholarly_registry, artifacts, SubprocessBackend())
        result = gw.execute(lease, agent_ref="agents.reader", tool_ref="scholarly.search",
                            arguments={"query": "test"}, operation_id=uuid4())
        assert result.status is OperationStatus.FAILED
        assert result.error_code == "source_rate_limited"
        evidence = json.loads(artifacts.get_text(result.result_ref))
        assert evidence == {"error": "HTTP 429", "error_code": "source_rate_limited",
                            "retryable": True, "tool": "scholarly.search",
                            "evidence_available": False}
    finally:
        store.close()


def test_real_sleep_timeout_is_killed(tmp_path):
    backend = SubprocessBackend()
    result = backend.run(["/bin/sleep", "30"], timeout_seconds=1)
    assert result.timed_out and result.returncode == 124


@bwrap_only
def test_bwrap_root_is_read_only_but_workspace_is_writable():
    backend = BubblewrapBackend()
    denied = backend.run(["/bin/sh", "-c", "echo pwn > /pwned"], timeout_seconds=10)
    assert denied.returncode != 0
    ok = backend.run(["/bin/sh", "-c", "echo hi > /tmp/out.txt && cat /tmp/out.txt"],
                     timeout_seconds=10)
    assert ok.returncode == 0 and ok.stdout == b"hi\n"


@bwrap_only
def test_bwrap_scrubs_parent_environment(monkeypatch):
    monkeypatch.setenv("ANCHOR_SANDBOX_TEST_SECRET", "must-not-be-inherited")
    result = BubblewrapBackend().run(["/usr/bin/env"], timeout_seconds=10)
    assert result.returncode == 0
    environment = dict(line.split("=", 1) for line in result.stdout.decode().splitlines())
    assert "ANCHOR_SANDBOX_TEST_SECRET" not in environment
    assert environment["PATH"] == "/usr/bin:/bin"
    assert environment["HOME"] == "/tmp"
    assert environment["TMPDIR"] == "/tmp"


@bwrap_only
def test_bwrap_has_no_network():
    backend = BubblewrapBackend()
    result = backend.run(
        ["/usr/bin/python3", "-c",
         "import socket; socket.create_connection(('127.0.0.1', 9), timeout=3)"],
        timeout_seconds=15)
    assert result.returncode != 0


@bwrap_only
def test_bwrap_echo_end_to_end_through_gateway(tmp_path):
    store, artifacts, _, lease = leased(tmp_path)
    try:
        gw = ToolGateway(store, registry(), artifacts, BubblewrapBackend())
        result = gw.execute(lease, agent_ref="agents.reader", tool_ref="echo",
                            arguments={"args": ["sandboxed"]}, operation_id=uuid4())
        assert result.status is OperationStatus.SUCCEEDED
        assert artifacts.get_text(result.result_ref) == "sandboxed\n"
    finally:
        store.close()


def test_missing_bwrap_binary_fails_fast():
    with pytest.raises(RuntimeError, match="sandbox binary not found"):
        BubblewrapBackend(binary="definitely-not-a-sandbox")
