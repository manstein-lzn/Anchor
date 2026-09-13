"""Vendor-neutral model gateway and optional PydanticAI implementation."""

from __future__ import annotations

from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from typing import Any, Protocol

from anchor.runtime.capabilities import ModelProfile
from anchor.runtime.model_recording import ModelRecorder, RecordingMode, RecordingModel
from anchor.runtime.model_replay import ReplayModel, ReplayPlan
from anchor.runtime.secrets import SecretProvider


@dataclass(frozen=True)
class ModelResponse:
    text: str
    provider: str
    model: str
    response_id: str | None = None
    # Token accounting. An agent tool loop re-sends its whole conversation on
    # every call, so a single node attempt can cost millions of input tokens;
    # without this the spend is invisible until the provider bill arrives.
    input_tokens: int = 0
    output_tokens: int = 0
    requests: int = 0
    cost: float | None = None
    # `input_tokens` is the gross prompt size. A tool loop re-sends its whole
    # conversation every call, so most of that is a prefix the provider serves
    # from cache at a fraction of the price. Recording the cached share is what
    # separates a frightening token counter from the amount actually charged.
    cache_read_tokens: int = 0
    cache_write_tokens: int = 0
    # The conversation this call produced, in order: what was asked, what the model said, which
    # tools it called and what they returned. Carried out so a caller can append it to a trace
    # instead of reconstructing behaviour by re-running experiments — which is what happens when
    # this is missing, and it is much slower than reading a file.
    messages: tuple[dict, ...] = ()


@dataclass(frozen=True)
class ToolFunction:
    """One model-callable tool. `call` receives raw JSON arguments."""

    name: str
    description: str = ""
    call: Callable[[str], Awaitable[str]] = field(default=lambda arguments: _unavailable(arguments))


async def _unavailable(arguments: str) -> str:  # pragma: no cover - defensive default
    raise RuntimeError("tool function has no implementation")


def _as_text(output: Any) -> str:
    """A readable string form of a structured answer, for the spend record.

    The caller wants the object; a `ModelResponse` carries text, and it should be something a
    person can read in a report rather than a `repr` of a pydantic model.
    """
    if isinstance(output, str):
        return output
    dump = getattr(output, "model_dump_json", None)
    return dump() if callable(dump) else str(output)


class ModelGateway(Protocol):
    async def generate(self, *, prompt: str, system_prompt: str = "") -> ModelResponse: ...

    async def generate_with_tools(self, *, prompt: str, system_prompt: str = "",
                                  tools: list[ToolFunction]) -> ModelResponse: ...

    async def generate_structured(self, *, prompt: str, system_prompt: str = "",
                                  output_type: Any) -> tuple[Any, ModelResponse]: ...


