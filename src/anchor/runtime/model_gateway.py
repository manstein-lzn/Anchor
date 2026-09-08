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
            from openai import AsyncOpenAI
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
        # httpx's default read timeout is five seconds. That is too short for
        # a hosted model's time-to-first-token, especially when the provider is
        # cold-starting or queueing a request. Keep transport timeouts longer
        # than node budgets; the worker's per-agent timeout remains the
        # authoritative execution limit.
        self._http_client = httpx2.AsyncClient(
            trust_env=False,
            timeout=httpx2.Timeout(connect=30.0, read=900.0, write=60.0, pool=30.0),
        )
        # Retries belong to the durable node layer, where attempts, backoff and
        # evidence reuse are observable. The OpenAI SDK otherwise retries each
        # request twice within a single node attempt and amplifies 60-second
        # upstream gateway failures into several invisible minutes.
        self._openai_client = AsyncOpenAI(
            base_url=profile.base_url, api_key=api_key,
            http_client=self._http_client, max_retries=0,
        )
        provider = OpenAIProvider(openai_client=self._openai_client)
        self._model = model if model is not None else model_type(profile.model, provider=provider)
        self._agent = Agent(self._model, output_type=str)

    async def generate(self, *, prompt: str, system_prompt: str = "") -> ModelResponse:
        return await self._run_agent(self._agent, prompt=prompt, system_prompt=system_prompt)

    async def _run_agent(self, agent, *, prompt: str, system_prompt: str) -> ModelResponse:
        if self.profile.stream:
            async with agent.run_stream(prompt, instructions=system_prompt or None) as result:
                output = await result.get_output()
        else:
            result = await agent.run(prompt, instructions=system_prompt or None)
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
        return await self._run_agent(agent, prompt=prompt, system_prompt=system_prompt)

    async def close(self) -> None:
        await self._openai_client.close()


def build_model_gateway(profile: ModelProfile, secrets: SecretProvider) -> ModelGateway:
    # Named gateways such as ZenMux expose the OpenAI-compatible wire
    # contract; their provider label is still useful for configuration and
    # diagnostics, so accept it without treating the key as an OpenAI key.
    if profile.provider in {"openai", "openai_compatible", "rightcode", "zenmux", "a6api", "deepseek"}:
        return PydanticAIModelGateway(profile, secrets)
    raise ValueError(f"unsupported model provider: {profile.provider}")
