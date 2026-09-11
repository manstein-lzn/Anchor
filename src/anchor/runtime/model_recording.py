"""Record what a model actually saw, so a context policy can be inspected.

Context management decides which content reaches an agent, how it is compressed
and how it is recalled. Judging such a policy needs controlled comparison, and
nothing in a run today can be held still: one campaign costs half an hour, is
entirely non-deterministic, and leaves no record of the prompt any node received.
When a writer produces the wrong artifact, the question "what exactly did it see?"
cannot be answered, only guessed at.

This module answers it. Every model call is written to the artifact store as an
immutable projection together with a `model.call` event that attributes it to a
node attempt and a sequence within that attempt. Reading a recording back gives
the literal messages the model was given.

Three constraints, from ADR-044:

* **A recording is a projection, never state.** I2 says a cache, a memory store or
  a summary must never independently determine recovery, and a recording is the
  same kind of thing: evidence for inspection, deletable without changing any
  recovery semantics. So a recorder never fails a run. If a recording cannot be
  written, or writing it would persist a secret, the call is dropped and the run
  continues — a projection that can kill a healthy run would have become state.
* **The wrapper goes on the model, not on the gateway.** One
  ``ModelGateway.generate_with_tools`` call covers a whole agent turn, because
  PydanticAI runs the tool loop inside ``agent.run()``. Recording there would keep
  one final answer and lose the growing context inside the loop, which is the part
  context work needs to observe.
* **Replay will match by position, not by prompt digest.** The digest is recorded
  so a divergence can be explained, never so that it can be the match key: the
  whole point of the exercise is to change the prompt.

Recording is off unless a mode says otherwise, so production pays nothing.
"""

from __future__ import annotations

import base64
import hashlib
import json
import logging
import re
from collections.abc import AsyncGenerator, Iterator, Sequence
from contextlib import asynccontextmanager, contextmanager
from contextvars import ContextVar, Token
from dataclasses import asdict, dataclass, is_dataclass
from enum import StrEnum
from typing import Any, Protocol
from uuid import UUID

from pydantic_ai import ModelMessagesTypeAdapter
from pydantic_ai.messages import ModelMessage
from pydantic_ai.models import ModelRequestParameters
from pydantic_ai.models.wrapper import WrapperModel
from pydantic_ai.settings import ModelSettings

from anchor.domain.context import canonical_json
from anchor.runtime.artifacts import ArtifactStore


logger = logging.getLogger(__name__)

#: Bumped when the recorded payload changes shape, so an old recording is not
#: silently read as if it were current.
RECORDING_FORMAT = 1

#: Obvious credential shapes. This is a backstop, not the guard: the guard is the
#: literal secret values the caller passes as `forbidden`.
_SECRET_PATTERNS = (
    re.compile(r"\bsk-[A-Za-z0-9_\-]{16,}"),
    re.compile(r"\bBearer\s+[A-Za-z0-9_\-\.]{20,}"),
)


class RecordingMode(StrEnum):
    """Whether model calls are recorded, replayed, or neither."""

    OFF = "off"
    RECORD = "record"
    REPLAY = "replay"


class EventStore(Protocol):
    """The narrow slice of the state store recording and replay need.

    One protocol rather than two, because a recorder that cannot read the events it
    wrote cannot serve a replay, and in production there is one store either way.
    """

    def append_event(self, *, stream_id: UUID, event_type: str, payload: dict[str, Any],
                     idempotency_key: str) -> Any: ...

    def list_events(self, stream_id: UUID) -> list[dict[str, Any]]: ...


@dataclass(frozen=True)
class CallContext:
    """Which node attempt is executing, so a call can be attributed."""

    run_id: UUID
    node_id: str
    node_run_id: UUID
    attempt: int


_active: ContextVar[CallContext | None] = ContextVar("anchor_model_call_context",
                                                     default=None)


def active_call() -> CallContext | None:
    """The node attempt currently executing, if any."""
    return _active.get()


def unbind_call(token: Token[CallContext | None]) -> None:
    """Restore the call context that was active before :func:`bind_call`."""
    _active.reset(token)


def bind_call(context: CallContext) -> Token[CallContext | None]:
    """Attribute subsequent model calls to ``context`` until the token is reset.

    The worker already surrounds a node attempt with a ``try/finally``, so it binds
    once for the whole attempt rather than nesting a block around each model call.
    That way every call is covered, including the serialization repair that runs
    after a validation failure.
    """
    return _active.set(context)


