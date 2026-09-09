"""Agent tool-use loop: model reasoning bound to the ledger-backed ToolGateway.

The model decides *what* to call; the gateway decides *whether* it may run.
Every tool call executes through `ToolGateway.execute` with a deterministic
operation identity derived from `(claim_id, tool_ref, call sequence)`, so a
retried claim replays persisted outcomes instead of re-executing. Denials and
failures return as tool messages — the model sees them, the run does not die.

Evidence shaping is declared by each `ToolCapability`; this loop never knows a
specific tool or domain.
"""

from __future__ import annotations

import asyncio
import json
from typing import Any
from uuid import uuid5

from anchor.runtime.capabilities import CapabilityRegistryError, AgentCapability, ToolCapability
from anchor.runtime.model_gateway import ModelGateway, ModelResponse, ToolFunction
from anchor.runtime.tool_gateway import ToolDenied, ToolGateway
from anchor.runtime.content import ContentUnavailable
from anchor.runtime.sandbox import SandboxDenied
from anchor.runtime.workspace import WorkspaceError

RETRY_CONTEXT_CHAR_BUDGET = 80_000
PRIOR_EVIDENCE_LIMIT = 30


def _truncate(value: Any, *, chars: int | None, list_limit: int, note: str) -> Any:
    """Bound a JSON value for the model while leaving the artifact untouched."""
    if isinstance(value, str):
        if chars is not None and len(value) > chars:
            return value[:chars] + note
        return value
    if isinstance(value, list):
        return [_truncate(item, chars=chars, list_limit=list_limit, note=note)
                for item in value[:list_limit]]
    if isinstance(value, dict):
        return {key: _truncate(item, chars=chars, list_limit=list_limit, note=note)
                for key, item in value.items()}
    return value


