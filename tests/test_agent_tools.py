"""Agent tool-use loop: real pydantic-ai tool calling, offline via TestModel.

The model loop runs through production `PydanticAIModelGateway` (TestModel
injected) and production `ToolGateway`/`AgentToolLoop` against a real store
lease. TestModel invokes every tool with its default arguments, so the echo
tool runs for real and its ledger-backed artifact flows into the final text.
"""

import asyncio
import json
import threading
import time
from types import SimpleNamespace
from uuid import uuid4

import pytest

from anchor.domain.admission import RunRequest
from anchor.domain.graph import GraphDefinition, GraphNode, GraphVersion, Trigger
from anchor.domain.operations import OperationStatus
from anchor.runtime.agent_tools import AgentToolLoop
from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.capabilities import (
    AgentCapability,
    CapabilityRegistry,
    ModelProfile,
    ToolCapability,
)
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.model_gateway import PydanticAIModelGateway
from anchor.runtime.receiver import DurableExecutionReceiver
from anchor.runtime.secrets import EnvironmentSecretProvider
from anchor.runtime.tool_gateway import SubprocessBackend, ToolGateway
from anchor.runtime.worker import AgentNodeWorker
from conftest import make_store


class StaticSecrets(EnvironmentSecretProvider):
    def get(self, name: str) -> str:
        return "test-secret"


def registry(tool_refs=("echo",)):
    return CapabilityRegistry(
        models=[ModelProfile(ref="models.test", provider="rightcode",
                             model="test-model", secret_ref="TEST_KEY")],
        agents=[AgentCapability(ref="agents.reader", model_ref="models.test",
                                tool_refs=list(tool_refs),
                                instructions="Use tools, then answer.")],
        tools=[ToolCapability(ref="echo", description="emit args",
                              side_effect=False, operation_kind="read")],
    )


def leased(tmp_path, name="tooloop.sqlite"):
    store = make_store(tmp_path, name)
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    definition = GraphDefinition(
        graph_id="tooloop", name="ToolLoop",
        nodes=[GraphNode(id="a", type="agent", name="A", agent_ref="agents.reader")])
    store.publish_graph(GraphVersion.publish(definition, 1))
    version = store.list_graph_versions("tooloop")[0]
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id,
                                           type="manual"))
    receipt = store.admit_run(RunRequest(trigger_id=trigger.id,
                                         idempotency_key=f"loop-{uuid4().hex}",
                                         objective="loop", inputs={}))
    asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
    lease = store.claim_ready_agent_node("worker", uuid4())
    assert lease is not None
    return store, artifacts, receipt, lease


def gateway_for(store, artifacts, refs=("echo",)):
    pydantic_ai = pytest.importorskip("pydantic_ai")
    from pydantic_ai.models.test import TestModel
    model_gateway = PydanticAIModelGateway(
        registry(refs).model("models.test"), StaticSecrets(), model=TestModel())
    tool_gateway = ToolGateway(store, registry(refs), artifacts, SubprocessBackend())
    return model_gateway, tool_gateway


def test_tool_loop_runs_real_tool_and_returns_artifact_text(tmp_path):
    store, artifacts, receipt, lease = leased(tmp_path)
    try:
        model_gateway, tool_gateway = gateway_for(store, artifacts)
        loop = AgentToolLoop(tool_gateway, artifacts)
        agent = registry().agent("agents.reader")
        response = asyncio.run(loop.run(model_gateway, lease=lease, agent=agent,
                                        prompt="say it", system_prompt="Answer."))
        operations = store.list_tool_operations(receipt.run_id)
        assert len(operations) == 1
        assert operations[0].status is OperationStatus.SUCCEEDED
        assert operations[0].tool_ref == "echo"
        body = artifacts.get_text(operations[0].result_ref)
        assert json.loads(response.text) == {"echo": body}
    finally:
        store.close()


def test_tool_loop_replay_does_not_re_execute(tmp_path):
    store, artifacts, receipt, lease = leased(tmp_path)
    try:
        model_gateway, tool_gateway = gateway_for(store, artifacts)
        loop = AgentToolLoop(tool_gateway, artifacts)
        agent = registry().agent("agents.reader")
        calls = {"n": 0}
        original = tool_gateway.backend.run

        def counting(argv, *, timeout_seconds):
            calls["n"] += 1
            return original(argv, timeout_seconds=timeout_seconds)

        tool_gateway.backend.run = counting
        first = asyncio.run(loop.run(model_gateway, lease=lease, agent=agent,
                                     prompt="say it"))
        second = asyncio.run(loop.run(model_gateway, lease=lease, agent=agent,
                                      prompt="say it"))
        assert calls["n"] == 1
        assert first.text == second.text
        assert len(store.list_tool_operations(receipt.run_id)) == 1
    finally:
        store.close()