@contextmanager
def recording_scope(context: CallContext) -> Iterator[CallContext]:
    """Attribute every model call made inside this block to ``context``.

    A worker service builds one gateway at startup and shares it across every run,
    so the model wrapper cannot be told its node at construction. The context is
    carried in a ``ContextVar`` instead: it follows the task, it cannot leak into
    the heartbeat task, and it needs no lock.
    """
    token = _active.set(context)
    try:
        yield context
    finally:
        _active.reset(token)


def _jsonable(value: Any) -> Any:
    """Convert library types into something JSON can hold.

    Bytes are base64-tagged rather than stringified, so a recording of a
    multimodal response can be read back as the bytes that were sent.
    """
    if isinstance(value, bytes):
        return {"__bytes__": base64.b64encode(value).decode("ascii")}
    if isinstance(value, dict):
        return {str(key): _jsonable(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_jsonable(item) for item in value]
    if is_dataclass(value) and not isinstance(value, type):
        return _jsonable(asdict(value))
    if isinstance(value, StrEnum):
        return str(value)
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    # Pydantic models (message parts, usage) and anything else with a schema.
    dump = getattr(value, "model_dump", None)
    if callable(dump):
        return _jsonable(dump(mode="json"))
    return str(value)


#: Fields that differ between two calls that asked the same thing. A message part
#: carries a wall-clock timestamp, so hashing the raw request would make every
#: digest unique and useless for spotting a divergence. The digest is over what
#: was asked, not over when it was asked.
_VOLATILE_KEYS = frozenset({
    "timestamp", "run_id", "conversation_id", "provider_response_id",
    "provider_details", "provider_url", "message_id", "part_index",
})


def _stable(value: Any) -> Any:
    """Strip volatile fields so one request digests the same on every call."""
    if isinstance(value, dict):
        return {key: _stable(item) for key, item in value.items()
                if key not in _VOLATILE_KEYS}
    if isinstance(value, list):
        return [_stable(item) for item in value]
    return value


def _instructions(parameters: ModelRequestParameters | None) -> list[str]:
    """The agent's instructions, which are not part of ``messages``.

    PydanticAI passes ``instructions`` to the model outside the message list, so a
    recording that only kept messages would silently omit the system prompt — the
    largest single block of text an agent is given.
    """
    rendered: list[str] = []
    for part in getattr(parameters, "instruction_parts", None) or []:
        content = getattr(part, "content", None)
        if isinstance(content, str):
            rendered.append(content)
        elif content is not None:
            rendered.append(json.dumps(_jsonable(content), ensure_ascii=False))
    return rendered


def _serialize_messages(messages: Sequence[ModelMessage]) -> list[dict[str, Any]]:
    """Serialize messages with the library's own tagged-union adapter.

    Hand-rolling this would be a second format to keep in step with the library,
    and replay depends on reading the format back exactly.
    """
    return list(ModelMessagesTypeAdapter.dump_python(list(messages), mode="json"))


def _request_digest(messages: Sequence[ModelMessage],
                    parameters: ModelRequestParameters | None,
                    settings: ModelSettings | None) -> str:
    """What was asked, independent of when it was asked.

    Used when recording and when replaying, so a changed prompt is detected rather
    than assumed.
    """
    return hashlib.sha256(canonical_json({
        "instructions": _stable(_instructions(parameters)),
        "messages": _stable(_serialize_messages(messages)),
        "settings": _stable(_jsonable(settings)),
        "tools": _tool_names(parameters)}).encode("utf-8")).hexdigest()


def _serialize_response(response: Any) -> dict[str, Any]:
    """Serialize one response through the same adapter, as a single-element list."""
    dumped = ModelMessagesTypeAdapter.dump_python([response], mode="json")
    return dict(dumped[0])


def _deserialize_response(payload: dict[str, Any]) -> Any:
    """Rebuild a response, or raise if the payload cannot be read back.

    A part the tagged union cannot validate (an image, say) must fail loudly: a
    replay that silently returned something else would be worse than no replay.
    """
    restored = ModelMessagesTypeAdapter.validate_python([payload])
    return restored[0]


def _tool_names(parameters: ModelRequestParameters | None) -> list[str]:
    """The tools visible to this call, by name only.

    A tool's callable cannot be serialized and must not be: a recording shows what
    the model could choose from, not how a choice would be executed.
    """
    names: list[str] = []
    for attribute in ("function_tools", "output_tools", "native_tools"):
        for tool in getattr(parameters, attribute, None) or []:
            name = getattr(tool, "name", None)
            if isinstance(name, str):
                names.append(name)
    return sorted(set(names))


def _detect_secret(text: str, forbidden: Sequence[str]) -> str | None:
    """Name the reason this text must not be persisted, or ``None``."""
    for value in forbidden:
        if len(value) >= 8 and value in text:
            return "a configured secret value"
    for pattern in _SECRET_PATTERNS:
        if pattern.search(text):
            return f"text matching {pattern.pattern!r}"
    return None


class ModelRecorder:
    """Writes model calls as immutable projections.

    ``store`` is optional so the recorder can be used in tests and in the
    read-only paths without a state store; without one the projections are still
    written and only the attribution event is skipped.
    """

    def __init__(self, artifacts: ArtifactStore, *, mode: RecordingMode = RecordingMode.OFF,
                 forbidden: Sequence[str] = (), store: EventStore | None = None) -> None:
        self.artifacts = artifacts
        self.mode = mode
        self.store = store
        # Literal secret values that must never reach disk. The gateway resolves
        # them, so it is the only place that can supply them.
        self._forbidden = tuple(value for value in forbidden if value)
        self._sequences: dict[tuple[UUID, int], int] = {}
        #: References of the recordings written during this process, in order. Step
        #: two will read them back to replay; tests use them to inspect a prompt.
        self.written: list[str] = []
        self.refused = 0
        self.recorded = 0

    @property
    def enabled(self) -> bool:
        return self.mode is not RecordingMode.OFF

    def forbid(self, *values: str) -> None:
        """Add literal values that must never be persisted.

        Additive and idempotent: one recorder is shared by every model profile, and
        each gateway contributes the secret it resolved. Values never leave this
        object, and they are used only for comparison.
        """
        merged = dict.fromkeys(self._forbidden + tuple(value for value in values if value))
        self._forbidden = tuple(merged)

    def _next_sequence(self, context: CallContext) -> int:
        key = (context.node_run_id, context.attempt)
        sequence = self._sequences.get(key, 0)
        self._sequences[key] = sequence + 1
        return sequence

    def _emit(self, context: CallContext, event_type: str, payload: dict[str, Any],
              *, key: str) -> None:
        if self.store is None:
            return
        try:
            self.store.append_event(stream_id=context.run_id, event_type=event_type,
                                    payload=payload,
                                    idempotency_key=f"{event_type}:{context.node_run_id}:{key}")
        except Exception:  # noqa: BLE001 - a projection must not fail a run (I2)
            logger.exception("could not record %s for node %s", event_type, context.node_id)

    def record(self, *, messages: Sequence[ModelMessage], response: Any,
               settings: ModelSettings | None = None,
               parameters: ModelRequestParameters | None = None) -> str | None:
        """Persist one model call. Returns its artifact reference, or ``None``.

        ``None`` means the call was not recorded — recording is off, the call is
        outside a node attempt, or it was refused. It never means the run failed.
        """
        context = _active.get()
        if not self.enabled or context is None:
            return None
        sequence = self._next_sequence(context)
        try:
            payload = self._payload(context, sequence, messages, response, settings,
                                   parameters)
            text = json.dumps(payload, ensure_ascii=False, sort_keys=True)
        except Exception:  # noqa: BLE001 - a projection must not fail a run (I2)
            logger.exception("could not build a recording for node %s", context.node_id)
            return None

        found = _detect_secret(text, self._forbidden)
        if found is not None:
            # Refuse to persist rather than fail the run: the secret must not reach
            # disk, and a projection must not decide whether a run continues.
            self.refused += 1
            logger.error("refused to record model call for node %s: %s",
                         context.node_id, found)
            self._emit(context, "model.call_refused",
                       {"node_id": context.node_id, "attempt": context.attempt,
                        "sequence": sequence, "reason": found},
                       key=str(sequence))
            return None

        try:
            ref = self.artifacts.put_text(text, media_type="application/json")
        except Exception:  # noqa: BLE001 - a projection must not fail a run (I2)
            logger.exception("could not write recording for node %s", context.node_id)
            return None

        self.recorded += 1
        self.written.append(ref)
        self._emit(context, "model.call",
                   {"node_id": context.node_id, "attempt": context.attempt,
                    "sequence": sequence, "recording_ref": ref,
                    "request_digest": payload["request_digest"],
                    "messages": len(payload["messages"])},
                   key=str(sequence))
        return ref

    def _payload(self, context: CallContext, sequence: int,
                 messages: Sequence[ModelMessage], response: Any,
                 settings: ModelSettings | None,
                 parameters: ModelRequestParameters | None) -> dict[str, Any]:
        """Build the recorded document for one call.

        Serialization uses the library's own tagged-union adapter, which replay also
        reads, so there is one format rather than a hand-rolled second one kept in
        step by hand.
        """
        requests = _serialize_messages(messages)
        instructions = _instructions(parameters)
        tools = _tool_names(parameters)
        request_digest = hashlib.sha256(
            canonical_json({"instructions": _stable(instructions),
                            "messages": _stable(requests),
                            "settings": _stable(_jsonable(settings)),
                            "tools": tools}).encode("utf-8")).hexdigest()
        return {
            "format": RECORDING_FORMAT,
            "run_id": str(context.run_id),
            "node_id": context.node_id,
            "node_run_id": str(context.node_run_id),
            "attempt": context.attempt,
            "sequence": sequence,
            "model": getattr(response, "model_name", None),
            "provider": getattr(response, "provider_name", None),
            "response_id": getattr(response, "provider_response_id", None),
            "settings": _jsonable(settings),
            "tools": tools,
            # The digest explains a divergence during replay; it is deliberately
            # not the replay key, because the point is to change the prompt.
            "request_digest": request_digest,
            "instructions": instructions,
            "messages": requests,
            "response": _serialize_response(response),
        }


class RecordingModel(WrapperModel):
    """A model wrapper that records every call it forwards.

    Wrapping here rather than at the gateway is deliberate: see the module
    docstring. ``WrapperModel`` is PydanticAI's own extension point and is what
    all three ``durable_exec`` backends use.
    """

    def __init__(self, wrapped: Any, recorder: ModelRecorder) -> None:
        super().__init__(wrapped)
        self.recorder = recorder

    async def request(self, messages: list[ModelMessage],
                      model_settings: ModelSettings | None,
                      model_request_parameters: ModelRequestParameters) -> Any:
        response = await self.wrapped.request(messages, model_settings,
                                              model_request_parameters)
        self.recorder.record(messages=messages, response=response,
                             settings=model_settings, parameters=model_request_parameters)
        return response

    @asynccontextmanager
    async def request_stream(self, messages: list[ModelMessage],
                             model_settings: ModelSettings | None,
                             model_request_parameters: ModelRequestParameters,
                             run_context: Any | None = None) -> AsyncGenerator[Any]:
        async with self.wrapped.request_stream(messages, model_settings,
                                               model_request_parameters,
                                               run_context) as stream:
            yield stream
        # The consumer has finished, so the assembled response is available. A
        # failure here is logged and swallowed: the run already has its answer.
        try:
            # ``get()`` returns the assembled response once the stream has been
            # consumed; it is not a coroutine.
            response = stream.get()
        except Exception:  # noqa: BLE001 - recording never fails a run
            logger.exception("could not read a streamed response for recording")
            return
        self.recorder.record(messages=messages, response=response,
                             settings=model_settings, parameters=model_request_parameters)


def read_recording(artifacts: ArtifactStore, ref: str) -> dict[str, Any]:
    """Read a recording back, for inspection or replay.

    The messages are what the model was given; ``text_of`` renders them as the
    text a person can read.
    """
    payload = json.loads(artifacts.get_text(ref))
    if not isinstance(payload, dict) or payload.get("format") != RECORDING_FORMAT:
        raise ValueError(f"{ref} is not a recording this runtime understands")
    return payload


def text_of(recording: dict[str, Any]) -> str:
    """Render a recording's request as readable text.

    This is the answer to "what did the agent actually see?", which previously
    could only be reconstructed by hand. Instructions come first because that is
    where they sit in the request the model receives.
    """
    lines: list[str] = []
    for instruction in recording.get("instructions") or []:
        lines.append(f"=== instructions ===\n{instruction}")
    for message in recording.get("messages") or []:
        kind = message.get("kind") or message.get("message_kind") or "message"
        lines.append(f"=== {kind} ===")
        for part in message.get("parts") or []:
            part_kind = part.get("part_kind") or "part"
            content = part.get("content")
            if isinstance(content, str):
                lines.append(f"[{part_kind}]\n{content}")
            else:
                lines.append(f"[{part_kind}] {json.dumps(content, ensure_ascii=False)}")
    return "\n".join(lines)


