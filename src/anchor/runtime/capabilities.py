"""Capability descriptors and registries used by execution workers.

The domain graph stores stable references (``agent_ref``/``tool_ref``).  This
module resolves those references at runtime without making the domain depend on
an Agent framework or a provider SDK.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Literal, Mapping, Protocol, Sequence

from pydantic import Field, model_validator

from anchor.domain.models import DomainModel
from anchor.domain.conditions import validate_condition


class ModelProfile(DomainModel):
    ref: str = Field(min_length=1, max_length=200)
    provider: str = Field(min_length=1, max_length=100)
    model: str = Field(min_length=1, max_length=200)
    base_url: str | None = Field(default=None, max_length=2000)
    wire_api: str = Field(default="responses", pattern=r"^[a-z][a-z0-9_-]*$")
    secret_ref: str = Field(min_length=1, max_length=200)
    stream: bool = False


class AgentCapability(DomainModel):
    ref: str = Field(min_length=1, max_length=200)
    model_ref: str = Field(min_length=1, max_length=200)
    instructions: str = ""
    tool_refs: list[str] = Field(default_factory=list)
    output_format: str = Field(default="text", pattern=r"^[a-z][a-z0-9_-]*$")
    # Physical node timeout, not a task lifetime budget. It protects a single
    # model/tool call from hanging forever; healthy runs may exceed this across
    # many business cycles. The execution policy controls task-level visibility.
    timeout_seconds: float = Field(default=600, gt=0, le=3600)
    # Output repair retries are serialization repair only. They never replay
    # tools, never invent evidence, and never extend a healthy run's lifetime.
    output_retries: int = Field(default=0, ge=0, le=2)
    # Bounded retries for transient model/transport failures. Zero keeps the
    # legacy default: a failed attempt is recorded and the node stays failed
    # until an operator or supervisor decides to retry. Academic bundles may
    # opt into bounded retries explicitly; they are not task-round budgets.
    max_retries: int = Field(default=0, ge=0, le=5)
    # Tool execution is separately bounded from model turns. Zero keeps the
    # legacy unlimited total, while per-tool limits constrain expensive or
    # rate-limited adapters without coupling policy to prompts.
    max_tool_calls: int = Field(default=0, ge=0, le=200)
    max_parallel_tools: int = Field(default=4, ge=1, le=16)
    tool_call_limits: dict[str, int] = Field(default_factory=dict)
    # Domain hooks resolved by the composition root; the kernel stays generic.
    behavior_ref: str | None = Field(default=None, max_length=200)
    # Deprecated migration shim for pre-behavior profiles. The kernel never
    # reads it; it only derives behavior_ref when that is absent.
    academic_role: Literal["planner", "researcher", "reviewer"] | None = None

    @model_validator(mode="after")
    def derive_behavior_reference(self):
        if self.behavior_ref is None and self.academic_role:
            self.behavior_ref = f"academic.{self.academic_role}"
        return self

    @model_validator(mode="after")
    def validate_tool_limits(self):
        if any(not ref or limit < 1 or limit > 200
               for ref, limit in self.tool_call_limits.items()):
            raise ValueError("tool_call_limits require nonempty refs and values from 1 to 200")
        if any(ref not in self.tool_refs for ref in self.tool_call_limits):
            raise ValueError("tool_call_limits may only reference tools assigned to the agent")
        if self.max_tool_calls and any(limit > self.max_tool_calls
                                       for limit in self.tool_call_limits.values()):
            raise ValueError("per-tool limits cannot exceed max_tool_calls")
        return self


class ToolCapability(DomainModel):
    ref: str = Field(min_length=1, max_length=500)
    description: str = ""
    side_effect: bool = False
    idempotent: bool = False
    operation_kind: str = Field(default="read", pattern=r"^[a-z][a-z0-9_-]*$")
    # Evidence shaping for tool results returned to the model. The complete
    # artifact stays durable; only the model-facing excerpt is bounded.
    evidence_json: bool = False
    model_excerpt_chars: int | None = Field(default=None, ge=1, le=200_000)
    retry_excerpt_chars: int | None = Field(default=None, ge=1, le=200_000)
    excerpt_list_limit: int = Field(default=10, ge=1, le=1000)
    # Higher priority evidence is preloaded first when the retry context is bounded.
    evidence_priority: int = Field(default=0, ge=0, le=100)


class VerifierCapability(DomainModel):
    ref: str = Field(min_length=1, max_length=200)
    version: str = Field(default="v1", min_length=1, max_length=100)
    adapter: Literal["deterministic", "model"]
    expression: str | None = Field(default=None, max_length=2000)
    model_ref: str | None = Field(default=None, max_length=200)
    instructions: str = Field(default="", max_length=8000)

    @model_validator(mode="after")
    def validate_adapter_config(self):
        if self.adapter == "deterministic":
            if not self.expression:
                raise ValueError("deterministic verifier requires expression")
            if self.model_ref is not None:
                raise ValueError("deterministic verifier cannot configure model_ref")
            validate_condition(self.expression)
        else:
            if not self.model_ref:
                raise ValueError("model verifier requires model_ref")
            if self.expression is not None:
                raise ValueError("model verifier cannot configure expression")
        return self


class CapabilityRegistryError(LookupError):
    """Raised when a graph reference cannot be resolved."""


class CapabilityRegistry:
    """In-memory immutable-by-convention registry for one worker process."""

    def __init__(self, *, models: Sequence[ModelProfile] = (), agents: Sequence[AgentCapability] = (),
                 tools: Sequence[ToolCapability] = (),
                 verifiers: Sequence[VerifierCapability] = ()) -> None:
        self._models = self._index(models, "model")
        self._agents = self._index(agents, "agent")
        self._tools = self._index(tools, "tool")
        self._verifiers = self._index(verifiers, "verifier")

    @staticmethod
    def _index(items: Sequence[DomainModel], kind: str) -> dict[str, DomainModel]:
        result: dict[str, DomainModel] = {}
        for item in items:
            ref = getattr(item, "ref")
            if ref in result:
                raise ValueError(f"duplicate {kind} capability reference: {ref}")
            result[ref] = item
        return result

    def model(self, ref: str) -> ModelProfile:
        try:
            return self._models[ref]  # type: ignore[return-value]
        except KeyError as exc:
            raise CapabilityRegistryError(f"unknown model capability: {ref}") from exc

    def agent(self, ref: str) -> AgentCapability:
        try:
            return self._agents[ref]  # type: ignore[return-value]
        except KeyError as exc:
            raise CapabilityRegistryError(f"unknown agent capability: {ref}") from exc

    def tool(self, ref: str) -> ToolCapability:
        try:
            return self._tools[ref]  # type: ignore[return-value]
        except KeyError as exc:
            raise CapabilityRegistryError(f"unknown tool capability: {ref}") from exc

    def verifier(self, ref: str) -> VerifierCapability:
        try:
            return self._verifiers[ref]  # type: ignore[return-value]
        except KeyError as exc:
            raise CapabilityRegistryError(f"unknown verifier capability: {ref}") from exc

    def validate_agent(self, ref: str) -> AgentCapability:
        agent = self.agent(ref)
        self.model(agent.model_ref)
        for tool_ref in agent.tool_refs:
            self.tool(tool_ref)
        return agent

    def validate_verifier(self, ref: str) -> VerifierCapability:
        verifier = self.verifier(ref)
        if verifier.model_ref is not None:
            self.model(verifier.model_ref)
        return verifier

    def snapshot(self) -> Mapping[str, tuple[str, ...]]:
        """Return stable reference lists for diagnostics, never credentials."""
        return {"models": tuple(sorted(self._models)), "agents": tuple(sorted(self._agents)),
                "tools": tuple(sorted(self._tools)), "verifiers": tuple(sorted(self._verifiers))}


class CapabilityResolver(Protocol):
    def validate_agent(self, ref: str) -> AgentCapability: ...

    def validate_verifier(self, ref: str) -> VerifierCapability: ...