def test_denied_tool_returns_message_not_failure(tmp_path):
    store, artifacts, receipt, lease = leased(tmp_path)
    try:
        pydantic_ai = pytest.importorskip("pydantic_ai")
        from pydantic_ai.models.test import TestModel
        denied_registry = CapabilityRegistry(
            models=[ModelProfile(ref="models.test", provider="rightcode",
                                 model="test-model", secret_ref="TEST_KEY")],
            agents=[AgentCapability(ref="agents.reader", model_ref="models.test",
                                    tool_refs=["db.write"])],
            tools=[ToolCapability(ref="db.write", description="write rows",
                                  side_effect=True, operation_kind="write")],
        )
        model_gateway = PydanticAIModelGateway(
            denied_registry.model("models.test"), StaticSecrets(), model=TestModel())
        tool_gateway = ToolGateway(store, denied_registry, artifacts, SubprocessBackend())
        loop = AgentToolLoop(tool_gateway, artifacts)
        agent = denied_registry.agent("agents.reader")
        response = asyncio.run(loop.run(model_gateway, lease=lease, agent=agent,
                                        prompt="write it"))
        assert "TOOL DENIED [approval_required]" in response.text
        assert store.list_tool_operations(receipt.run_id) == []
    finally:
        store.close()


def test_tool_budget_denies_calls_before_gateway_execution(tmp_path):
    store, artifacts, _, lease = leased(tmp_path)
    try:
        _, tool_gateway = gateway_for(store, artifacts)
        loop = AgentToolLoop(tool_gateway, artifacts)
        agent = registry().agent("agents.reader").model_copy(update={
            "max_tool_calls": 1, "tool_call_limits": {"echo": 1},
        })
        function = loop._functions(lease, agent)[0]

        async def calls():
            first = await function.call('{"args":["first"]}')
            second = await function.call('{"args":["second"]}')
            return first, second

        first, second = asyncio.run(calls())
        assert first == "first\n"
        assert "tool_budget_exceeded" in second
        assert len(store.list_tool_operations(lease.run_id)) == 1
    finally:
        store.close()


def test_tool_parallelism_is_bounded_before_execution():
    activity = {"current": 0, "peak": 0}
    lock = threading.Lock()

    class Registry:
        def tool(self, ref):
            return SimpleNamespace(description="test")

    class Tools:
        registry = Registry()

        def execute(self, *args, **kwargs):
            with lock:
                activity["current"] += 1
                activity["peak"] = max(activity["peak"], activity["current"])
            time.sleep(0.03)
            with lock:
                activity["current"] -= 1
            return SimpleNamespace(status=OperationStatus.FAILED,
                                   result_ref=None, error_code="test")

    loop = AgentToolLoop(Tools(), SimpleNamespace())
    agent = AgentCapability(ref="agents.reader", model_ref="models.test",
                            tool_refs=["echo"], max_parallel_tools=2)
    function = loop._functions(SimpleNamespace(claim_id=uuid4()), agent)[0]

    async def calls():
        await asyncio.gather(*(function.call('{"args":[]}') for _ in range(6)))

    asyncio.run(calls())
    assert activity["peak"] == 2


def _shaper(capability: ToolCapability) -> AgentToolLoop:
    tools = SimpleNamespace(registry=SimpleNamespace(tool=lambda ref: capability))
    return AgentToolLoop(tools, None)


def test_model_evidence_is_bounded_without_mutating_artifact_contract():
    search = ToolCapability(ref="search", evidence_json=True, model_excerpt_chars=1500,
                            excerpt_list_limit=10)
    evidence = {"papers": [{"abstract": "x" * 2000} for _ in range(12)]}
    result = _shaper(search)._shape("search", evidence, mode="model")
    assert len(result["papers"]) == 10
    assert len(result["papers"][0]["abstract"]) < 1600
    assert len(evidence["papers"]) == 12  # the durable artifact is untouched
    reading = ToolCapability(ref="read", evidence_json=True, model_excerpt_chars=12000)
    bounded = _shaper(reading)._shape("read", {"text": "x" * 24000}, mode="model")
    assert len(bounded["text"]) == 12000 + len(" [truncated for model context]")


def test_retry_evidence_is_more_compact_than_live_tool_context():
    search = ToolCapability(ref="search", evidence_json=True, retry_excerpt_chars=500)
    retry = _shaper(search)._shape(
        "search", {"papers": [{"abstract": "x" * 2000} for _ in range(12)]}, mode="retry")
    assert len(retry["papers"][0]["abstract"]) < 600
    reading = ToolCapability(ref="read", evidence_json=True, retry_excerpt_chars=8000)
    bounded = _shaper(reading)._shape("read", {"text": "x" * 24000}, mode="retry")
    assert len(bounded["text"]) == 8000 + len(" [truncated for retry context]")


def test_non_evidence_tools_are_returned_verbatim():
    plain = ToolCapability(ref="echo")
    evidence = {"text": "x" * 5000}
    assert _shaper(plain)._shape("echo", evidence, mode="model") is evidence


def test_worker_uses_plain_path_without_tools_or_loop(tmp_path):
    from anchor.runtime.model_gateway import ModelResponse

    store, artifacts, receipt, lease = leased(tmp_path)
    try:
        used = {}

        class PlainGateway:
            async def generate(self, *, prompt, system_prompt=""):
                used["plain"] = True
                return ModelResponse(text="plain", provider="p", model="m")

        class Sink:
            async def persist_model_result(self, **kwargs):
                used["sink"] = True

        worker = AgentNodeWorker(store, registry(), {"models.test": PlainGateway()},
                                 Sink())
        outcome = asyncio.run(worker.execute_claimed_once(
            worker_id="worker", agent_ref="agents.reader", prompt="hi", lease=lease,
            expected_node_id="a"))
        assert used.get("plain") and used.get("sink")
        assert outcome.response.text == "plain"
    finally:
        store.close()