class AgentToolLoop:
    """Binds model tool calls to gateway execution for one worker process."""

    def __init__(self, tools: ToolGateway, artifacts, *, native=None) -> None:
        self.tools = tools
        self.artifacts = artifacts
        # Native tools run in the kernel (for example workspace mutations) and
        # keep their own ledger; gateway tools run in a sandbox.
        self.native = native

    def _capability(self, tool_ref: str) -> ToolCapability | None:
        try:
            return self.tools.registry.tool(tool_ref)
        except CapabilityRegistryError:
            return None

    def _shape(self, tool_ref: str, evidence: dict, *, mode: str) -> dict:
        capability = self._capability(tool_ref)
        if capability is None or not getattr(capability, "evidence_json", False):
            return evidence
        if mode == "model":
            chars = getattr(capability, "model_excerpt_chars", None)
            note = " [truncated for model context]"
        else:
            chars = getattr(capability, "retry_excerpt_chars", None)
            note = " [truncated for retry context]"
        limit = getattr(capability, "excerpt_list_limit", 10)
        return _truncate(evidence, chars=chars, list_limit=limit, note=note)

    def _functions(self, lease, agent: AgentCapability, input_snapshot=None) -> list[ToolFunction]:
        state = {"seq": 0, "total": 0, "by_tool": {}}
        state_lock = asyncio.Lock()
        concurrency = asyncio.Semaphore(agent.max_parallel_tools)
        functions: list[ToolFunction] = []
        for tool_ref in agent.tool_refs:
            capability = self._capability(tool_ref)
            description = getattr(capability, "description", "") if capability else ""

            async def call(arguments_json: str, _ref: str = tool_ref,
                           _capability=capability) -> str:
                try:
                    arguments = json.loads(arguments_json)
                except (json.JSONDecodeError, TypeError):
                    return "TOOL DENIED [invalid_arguments]: arguments must be a JSON object"
                if not isinstance(arguments, dict):
                    return "TOOL DENIED [invalid_arguments]: arguments must be a JSON object"
                async with state_lock:
                    tool_count = state["by_tool"].get(_ref, 0)
                    limit = agent.tool_call_limits.get(_ref)
                    if agent.max_tool_calls and state["total"] >= agent.max_tool_calls:
                        return ("TOOL DENIED [tool_budget_exceeded]: total tool-call budget "
                                f"of {agent.max_tool_calls} is exhausted; synthesize from collected evidence")
                    if limit is not None and tool_count >= limit:
                        return (f"TOOL DENIED [tool_budget_exceeded]: {_ref} budget of {limit} "
                                "is exhausted; synthesize from collected evidence")
                    state["seq"] += 1
                    state["total"] += 1
                    state["by_tool"][_ref] = tool_count + 1
                    operation_id = uuid5(lease.claim_id, _ref + ":" + str(state["seq"]))
                if self.native is not None and self.native.handles(_ref):
                    try:
                        async with concurrency:
                            return await asyncio.to_thread(
                                self.native.execute, lease=lease, tool_ref=_ref,
                                arguments=arguments, input_snapshot=input_snapshot)
                    except (WorkspaceError, ContentUnavailable, SandboxDenied) as exc:
                        # A tool failure is a message to the model, never a node
                        # crash: the model may adapt (for example, write a file
                        # before reading it back).
                        return "TOOL FAILED [workspace_error]: " + str(exc)
                try:
                    async with concurrency:
                        result = await asyncio.to_thread(self.tools.execute,
                            lease, agent_ref=agent.ref, tool_ref=_ref,
                            arguments=arguments, operation_id=operation_id)
                except ToolDenied as exc:
                    return "TOOL DENIED [" + exc.code + "]: " + str(exc)
                is_evidence = _capability is not None and getattr(_capability, "evidence_json", False)
                if result.status.value == "succeeded" and result.result_ref:
                    try:
                        text = self.artifacts.get_text(result.result_ref)
                        if is_evidence:
                            evidence = json.loads(text)
                            evidence["evidence_ref"] = result.result_ref
                            evidence["operation_id"] = str(operation_id)
                            return json.dumps(self._shape(_ref, evidence, mode="model"),
                                              ensure_ascii=False)
                        return text
                    except (OSError, ValueError) as exc:
                        return "TOOL FAILED [artifact_unreadable]: " + str(exc)
                if is_evidence and result.result_ref:
                    return "TOOL FAILED: " + self.artifacts.get_text(result.result_ref)
                return "TOOL FAILED [" + str(result.error_code or "unknown") + "]"

            functions.append(ToolFunction(name=tool_ref, description=description, call=call))
        return functions

    def _prior_evidence(self, lease) -> tuple[list[dict], list[dict]]:
        """Durable successful evidence from earlier attempts, for retry context."""
        if not hasattr(self.tools.store, "list_tool_operations"):
            return [], []
        node_ids = {str(node.id): node.node_id
                    for node in self.tools.store.list_node_runs(lease.run_id)}
        successful = [operation for operation in self.tools.store.list_tool_operations(lease.run_id)
                      if (node_ids.get(str(operation.node_run_id)) == lease.node_id
                          and operation.status.value == "succeeded" and operation.result_ref)]
        prior = [{"tool": operation.tool_ref, "arguments": operation.arguments,
                  "evidence_ref": operation.result_ref} for operation in successful][-PRIOR_EVIDENCE_LIMIT:]
        prior_evidence: list[dict] = []
        seen_refs: set[str] = set()
        context_chars = 0
        # Higher-priority evidence (claim-level support) loads before metadata.
        def priority(operation):
            capability = self._capability(operation.tool_ref)
            return -(getattr(capability, "evidence_priority", 0) if capability else 0)
        for operation in sorted(successful, key=priority):
            if operation.result_ref in seen_refs:
                continue
            try:
                evidence = json.loads(self.artifacts.get_text(operation.result_ref))
            except (OSError, ValueError, TypeError):
                continue
            envelope = {
                "tool": operation.tool_ref,
                "arguments": operation.arguments,
                "evidence_ref": operation.result_ref,
                "result": self._shape(operation.tool_ref, evidence, mode="retry"),
            }
            serialized = json.dumps(envelope, ensure_ascii=False)
            if context_chars + len(serialized) > RETRY_CONTEXT_CHAR_BUDGET:
                continue
            prior_evidence.append(envelope)
            seen_refs.add(operation.result_ref)
            context_chars += len(serialized)
        return prior, prior_evidence

    async def run(self, model: ModelGateway, *, lease, agent: AgentCapability,
                  prompt: str, system_prompt: str = "", input_snapshot=None) -> ModelResponse:
        generate = getattr(model, "generate_with_tools", None)
        if generate is None or not agent.tool_refs:
            return await model.generate(prompt=prompt, system_prompt=system_prompt)
        catalog = ", ".join(agent.tool_refs)
        prior, prior_evidence = self._prior_evidence(lease)
        limits = ({"total": agent.max_tool_calls or "unlimited",
                   "parallel": agent.max_parallel_tools,
                   "per_tool": agent.tool_call_limits})
        system = (system_prompt + "\n\nAvailable tools (call each with a JSON object "
                  "of arguments): " + catalog + ". Denials and failures return as "
                  "tool messages; adjust and continue or finish with your answer. "
                  "Tool execution limits: " + json.dumps(limits, ensure_ascii=False) + "."
                  + ("\nDurable successful evidence from earlier attempts is listed below. "
                     "Re-call the exact tool and arguments to load it from the ledger without "
                     "another network request; do not replace it with slightly varied duplicate queries:\n"
                     + json.dumps(prior, ensure_ascii=False) if prior else "")
                  + ("\n\nThis is a retry after an interrupted model response. The complete prior tool "
                     "results below are already loaded and remain authoritative. Do not re-call listed "
                     "arguments. Make a new tool call only for a specific evidence gap; otherwise synthesize "
                     "the requested output directly from this evidence:\n"
                     + json.dumps(prior_evidence, ensure_ascii=False) if prior_evidence else "")).strip()
        return await generate(prompt=prompt, system_prompt=system,
                              tools=self._functions(lease, agent, input_snapshot))
