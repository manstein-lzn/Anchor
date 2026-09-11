"""Serve recorded model calls back, so a run can be reproduced exactly.

Recording (``runtime/model_recording.py``) makes what a model was asked and what it
answered inspectable. Replay is the other half: the same run again, with the answers
held fixed, so that anything else under study — a context policy, a prompt, a tool
matrix — is the only thing that moved.

Three properties, from ADR-044:

* **Replay matches by position**, ``(node_run_id, attempt, sequence)``, never by a
  digest of the prompt. The whole point is to change the prompt: matching on it
  would make replay fail on exactly the change being studied. A prompt that differs
  is therefore *reported*, not rejected.
* **Divergence fails loudly and located.** A call with no recording behind it has
  nothing honest to answer with, so it raises and names the node, the attempt and
  the call index. Continuing silently would turn "the replay passed" into a signal
  worth nothing.
* **A replay never falls through to the live model.** Not for a missing recording,
  and not for a streaming call: either would mix recorded and live answers while
  reporting success.

What this cannot do is stated where it matters — in ``docs/RECORDING_AND_REPLAY.md``
and ADR-044: replay cannot A/B a context policy that changes the prompt, because a
changed prompt has to be answered by the model for real.
"""

from __future__ import annotations

import logging
from collections.abc import AsyncGenerator, Sequence
from contextlib import asynccontextmanager
from dataclasses import dataclass
from typing import Any
from uuid import UUID

from pydantic_ai.messages import ModelMessage
from pydantic_ai.models import ModelRequestParameters
from pydantic_ai.models.wrapper import WrapperModel
from pydantic_ai.settings import ModelSettings

from anchor.runtime.artifacts import ArtifactStore
from anchor.runtime.model_recording import (
    MAX_TRACKED_ATTEMPTS,
    CallContext,
    EventStore,
    _active,
    _deserialize_response,
    _request_digest,
    read_recording,
)


logger = logging.getLogger(__name__)

#: How many runs' plans one process keeps. A replay is a bounded activity, so this
#: only prevents unbounded growth in a process that replays many runs.
MAX_PLANNED_RUNS = 64


@dataclass(frozen=True)
class ReplayEntry:
    """One recorded call, at the position a replay will ask for it."""

    node_id: str
    node_run_id: str
    attempt: int
    sequence: int
    request_digest: str
    payload: dict[str, Any]


class ReplayDivergence(RuntimeError):
    """A replay could not continue, and says exactly where it stopped.

    Raised rather than continued. A replay that quietly served a different answer,
    or quietly called the real model, would report success while proving nothing —
    which is the failure mode this whole mechanism exists to avoid.
    """

    def __init__(self, message: str, *, node_id: str, attempt: int | None = None,
                 sequence: int | None = None) -> None:
        super().__init__(message)
        self.node_id = node_id
        self.attempt = attempt
        self.sequence = sequence


class ReplayPlan:
    """The recorded model calls of a run, addressed by position.

    Loading is lazy and per run, because one process serves every run and a plan is
    only meaningful for the run being replayed.
    """

    def __init__(self, artifacts: ArtifactStore, *, store: EventStore | None = None) -> None:
        self.artifacts = artifacts
        self.store = store
        self._by_run: dict[UUID, dict[tuple[str, int, int], ReplayEntry]] = {}
        self._loaded: set[UUID] = set()

    @classmethod
    def from_recordings(cls, artifacts: ArtifactStore, refs: Sequence[str]) -> ReplayPlan:
        """Build a plan directly from recordings, for tests and offline inspection."""
        plan = cls(artifacts)
        for ref in refs:
            payload = read_recording(artifacts, ref)
            plan._index(payload)
        return plan

    def _index(self, payload: dict[str, Any]) -> None:
        run_id = UUID(str(payload["run_id"]))
        entry = ReplayEntry(
            node_id=str(payload["node_id"]), node_run_id=str(payload["node_run_id"]),
            attempt=int(payload["attempt"]), sequence=int(payload["sequence"]),
            request_digest=str(payload["request_digest"]), payload=payload)
        if run_id not in self._by_run and len(self._by_run) >= MAX_PLANNED_RUNS:
            # A long-lived process replays a bounded number of runs; keeping every
            # plan it ever built would grow without limit. Evict the oldest.
            oldest = next(iter(self._by_run))
            self._by_run.pop(oldest, None)
            self._loaded.discard(oldest)
        self._by_run.setdefault(run_id, {})[(
            entry.node_run_id, entry.attempt, entry.sequence)] = entry

    def _calls(self, run_id: UUID) -> dict[tuple[str, int, int], ReplayEntry]:
        if run_id not in self._loaded:
            self._loaded.add(run_id)
            if self.store is not None:
                for event in self.store.list_events(run_id):
                    if event.get("event_type") != "model.call":
                        continue
                    ref = (event.get("payload") or {}).get("recording_ref")
                    if not isinstance(ref, str):
                        continue
                    try:
                        self._index(read_recording(self.artifacts, ref))
                    except (ValueError, OSError):
                        logger.exception("unreadable recording %s; it cannot be replayed",
                                         ref)
        return self._by_run.setdefault(run_id, {})

    def entry(self, context: CallContext, sequence: int) -> ReplayEntry | None:
        """The recorded call at this position, or ``None`` if there is none."""
        return self._calls(context.run_id).get(
            (str(context.node_run_id), context.attempt, sequence))

    def recorded(self, context: CallContext) -> int:
        """How many calls were recorded for this node attempt."""
        return sum(1 for (node_run_id, attempt, _) in self._calls(context.run_id)
                   if node_run_id == str(context.node_run_id) and attempt == context.attempt)


