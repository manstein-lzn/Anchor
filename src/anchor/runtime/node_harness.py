"""Run one node of a finished run again, cheaply enough to iterate on.

A campaign costs half an hour and a couple of yuan, and it is not deterministic, so
a context policy cannot be judged from it: change the policy, run a campaign, and
the outcome moves for reasons nobody can separate. The unit that can actually be
compared is one node attempt against the input it really received.

This module is that unit. It reads an attempt's *own* persisted context snapshot —
the declared input as it was, not a re-derivation from current state — assembles the
prompt with the same function the worker uses, executes it once, and reports what
came back and what it cost. It never writes to the run it read, so an experiment
cannot damage the evidence it is studying.

Three things it deliberately does not pretend to do:

- **No tools.** A tool-using node would need a ledger to make its calls auditable,
  which means a scratch run, which is a design decision of its own. Asking for one
  raises rather than quietly executing something unaudited.
- **No replay.** This exists to ask the model a question the recording cannot answer,
  because a changed prompt has to be answered for real (ADR-044). A harness run is
  a live call and costs what a live call costs.
- **No verdict.** It reports; it does not decide whether an output is better. That is
  the domain's job and stays where it already is — in the review gate.
"""

from __future__ import annotations

import time
from dataclasses import dataclass
from typing import Any
from uuid import UUID

from anchor.runtime.artifacts import ArtifactStore
from anchor.runtime.model_gateway import ModelGateway
from anchor.runtime.model_recording import CallContext
from anchor.runtime.model_replay import ReplayPlan
from anchor.runtime.node_prompt import PromptParts, assemble_prompt


def recorded_user_prompt(payload: dict[str, Any]) -> str | None:
    """The user prompt the model actually received, read back from a recording.

    This is what a fidelity check has to compare against: the assembled prompt is
    the *user* text of the request, not the whole rendered transcript.
    """
    for message in reversed(payload.get("messages") or []):
        for part in reversed(message.get("parts") or []):
            if part.get("part_kind") == "user-prompt":
                content = part.get("content")
                if isinstance(content, str):
                    return content
    return None


def _first_difference(left: str, right: str) -> dict[str, Any]:
    """Where two texts first diverge, so a mismatch is locatable."""
    limit = min(len(left), len(right))
    index = next((i for i in range(limit) if left[i] != right[i]), limit)
    return {"index": index, "recorded": left[index:index + 60],
            "reassembled": right[index:index + 60]}


class HarnessUnsupported(RuntimeError):
    """The harness was asked for something it must not fake.

    Raised rather than approximated: a harness that silently measured the wrong
    thing would be worse than no harness, because its numbers would be believed.
    """


#: The request came from a recording of the call itself, so it is what the model
#: was actually given.
PROVENANCE_RECORDED = "recording"
#: The request was rebuilt from the persisted input snapshot plus *today's*
#: capability configuration. Faithful to the inputs, not provably to the request.
PROVENANCE_RECONSTRUCTED = "reconstructed-from-current-configuration"


@dataclass(frozen=True)
class FrozenAttempt:
    """A node attempt as durable state recorded it.

    ``provenance`` says how much the request can be trusted. A run made after model
    call recording was introduced has one: its instructions, tools and messages are
    the ones the model received. A run made before it, or one whose call was not
    recorded, is *reconstructed* from the frozen snapshot and the current
    configuration — which is not the same claim, and a caller that does not know the
    difference would report the second as the first. See ``fidelity``.
    """

    run_id: UUID
    node_id: str
    node_run_id: UUID
    attempt: int
    objective: str
    node_name: str
    agent_ref: str
    instructions: str
    snapshot: dict[str, Any]
    tools: tuple[str, ...]
    provenance: str = PROVENANCE_RECONSTRUCTED
    model: str | None = None


@dataclass(frozen=True)
class NodeRunResult:
    """What one harness execution produced."""

    node_id: str
    attempt: int
    prompt: str
    text: str
    model: str
    provider: str
    input_tokens: int
    output_tokens: int
    cached_tokens: int
    cost: float | None
    seconds: float

    def summary(self) -> dict[str, Any]:
        """The shape a comparison reads: identity, size and spend, not the text."""
        return {"node_id": self.node_id, "attempt": self.attempt,
                "model": self.model, "prompt_chars": len(self.prompt),
                "output_chars": len(self.text), "input_tokens": self.input_tokens,
                "cached_tokens": self.cached_tokens,
                "output_tokens": self.output_tokens, "cost": self.cost,
                "seconds": round(self.seconds, 2)}


