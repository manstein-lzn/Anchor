"""PydanticAI AgentNode runtime.

Bash is an ordinary workspace tool. A structured ``AgentCompletion`` is the only fact
that completes an AgentNode and is persisted before control returns to the scheduler.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable

from pydantic import BaseModel, Field
from pydantic_ai import Agent, ModelRetry, RunContext
from pydantic_ai.models.wrapper import WrapperModel

from anchor.runtime.execenv import Executed, NodeSandbox

RULES = """\
Work in your own workspace. Use the bash tool when you need to inspect or change it, then return a
structured completion when this pass is complete. A text explanation alone is not a graph completion:
the final result must contain `summary` and, when this node has several exits, exactly one `route`.

Inputs are read-only. Your workspace is kept between passes and is the only thing downstream nodes see,
so deliverables that must be passed onward belong there. Completion is separate from tool use: a node
may finish without calling bash, and calling bash does not finish the node.
"""


class AgentCompletion(BaseModel):
    """The only model output that can advance an AgentNode."""

    summary: str = Field(min_length=1, description="What this pass accomplished and what remains uncertain")
    route: str | None = Field(default=None, description="One legal next node, when a choice is required")


class _CountingModel(WrapperModel):
    requests: int = 0
    control: Any = None
    allowed: int | None = None
    cancelled: Callable[[], bool] | None = None

    async def request(self, messages: Any, model_settings: Any, model_request_parameters: Any) -> Any:
        # A killed command comes back as an ordinary failure, so the loop would ask the provider again
        # and retry its way to a different verdict. Asking after the operator stopped is a request
        # nobody wants and nobody is paying for; cancelling here ends the pass instead.
        if self.cancelled is not None and self.cancelled():
            raise asyncio.CancelledError()
        self.requests += 1
        if self.control is not None:
            from anchor.node.recovery import charge_request
            charge_request(Path(str(self.control)), self.allowed)
        return await super().request(messages, model_settings, model_request_parameters)


@dataclass
class _Done:
    submission: str
    route: str | None


@dataclass
class _Wiring:
    sandbox: NodeSandbox
    routes: tuple[str, ...]
    done: _Done | None = None
    control: Any = None
    execution_id: str = ""
    framework_run: str = ""
    commands: int = 0
    ran: list[tuple] | None = None

    def __post_init__(self) -> None:
        if self.ran is None:
            self.ran = []


def _first_line(output: str) -> str:
    return next((line.strip() for line in output.lstrip().splitlines() if line.strip()), "")


def _observation(ran: Executed) -> str:
    parts = [f"<returncode>{ran.returncode}</returncode>", "<output>",
             ran.output.rstrip("\n"), "</output>"]
    if ran.timed_out:
        parts.insert(1, "<timeout>the command was stopped for running too long</timeout>")
    return "\n".join(parts)


def _bash(ctx: RunContext[_Wiring], command: str) -> str:
    """Run a normal shell command in the node sandbox."""
    wiring = ctx.deps
    ran = wiring.sandbox.run(command)
    wiring.commands += 1
    wiring.ran.append((command, ran.returncode, _first_line(ran.output), ran.output))
    return _observation(ran)


def _record_completion(wiring: _Wiring, value: AgentCompletion) -> None:
    from anchor.node.recovery import CompletionFact, record_completion

    kind = "routed" if value.route is not None else "done"
    wiring.done = _Done(submission=value.summary, route=value.route)
    if wiring.control is None:
        return
    record_completion(Path(wiring.control), CompletionFact(
        node=wiring.execution_id, run=wiring.framework_run or "", kind=kind,
        submission=value.summary, route=value.route, command="",
        at=datetime.now(timezone.utc).isoformat()))


def build_agent(model: Any, *, instructions: str = "", max_retries: int = 60,
                capabilities: tuple[Any, ...] = ()) -> Agent[_Wiring, AgentCompletion]:
    agent: Agent[_Wiring, AgentCompletion] = Agent(
        model,
        capabilities=list(capabilities),
        deps_type=_Wiring,
        output_type=AgentCompletion,
        retries=max_retries,
        instructions=f"{instructions.strip()}\n\n{RULES}".strip(),
    )
    agent.tool(name="bash", description="Execute a bash command", sequential=True)(_bash)

    @agent.output_validator
    def validate_completion(ctx: RunContext[_Wiring], value: AgentCompletion) -> AgentCompletion:
        routes = ctx.deps.routes
        if len(routes) > 1 and value.route is None:
            raise ModelRetry(f"choose exactly one route: {', '.join(routes)}")
        if value.route is not None and value.route not in routes:
            raise ModelRetry(f"{value.route!r} is not a legal route; choose one of: {', '.join(routes)}")
        _record_completion(ctx.deps, value)
        return value

    return agent
