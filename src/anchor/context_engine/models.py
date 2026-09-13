"""Pure value objects for Anchor's node invocation context."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Literal, Mapping
from uuid import UUID
import hashlib
import json


Capacity = Literal["within_budget", "unknown", "exceeded"]
EMPTY_BLOCK = "(none)"


@dataclass(frozen=True)
class PromptParts:
    objective: str
    node_name: str
    snapshot: dict[str, Any]
    run_memory: tuple[str, ...] = ()
    promoted_memory: tuple[tuple[str, str], ...] = ()


@dataclass(frozen=True)
class PromptSegments:
    prefix: str
    declared: str
    working_set: str

    def hashes(self, *, prefix: str | None = None) -> dict[str, str]:
        digest = lambda text: hashlib.sha256(text.encode("utf-8")).hexdigest()
        return {"prefix_hash": digest(self.prefix if prefix is None else prefix),
                "declared_hash": digest(self.declared),
                "working_set_hash": digest(self.working_set)}


def _canonical(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def _run_memory_block(parts: tuple[str, ...]) -> str:
    return "\n".join(f"- {item}" for item in parts) or EMPTY_BLOCK


def _promoted_memory_block(parts: tuple[tuple[str, str], ...]) -> str:
    return "\n".join(f"- [{domain or 'general'}] {content}" for domain, content in parts) or EMPTY_BLOCK


def prompt_segments(parts: PromptParts, *, prefix: str = "") -> PromptSegments:
    return PromptSegments(
        prefix=prefix,
        declared=(f"Task objective:\n{parts.objective}\n\n"
                  f"Execute graph node: {parts.node_name}\n"
                  f"\nDurable input snapshot:\n{_canonical(parts.snapshot)}"),
        working_set=(f"\n\nRun memory:\n{_run_memory_block(parts.run_memory)}"
                     f"\n\nPromoted organizational knowledge:\n"
                     f"{_promoted_memory_block(parts.promoted_memory)}"))


def render(segments: PromptSegments) -> str:
    return f"{segments.declared}{segments.working_set}"


def assemble_prompt(parts: PromptParts) -> str:
    return render(prompt_segments(parts))


@dataclass(frozen=True)
class ContextSource:
    """A caller-authorized source available to the planner."""

    ref: str
    kind: str
    content: str
    required: bool = False

    @property
    def content_hash(self) -> str:
        return hashlib.sha256(self.content.encode("utf-8")).hexdigest()


@dataclass(frozen=True)
class ContextView:
    """A named, bounded view of one authorized source."""

    name: str
    source_ref: str
    content: str
    required: bool = False


@dataclass(frozen=True)
class ContextRequest:
    """The Anchor-owned boundary for one model invocation."""

    run_id: UUID | None
    node_run_id: UUID | None
    node_id: str
    node_name: str
    attempt: int
    model_ref: str | None
    objective: str
    instructions: str
    declared_input: Mapping[str, Any]
    allowed_tools: tuple[str, ...] = ()
    output_token_reservation: int | None = None
    context_window: int | None = None
    policy_version: str = "anchor.context.v1"
    graph_version_id: UUID | None = None

    def __post_init__(self) -> None:
        import copy
        object.__setattr__(self, "declared_input", copy.deepcopy(dict(self.declared_input)))


@dataclass(frozen=True)
class ContextSegment:
    """One deterministic, source-labelled part of an invocation."""

    kind: Literal["instructions", "declared_input", "working_set"]
    text: str
    source_ref: str | None
    required: bool
    order: int

    @property
    def content_hash(self) -> str:
        import hashlib
        return hashlib.sha256(self.text.encode("utf-8")).hexdigest()


@dataclass(frozen=True)
class Omission:
    """A source intentionally absent from the current invocation."""

    source_ref: str
    reason: Literal["not_declared", "policy", "budget", "truncated", "unavailable"]
    recoverable: bool
    recovery_ref: str | None = None


@dataclass(frozen=True)
class ContextPlan:
    """A fully assembled invocation plan, independent of the model framework."""

    request: ContextRequest
    segments: tuple[ContextSegment, ...]
    omissions: tuple[Omission, ...]
    system_prompt: str
    user_prompt: str
    estimated_input_tokens: int | None
    reserved_output_tokens: int | None
    capacity: Capacity
    plan_hash: str
    capacity_scope: str = "initial_invocation"
    sources: tuple[ContextSource, ...] = ()
    views: tuple[ContextView, ...] = ()

    def hashes(self, *, prefix: str | None = None) -> dict[str, str]:
        """Return hashes for usage telemetry without exposing framework types."""
        import hashlib

        def digest(value: str) -> str:
            return hashlib.sha256(value.encode("utf-8")).hexdigest()

        texts = {segment.kind: segment.text for segment in self.segments}
        return {
            "prefix_hash": digest(texts["instructions"] if prefix is None else prefix),
            "declared_hash": digest(texts["declared_input"]),
            "working_set_hash": digest(texts["working_set"]),
        }