class NodeHarness:
    """Re-run one node attempt against its own recorded input."""

    def __init__(self, store: Any, artifacts: ArtifactStore, *,
                 gateway: ModelGateway, registry: Any, memory: Any | None = None) -> None:
        self.store = store
        self.artifacts = artifacts
        self.gateway = gateway
        self.registry = registry
        self.memory = memory

    # -- reading the frozen input ------------------------------------------

    def attempts(self, run_id: UUID, node_id: str) -> list[Any]:
        """Every attempt this run made at a node, oldest first."""
        return sorted((item for item in self.store.list_node_runs(run_id)
                       if item.node_id == node_id), key=lambda item: item.attempt)

    def frozen_attempt(self, run_id: UUID, node_id: str,
                       attempt: int | None = None) -> FrozenAttempt:
        """Read the attempt's own persisted input.

        The snapshot is read from ``context_snapshots`` rather than rebuilt from
        current state, because a run that advanced past this attempt would otherwise
        hand back what the node would see *now* — a different question, answered
        convincingly.
        """
        run = self.store.get_run(run_id)
        if run is None:
            raise KeyError(f"unknown run {run_id}")
        task = self.store.get_task(run.task_id)
        graph = self.store.get_graph_version(run.graph_version_id)
        if task is None or graph is None:
            raise KeyError(f"run {run_id} has no task or graph version")
        node = next((item for item in graph.definition.nodes if item.id == node_id), None)
        if node is None:
            raise KeyError(f"run {run_id} has no node {node_id!r}")

        rows = self.attempts(run_id, node_id)
        if not rows:
            raise KeyError(f"node {node_id!r} was never attempted in run {run_id}")
        row = rows[-1] if attempt is None else next(
            (item for item in rows if item.attempt == attempt), None)
        if row is None:
            raise KeyError(f"node {node_id!r} has no attempt {attempt} in run {run_id}")

        stored = self.store.get_context_snapshot(row.id)
        if stored is None:
            raise HarnessUnsupported(
                f"node {node_id!r} attempt {row.attempt} has no persisted context "
                f"snapshot, so there is no frozen input to run against")
        if not node.agent_ref:
            raise HarnessUnsupported(
                f"node {node_id!r} is not an agent node; the harness runs agents only")

        context = CallContext(run_id=run_id, node_id=node_id, node_run_id=row.id,
                              attempt=row.attempt)
        recorded = self.recording(context)
        if recorded is not None:
            # A recording is the request the model actually received. Prefer it, and
            # say so, rather than rebuilding one that merely resembles it.
            instructions = "\n".join(recorded.get("instructions") or [])
            tools = tuple(recorded.get("tools") or [])
            provenance = PROVENANCE_RECORDED
            model = recorded.get("model")
        else:
            agent = self._agent(node.agent_ref)
            instructions, tools = agent.instructions, tuple(agent.tool_refs)
            provenance = PROVENANCE_RECONSTRUCTED
            model = None
        return FrozenAttempt(
            run_id=run_id, node_id=node_id, node_run_id=row.id, attempt=row.attempt,
            objective=task.objective, node_name=node.name, agent_ref=node.agent_ref,
            instructions=instructions, snapshot=dict(stored.snapshot),
            tools=tools, provenance=provenance, model=model)

    def recording(self, context: CallContext,
                  sequence: int = 0) -> dict[str, Any] | None:
        """The recorded request at this position, if this attempt has one."""
        entry = ReplayPlan(self.artifacts, store=self.store).entry(context, sequence)
        return None if entry is None else entry.payload

    def fidelity(self, attempt: FrozenAttempt) -> dict[str, Any]:
        """Whether the re-assembled prompt matches what the model actually received.

        A reconstruction is only useful if someone can tell how far it is from the
        original. When a recording exists this compares the two and reports the
        difference; when it does not, it says so rather than implying equivalence.

        The check is narrow on purpose: it compares the rendered text, so a faithful
        reconstruction passes and an unfaithful one fails, but passing does not prove
        the request was identical at the wire level.
        """
        context = CallContext(run_id=attempt.run_id, node_id=attempt.node_id,
                              node_run_id=attempt.node_run_id, attempt=attempt.attempt)
        payload = self.recording(context)
        report: dict[str, Any] = {
            "provenance": attempt.provenance,
            "memory_available_to_harness": self.memory is not None,
        }
        if payload is None:
            report["recorded"] = False
            report["note"] = ("no recording for this attempt: the prompt is rebuilt from "
                              "the frozen snapshot and the current configuration")
            return report
        replayed = self.prompt_for(attempt)
        sent = recorded_user_prompt(payload)
        report["recorded"] = True
        report["recorded_chars"] = None if sent is None else len(sent)
        report["reassembled_chars"] = len(replayed)
        report["matches"] = sent == replayed
        if sent is None:
            report["note"] = ("the recording has no user prompt to compare against; "
                              "fidelity is unknown for this attempt")
        elif sent != replayed:
            # The reconstruction is not the request. Say which way it differs, so the
            # difference is a measured fact rather than a caveat in a comment.
            report["note"] = ("the re-assembled prompt differs from the recorded one: "
                              "the inputs are the attempt's, but the way they are "
                              "rendered is today's")
            report["first_difference_at"] = _first_difference(sent, replayed)
        return report

    def _agent(self, agent_ref: str) -> Any:
        """The capability the node names, which holds its instructions and tools."""
        return self.registry.validate_agent(agent_ref)

    # -- building the prompt ------------------------------------------------

    def prompt_for(self, attempt: FrozenAttempt,
                   *, snapshot: dict[str, Any] | None = None,
                   include_memory: bool = True) -> str:
        """The prompt this attempt would be run with.

        ``snapshot`` and ``include_memory`` are the policy surface: an experiment
        varies one of them and leaves the rest of the request alone. They default to
        exactly what the worker does, so a harness run is the real prompt until an
        experiment deliberately changes it.
        """
        run_memory: list[str] = []
        promoted: list[tuple[str, str]] = []
        if include_memory and self.memory is not None:
            rows = self.memory.list(run_id=attempt.run_id)
            seen = {item.memory_id for item in rows}
            run_memory = [item.content for item in rows]
            promoted = [(item.domain or "general", item.content)
                        for item in self.memory.list(status="promoted")
                        if item.memory_id not in seen]
        return assemble_prompt(PromptParts(
            objective=attempt.objective, node_name=attempt.node_name,
            snapshot=attempt.snapshot if snapshot is None else snapshot,
            run_memory=run_memory, promoted_memory=promoted))

    # -- executing ----------------------------------------------------------

    async def run(self, attempt: FrozenAttempt, *,
                  snapshot: dict[str, Any] | None = None,
                  include_memory: bool = True,
                  attempts: int = 2) -> NodeRunResult:
        """Execute the attempt once, live, and report what it produced.

        `attempts` is a bounded retry of a transient provider fault. The gateway
        sets ``max_retries=0`` on purpose, because in production the durable node
        layer owns retries with backoff and an observable attempt; the harness has
        no such layer, so it has to own this itself or a flaky response becomes a
        failed experiment. The retry is here rather than in the gateway so that the
        production path keeps exactly one rule.
        """
        if attempt.tools:
            raise HarnessUnsupported(
                f"node {attempt.node_id!r} declares tools {list(attempt.tools)}; a "
                f"harness run would execute them without a ledger to make the calls "
                f"auditable. Run a whole run for a tool-using node, or add scratch-run "
                f"ledgering to this harness first.")
        prompt = self.prompt_for(attempt, snapshot=snapshot, include_memory=include_memory)
        started = time.monotonic()
        response = None
        for remaining in range(max(1, attempts), 0, -1):
            try:
                response = await self.gateway.generate(prompt=prompt,
                                                       system_prompt=attempt.instructions)
                break
            except Exception as exc:  # noqa: BLE001 - decided on the next line
                if remaining == 1 or not _retryable(exc):
                    raise
        assert response is not None  # the loop either assigned it or raised
        elapsed = time.monotonic() - started
        return NodeRunResult(
            node_id=attempt.node_id, attempt=attempt.attempt, prompt=prompt,
            text=response.text, model=response.model, provider=response.provider,
            input_tokens=response.input_tokens, output_tokens=response.output_tokens,
            cached_tokens=response.cache_read_tokens, cost=response.cost,
            seconds=elapsed)


def _retryable(exc: BaseException) -> bool:
    """Would the durable node layer have retried this? Reuse its verdict."""
    from anchor.runtime.worker import is_retryable_model_error

    return is_retryable_model_error(exc)


def compare(before: NodeRunResult, after: NodeRunResult) -> dict[str, Any]:
    """What changed between two harness runs of the same attempt.

    Deliberately says nothing about which is better: the harness reports, the review
    gate judges. It does report whether the output is identical, because "the policy
    saved a third of the prompt and the answer did not move" is the result most worth
    having and the easiest to miss.
    """
    return {
        "node_id": before.node_id,
        "identical_output": before.text == after.text,
        "prompt_chars": [len(before.prompt), len(after.prompt)],
        "prompt_delta": len(after.prompt) - len(before.prompt),
        "output_chars": [len(before.text), len(after.text)],
        "input_tokens": [before.input_tokens, after.input_tokens],
        "output_tokens": [before.output_tokens, after.output_tokens],
        "cost": [before.cost, after.cost],
        "seconds": [round(before.seconds, 2), round(after.seconds, 2)],
    }
