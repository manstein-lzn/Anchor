from __future__ import annotations

import asyncio
from pathlib import Path

from pydantic_ai.messages import ModelResponse, ToolCallPart
from pydantic_ai.models.function import FunctionModel

from anchor.node import COMPLETED, FAILED, NodeRequest
from anchor.node.adapter import run_node


def _result(summary: str, route: str | None = None) -> FunctionModel:
    def answer(messages, info):
        args = {"summary": summary}
        if route is not None:
            args["route"] = route
        return ModelResponse(parts=[ToolCallPart(tool_name="final_result", args=args)])

    return FunctionModel(answer)


def _run(tmp_path: Path, *, routes=(), model=None, control=None):
    workspace = tmp_path / "workspace"
    workspace.mkdir(exist_ok=True)
    return asyncio.run(run_node(
        NodeRequest(execution_id="agent", task="do the work", workspace=workspace,
                    routes=tuple(routes)),
        model=model or _result("finished without tools"), recovery_store=control))


def test_agent_can_complete_without_calling_a_tool(tmp_path):
    outcome = _run(tmp_path)
    assert (outcome.status, outcome.submission, outcome.route) == (
        COMPLETED, "finished without tools", None)


def test_multi_exit_completion_requires_a_legal_route_and_persists_it(tmp_path):
    control = tmp_path / "control"
    outcome = _run(tmp_path, routes=("research", "write"), model=_result("evidence gap", "research"),
                   control=control)

    assert (outcome.status, outcome.route) == (COMPLETED, "research")
    import json
    fact = json.loads((control / "completion.json").read_text())
    assert fact["submission"] == "evidence gap"
    assert fact["route"] == "research"
    assert fact["run"] == "agent-a1"


def test_multi_exit_completion_without_route_cannot_advance(tmp_path):
    outcome = _run(tmp_path, routes=("research", "write"), model=_result("not routed"))
    assert outcome.status == FAILED
    assert "output retries" in outcome.reason
    assert not (tmp_path / "control" / "completion.json").exists()


def test_unknown_route_cannot_advance(tmp_path):
    outcome = _run(tmp_path, routes=("research", "write"), model=_result("wrong", "elsewhere"))
    assert outcome.status == FAILED
    assert "output retries" in outcome.reason