class ReplayModel(WrapperModel):
    """Serves recorded responses by position, and fails loudly on divergence.

    Position, not content: the point of the exercise is to change the prompt, so a
    request that differs from the recorded one is the expected case and is reported
    rather than rejected. What cannot be tolerated is asking for a call that was
    never recorded — there is nothing honest to answer with.
    """

    def __init__(self, wrapped: Any, plan: ReplayPlan, *,
                 store: EventStore | None = None) -> None:
        super().__init__(wrapped)
        self.plan = plan
        self.store = store
        self._positions: dict[tuple[UUID, int], int] = {}
        #: Positions where the prompt asked something different from the recording.
        self.changed: list[dict[str, Any]] = []
        self.replayed = 0

    def _emit(self, context: CallContext, payload: dict[str, Any], *, key: str) -> None:
        if self.store is None:
            return
        try:
            self.store.append_event(stream_id=context.run_id, event_type="model.call_replayed",
                                    payload=payload,
                                    idempotency_key=f"model.call_replayed:{context.node_run_id}:{key}")
        except Exception:  # noqa: BLE001 - reporting must not fail the replay itself
            logger.exception("could not emit model.call_replayed for %s", context.node_id)

    async def request(self, messages: list[ModelMessage],
                      model_settings: ModelSettings | None,
                      model_request_parameters: ModelRequestParameters) -> Any:
        context = _active.get()
        if context is None:
            raise ReplayDivergence(
                "a replayed model call happened outside a node attempt, so there is "
                "nothing to look it up by", node_id="?")
        key = (context.node_run_id, context.attempt)
        sequence = self._positions.get(key, 0)
        if key not in self._positions and len(self._positions) >= MAX_TRACKED_ATTEMPTS:
            self._positions.pop(next(iter(self._positions)), None)
        self._positions[key] = sequence + 1

        entry = self.plan.entry(context, sequence)
        if entry is None:
            raise ReplayDivergence(
                f"node {context.node_id!r} attempt {context.attempt} asked for model call "
                f"#{sequence}, but {self.plan.recorded(context)} call(s) were recorded "
                f"for that attempt", node_id=context.node_id, attempt=context.attempt,
                sequence=sequence)

        try:
            response = _deserialize_response(entry.payload["response"])
        except Exception as exc:
            raise ReplayDivergence(
                f"the recording of node {context.node_id!r} attempt {context.attempt} "
                f"call #{sequence} cannot be read back ({type(exc).__name__}); refusing "
                f"to substitute anything for it", node_id=context.node_id,
                attempt=context.attempt, sequence=sequence) from exc

        digest = _request_digest(messages, model_request_parameters, model_settings)
        changed = digest != entry.request_digest
        if changed:
            self.changed.append({"node_id": context.node_id, "attempt": context.attempt,
                                 "sequence": sequence,
                                 "recorded_digest": entry.request_digest,
                                 "actual_digest": digest})
        self.replayed += 1
        self._emit(context, {
            "node_id": context.node_id, "attempt": context.attempt, "sequence": sequence,
            "recording_ref": entry.payload.get("response_id"),
            "recorded_digest": entry.request_digest, "actual_digest": digest,
            "prompt_changed": changed}, key=str(sequence))
        return response

    @asynccontextmanager
    async def request_stream(self, messages: list[ModelMessage],
                             model_settings: ModelSettings | None,
                             model_request_parameters: ModelRequestParameters,
                             run_context: Any | None = None) -> AsyncGenerator[Any]:
        # Deliberately not implemented by falling through to the real model: during a
        # replay that would mix recorded and live answers without saying so.
        raise ReplayDivergence(
            "streaming calls cannot be replayed yet; run the replay against a "
            "non-streaming model profile", node_id="?")
        yield  # pragma: no cover - unreachable, keeps the signature a generator

    def summary(self) -> dict[str, Any]:
        """What this replay did, for a report."""
        return {"replayed": self.replayed, "prompt_changed": len(self.changed),
                "changed": self.changed}