class PydanticAIModelGateway:
    """One-shot Responses API gateway built on PydanticAI.

    The provider and model are constructed once, while credentials are obtained
    only at construction time from the SecretProvider and never exposed by this
    object.
    """

    def __init__(self, profile: ModelProfile, secrets: SecretProvider,
                 model: Any | None = None, recorder: ModelRecorder | None = None) -> None:
        """`model` accepts a prebuilt pydantic-ai model (tests use TestModel).

        `recorder` is optional and does nothing unless recording is enabled; when
        it is, the model is wrapped here so every call it forwards is recorded.
        """
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
        # The window is told to PydanticAI as well as kept here, and it has to be: the framework
        # computes `context_window_used` from *its* `context_window`, so a window only this code
        # knows leaves that fraction permanently unknown, and a compressor that reads it can never
        # choose a moment. It looked exactly like a compressor that was working — nothing happened,
        # either way.
        model_kwargs: dict[str, Any] = {}
        if profile.context_window:
            model_kwargs["profile"] = {"context_window": profile.context_window}
        self._model = (model if model is not None
                       else model_type(profile.model, provider=provider, **model_kwargs))
        if recorder is not None and recorder.enabled:
            # Wrap the model, not this gateway: one generate_with_tools call covers
            # a whole agent turn, so recording here would keep one final answer and
            # lose the growing context inside the tool loop.
            if api_key:
                # The recorder refuses to persist a call whose text contains a
                # resolved secret, and only this object holds the value.
                recorder.forbid(api_key)
            if recorder.mode is RecordingMode.REPLAY:
                # Replay serves recorded answers and never falls through to the wrapped
                # model, so a replay cannot quietly become live.
                plan = ReplayPlan(recorder.artifacts, store=recorder.store)
                self._model = ReplayModel(self._model, plan, store=recorder.store,
                                          replay_of=recorder.replay_of)
            else:
                self._model = RecordingModel(self._model, recorder)
        #: Capabilities every agent this gateway builds must carry. Held here rather than baked
        #: into `self._agent`, because `generate_with_tools` builds its own agent — and a node with
        #: tools is exactly the one whose history grows unboundedly, so a capability that reached
        #: only the tool-less path would miss the case it exists for.
        self._capabilities: list[Any] = []
        self._agent = Agent(self._model, output_type=str,
                            model_settings=self._settings(),
                            capabilities=list(self._capabilities))

    def _settings(self) -> Any:
        """Explicit output budget, when the profile sets one.

        A reasoning model spends part of the budget on thinking before it emits
        the answer, so without a generous explicit value a structured response
        can be truncated mid-string.
        """
        from pydantic_ai.settings import ModelSettings

        if not self.profile.max_tokens:
            return None
        return ModelSettings(max_tokens=self.profile.max_tokens)

    async def generate(self, *, prompt: str, system_prompt: str = "") -> ModelResponse:
        return await self._run_agent(self._agent, prompt=prompt, system_prompt=system_prompt)

    async def _run_agent(self, agent, *, prompt: str, system_prompt: str,
                         request_limit: int | None = None,
                         message_history: tuple[dict, ...] | None = None) -> ModelResponse:
        # Pydantic AI caps an agent run at 50 requests by default. A node that works until a goal is
        # met is not a chat turn and can legitimately need more, so the caller sets this — and 50 is
        # low enough that a real gather loop reaches it, which is how the default was found.
        limits = None
        if request_limit:
            from pydantic_ai.usage import UsageLimits
            limits = UsageLimits(request_limit=request_limit)
        history = None
        if message_history:
            # Handed back as the library's own message type; a dict is not a message.
            from pydantic_ai.messages import ModelMessagesTypeAdapter
            history = ModelMessagesTypeAdapter.validate_python(
                [{key: value for key, value in record.items() if key != "class_"}
                 for record in message_history])
        if self.profile.stream:
            async with agent.run_stream(prompt, instructions=system_prompt or None,
                                        usage_limits=limits,
                                        message_history=history) as result:
                output = await result.get_output()
        else:
            result = await agent.run(prompt, instructions=system_prompt or None,
                                     usage_limits=limits, message_history=history)
            output = result.output
        text = output if isinstance(output, str) else str(output)
        return self._response_from(result, text=text)

    @staticmethod
    def _messages_of(result) -> tuple[dict, ...]:
        """The conversation as plain JSON, so a trace is readable without the library to hand.

        Through the library's own adapter, not `model_dump`: these messages are not pydantic models,
        and guessing at their shape produced a trace of `repr` strings. The adapter also round-trips,
        which is what lets a caller hand the history back to continue the same conversation.
        """
        messages = list(getattr(result, "all_messages", lambda: [])())
        if not messages:
            return ()
        from pydantic_ai.messages import ModelMessagesTypeAdapter
        dumped = ModelMessagesTypeAdapter.dump_python(messages, mode="json")
        # `class_`, not `kind`: the record already carries `kind` as the library's discriminator,
        # and a field of mine under that name overwrote it — which is why handing the history back
        # then failed to validate.
        return tuple({"class_": type(message).__name__, **record}
                     for message, record in zip(messages, dumped))

    def _response_from(self, result, *, text: str) -> ModelResponse:
        """The spend and identity of one call, shared by every shape of answer."""
        response_id = getattr(result, "response_id", None)
        usage = None
        try:
            usage = result.usage() if callable(getattr(result, "usage", None)) else result.usage
        except Exception:  # noqa: BLE001 - usage is reporting, never a node failure
            usage = None
        raw_cost = getattr(usage, "cost", None)
        return ModelResponse(
            text=text, provider=self.profile.provider, model=self.profile.model,
            response_id=response_id,
            input_tokens=int(getattr(usage, "input_tokens", 0) or 0),
            output_tokens=int(getattr(usage, "output_tokens", 0) or 0),
            requests=int(getattr(usage, "requests", 0) or 0),
            cache_read_tokens=int(getattr(usage, "cache_read_tokens", 0) or 0),
            cache_write_tokens=int(getattr(usage, "cache_write_tokens", 0) or 0),
            cost=float(raw_cost) if raw_cost is not None else None,
            messages=self._messages_of(result))

    async def generate_structured(self, *, prompt: str, system_prompt: str,
                                  output_type: Any) -> tuple[Any, ModelResponse]:
        """One call whose answer the provider validates against ``output_type``.

        Distinct from asking for JSON in a prompt and parsing it afterwards. The schema is sent, so
        a constraint in it is one the model cannot violate rather than one this code notices
        afterwards — which is the whole reason the proposal schema enumerates the ids an operation
        may name. An answer that is parsed and then checked has already had its chance to be wrong.

        Streaming is refused rather than silently downgraded: a streamed structured answer is a
        different feature, and returning a partial object that validates against nothing would be
        worse than saying so.
        """
        from pydantic_ai import Agent as PydanticAgent
        from pydantic_ai import NativeOutput

        if self.profile.stream:
            raise RuntimeError(
                "a structured answer cannot be streamed through this gateway; use a profile with "
                "stream=False for calls whose result is validated against a schema")
        # `NativeOutput`, not the default tool-based output. The default forces a tool choice, and
        # a thinking model refuses that outright: `400 Thinking mode does not support this
        # tool_choice`. The native path sends the JSON schema as the response format instead, which
        # the provider supports, and the schema still travels — including the id enumeration that
        # makes a fabricated id impossible, which is the reason this call is structured at all.
        agent: Any = PydanticAgent(self._model, output_type=NativeOutput(output_type),
                                   model_settings=self._settings())
        result = await agent.run(prompt, instructions=system_prompt or None)
        output = result.output
        return output, self._response_from(result, text=_as_text(output))

    async def generate_with_tools(self, *, prompt: str, system_prompt: str = "",
                                  tools: list[ToolFunction],
                                  request_limit: int | None = None,
                                  message_history: tuple[dict, ...] | None = None
                                  ) -> ModelResponse:
        """Run the model with function tools until it stops calling them.

        `request_limit` bounds how many model calls one such run may make; unset keeps Pydantic AI's
        own default, which is 50 and too low for a node that works until its goal is met.

        `message_history` continues an earlier call rather than starting a new conversation. Without
        it a caller that asks the model to keep going throws away what it just did, and the model
        cannot see its own tool calls from the turn before.
        """
        from pydantic_ai import Agent as PydanticAgent

        def make_entry(call: Callable[[str], Awaitable[str]]):
            async def tool_entry(arguments_json: str = "{}") -> str:
                return await call(arguments_json)
            return tool_entry

        agent: Any = PydanticAgent(self._model, output_type=str,
                                   model_settings=self._settings(),
                                   capabilities=list(self._capabilities))
        for function in tools:
            entry = make_entry(function.call)
            entry.__name__ = function.name.replace("-", "_").replace(".", "_")
            entry.__doc__ = function.description or f"Call the {function.name} tool."
            agent.tool_plain(entry)
        return await self._run_agent(agent, prompt=prompt, system_prompt=system_prompt,
                                     request_limit=request_limit,
                                     message_history=message_history)

    async def close(self) -> None:
        await self._openai_client.close()


def build_model_gateway(profile: ModelProfile, secrets: SecretProvider,
                        *, recorder: ModelRecorder | None = None) -> ModelGateway:
    # Named gateways such as ZenMux expose the OpenAI-compatible wire
    # contract; their provider label is still useful for configuration and
    # diagnostics, so accept it without treating the key as an OpenAI key.
    if profile.provider in {"openai", "openai_compatible", "rightcode", "zenmux", "a6api", "deepseek"}:
        return PydanticAIModelGateway(profile, secrets, recorder=recorder)
    raise ValueError(f"unsupported model provider: {profile.provider}")
