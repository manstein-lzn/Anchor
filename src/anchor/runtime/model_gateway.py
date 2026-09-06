"""Vendor-neutral model gateway and optional PydanticAI implementation."""

from __future__ import annotations

from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from typing import Any, Protocol

from anchor.runtime.capabilities import ModelProfile
from anchor.runtime.secrets import SecretProvider


@dataclass(frozen=True)
class ModelResponse:
    text: str
    provider: str
    model: str
    response_id: str | None = None


@dataclass(frozen=True)
class ToolFunction:
    """One model-callable tool. `call` receives raw JSON arguments."""

    name: str
    description: str = ""
    call: Callable[[str], Awaitable[str]] = field(default=lambda arguments: _unavailable(arguments))


async def _unavailable(arguments: str) -> str:  # pragma: no cover - defensive default
    raise RuntimeError("tool function has no implementation")


class ModelGateway(Protocol):
    async def generate(self, *, prompt: str, system_prompt: str = "") -> ModelResponse: ...

    async def generate_with_tools(self, *, prompt: str, system_prompt: str = "",
                                  tools: list[ToolFunction]) -> ModelResponse: ...


class PydanticAIModelGateway:
    """One-shot Responses API gateway built on PydanticAI.

    The provider and model are constructed once, while credentials are obtained
    only at construction time from the SecretProvider and never exposed by this
    object.
    """

    def __init__(self, profile: ModelProfile, secrets: SecretProvider,
                 model: Any | None = None) -> None:
        """`model` accepts a prebuilt pydantic-ai model (tests use TestModel)."""
        self.profile = profile
        api_key = secrets.get(profile.secret_ref)
        try:
            from pydantic_ai import Agent
            from pydantic_ai.models.openai import OpenAIChatModel, OpenAIResponsesModel
            from pydantic_ai.providers.openai import OpenAIProvider
            import httpx2
        except ImportError as exc:  # pragma: no cover - exercised in minimal installs
            raise RuntimeError("install Anchor's pydantic-ai extra to use this gateway") from exc
        if profile.wire_api == "responses":
            model_type = OpenAIResponsesModel
        elif profile.wire_api == "chat_completions":
            model_type = OpenAIChatModel
        else:
            raise ValueError(f"unsupported OpenAI wire_api: {profile.wire_api}")
        # Worker processes must not silently inherit a workstation's proxy or
        # credential environment. Operators can provide an explicit transport
        # when a proxy is genuinely required.
        self._http_client = httpx2.AsyncClient(trust_env=False)
        provider = OpenAIProvider(base_url=profile.base_url, api_key=api_key, http_client=self._http_client)
        self._model = model if model is not None else model_type(profile.model, provider=provider)
        self._agent = Agent(self._model, output_type=str)

    async def generate(self, *, prompt: str, system_prompt: str = "") -> ModelResponse:
        result = await self._agent.run(prompt, instructions=system_prompt or None)
        output = result.output
        text = output if isinstance(output, str) else str(output)
        response_id = getattr(result, "response_id", None)
        return ModelResponse(text=text, provider=self.profile.provider, model=self.profile.model,
                             response_id=response_id)

    async def generate_with_tools(self, *, prompt: str, system_prompt: str = "",
                                  tools: list[ToolFunction]) -> ModelResponse:
        """Run the model with function tools; each call is ledger-bound by the caller."""
        from pydantic_ai import Agent as PydanticAgent

        def make_entry(call: Callable[[str], Awaitable[str]]):
            async def tool_entry(arguments_json: str = "{}") -> str:
                return await call(arguments_json)
            return tool_entry

        agent: Any = PydanticAgent(self._model, output_type=str)
        for function in tools:
            entry = make_entry(function.call)
            entry.__name__ = function.name.replace("-", "_").replace(".", "_")
            entry.__doc__ = function.description or f"Call the {function.name} tool."
            agent.tool_plain(entry)
        result = await agent.run(prompt, instructions=system_prompt or None)
        output = result.output
        text = output if isinstance(output, str) else str(output)
        response_id = getattr(result, "response_id", None)
        return ModelResponse(text=text, provider=self.profile.provider, model=self.profile.model,
                             response_id=response_id)

    async def close(self) -> None:
        await self._http_client.aclose()


def build_model_gateway(profile: ModelProfile, secrets: SecretProvider) -> ModelGateway:
    if profile.provider in {"openai", "openai_compatible", "rightcode"}:
        return PydanticAIModelGateway(profile, secrets)
    raise ValueError(f"unsupported model provider: {profile.provider}")
