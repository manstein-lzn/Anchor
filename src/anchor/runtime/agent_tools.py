"""Agent tool-use loop: model reasoning bound to the ledger-backed ToolGateway.

The model decides *what* to call; the gateway decides *whether* it may run.
Every tool call executes through `ToolGateway.execute` with a deterministic
operation identity derived from `(claim_id, tool_ref, call sequence)`, so a
retried claim replays persisted outcomes instead of re-executing. Denials and
failures return as tool messages — the model sees them, the run does not die.
"""

from __future__ import annotations

import json
from uuid import uuid5

from anchor.runtime.capabilities import AgentCapability
from anchor.runtime.model_gateway import ModelGateway, ModelResponse, ToolFunction
from anchor.runtime.tool_gateway import ToolDenied, ToolGateway


class AgentToolLoop:
    """Binds model tool calls to gateway execution for one worker process."""

    def __init__(self, tools: ToolGateway, artifacts) -> None:
        self.tools = tools
        self.artifacts = artifacts

    def _functions(self, lease, agent: AgentCapability) -> list[ToolFunction]:
        state = {"seq": 0}
        functions: list[ToolFunction] = []
        for tool_ref in agent.tool_refs:
            try:
                description = self.tools.registry.tool(tool_ref).description
            except Exception:
                description = ""

            async def call(arguments_json: str, _ref: str = tool_ref) -> str:
                try:
                    arguments = json.loads(arguments_json)
                except (json.JSONDecodeError, TypeError):
                    return "TOOL DENIED [invalid_arguments]: arguments must be a JSON object"
                if not isinstance(arguments, dict):
                    return "TOOL DENIED [invalid_arguments]: arguments must be a JSON object"
                state["seq"] += 1
                operation_id = uuid5(lease.claim_id, _ref + ":" + str(state["seq"]))
                try:
                    result = self.tools.execute(
                        lease, agent_ref=agent.ref, tool_ref=_ref,
                        arguments=arguments, operation_id=operation_id)
                except ToolDenied as exc:
                    return "TOOL DENIED [" + exc.code + "]: " + str(exc)
                if result.status.value == "succeeded" and result.result_ref:
                    try:
                        return self.artifacts.get_text(result.result_ref)
                    except (OSError, ValueError) as exc:
                        return "TOOL FAILED [artifact_unreadable]: " + str(exc)
                return "TOOL FAILED [" + str(result.error_code or "unknown") + "]"

            functions.append(ToolFunction(name=tool_ref, description=description,
                                          call=call))
        return functions

    async def run(self, model: ModelGateway, *, lease, agent: AgentCapability,
                  prompt: str, system_prompt: str = "") -> ModelResponse:
        generate = getattr(model, "generate_with_tools", None)
        if generate is None or not agent.tool_refs:
            return await model.generate(prompt=prompt, system_prompt=system_prompt)
        catalog = ", ".join(agent.tool_refs)
        system = (system_prompt + "\n\nAvailable tools (call each with a JSON object "
                  "of arguments): " + catalog + ". Denials and failures return as "
                  "tool messages; adjust and continue or finish with your answer.").strip()
        return await generate(prompt=prompt, system_prompt=system,
                              tools=self._functions(lease, agent))
