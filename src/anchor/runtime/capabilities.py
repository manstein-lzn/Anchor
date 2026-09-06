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


class AgentCapability(DomainModel):
    ref: str = Field(min_length=1, max_length=200)
    model_ref: str = Field(min_length=1, max_length=200)
    instructions: str = ""
    tool_refs: list[str] = Field(default_factory=list)
    output_format: str = Field(default="text", pattern=r"^[a-z][a-z0-9_-]*$")


class ToolCapability(DomainModel):
    ref: str = Field(min_length=1, max_length=500)
    description: str = ""
    side_effect: bool = False
    idempotent: bool = False
    operation_kind: str = Field(default="read", pattern=r"^[a-z][a-z0-9_-]*$")


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
