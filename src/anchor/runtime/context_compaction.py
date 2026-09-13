"""The PydanticAI side of compression: a `ProcessHistory` processor bound to one node attempt.

Everything about *what* to compress and *what the projection says* lives in
`context_engine/compaction.py`, which knows nothing about a framework. This file is the adapter,
and it is deliberately thin: it reads the window fraction the framework already computed, converts
messages to and from the shapes each side wants, and holds the cognition for the length of the
attempt.

Two properties worth stating because they are the reasons this shape was chosen.

**The state is per attempt and in memory.** The archived project's Checkpoint was authoritative and
survived a restart, because Pi's session survived with it. A tool loop's messages do not, so a
cognition built here refers to a history that only exists while the attempt does. That is
acceptable — it is a projection, and a retry starts from the node's declared input — but it is a
different thing from the original, and calling it a Checkpoint would overstate it.

**A failed compression leaves the history alone.** If the update is rejected or the provider
errors, the processor returns the messages unchanged and the next request tries again, possibly
failing against the provider's own limit. That is a true statement about what happened. Replacing a
history with a state the certificate refused would not be.
"""

from __future__ import annotations

import logging
from typing import Any, Literal

from anchor.context_engine.cognition import Cognition
from anchor.context_engine.compaction import (
    DEFAULT_THRESHOLD,
    Message,
    decide,
    projected,
    render_episode,
)
from anchor.context_engine.update import (
    BOOTSTRAP_SYSTEM,
    Episode,
    UpdateRejected,
    run_update,
)

logger = logging.getLogger("anchor.context_compaction")

#: How many complete messages survive a compression. Complete, not partial: a tool call and its
#: result are one unit, and cutting between them leaves a history the provider rejects outright.
DEFAULT_KEEP_RECENT = 6


class Compactor:
    """A compression policy bound to one node attempt.

    Mutable because the cognition has to survive between model calls within an attempt — that is
    the whole point of compressing once and then reusing the result. It is not thread-safe and does
    not need to be: one attempt runs on one task.
    """

    def __init__(self, gateway: Any, *, run_id: Any = "", node_id: str = "",
                 keep_recent: int = DEFAULT_KEEP_RECENT,
                 threshold: float = DEFAULT_THRESHOLD) -> None:
        if keep_recent < 1:
            raise ValueError("keep_recent must be at least one message")
        if not 0 < threshold <= 1:
            raise ValueError("threshold must be a fraction greater than zero and at most one")
        self.gateway = gateway
        self.run_id = run_id
        self.node_id = node_id
        self.keep_recent = keep_recent
        self.threshold = threshold
        self.cognition: Cognition | None = None
        #: Counted so a report can say whether compression ever ran, rather than inferring it from
        #: the absence of long histories.
        self.compressions = 0
        self.failures = 0

    async def __call__(self, ctx: Any, messages: list[Any]) -> list[Any]:
        """The `ProcessHistory` processor. Returns what the next model request should send."""
        ours = tuple(to_message(item) for item in messages)
        decision = decide(messages=ours, context_window_used=_window_fraction(ctx),
                          keep_recent=self.keep_recent, threshold=self.threshold)
        if not decision.compressing:
            return messages
        try:
            cognition = await self._compress(decision)
        except UpdateRejected as exc:
            self.failures += 1
            # The engine built a state its own certificate refuses. Compressing anyway would mean
            # sending a history nobody could account for, which is worse than a long one.
            logger.warning("compression rejected, keeping the full history: %s", exc)
            return messages
        except Exception as exc:  # noqa: BLE001 - a failed compression is not a failed node
            self.failures += 1
            logger.warning("compression failed, keeping the full history: %s: %s",
                           type(exc).__name__, exc)
            return messages
        self.cognition = cognition
        self.compressions += 1
        return [from_message(item) for item in projected(decision, cognition)]

    async def _compress(self, decision: Any) -> Cognition:
        first = self.cognition is None
        episode = Episode(leaving=render_episode(decision),
                          source=f"run:{self.run_id}" if self.run_id else "")
        outcome = await run_update(self.gateway, self.cognition or _empty(), episode,
                                   run_id=str(self.run_id), node_id=self.node_id,
                                   system_prompt=BOOTSTRAP_SYSTEM if first else None)
        return outcome.materialized.cognition


def _empty() -> Cognition:
    """The state a first compression starts from: nothing known, nothing to account for."""
    return Cognition(situation={"confirmed_facts": [], "active_hypotheses": [],
                                "unresolved_conflicts": [], "blockers": []},
                     experience={"decisions": [], "failed_paths": []},
                     intent={"open_questions": []})


def _window_fraction(ctx: Any) -> float | None:
    """What the framework reports, or ``None`` when it cannot say.

    PydanticAI computes this from the provider's own reported token counts rather than an estimate,
    which is better than counting characters here. `None` means no response yet or an unknown
    window, and the decision treats it as "do not compress" rather than as zero.
    """
    fraction = getattr(ctx, "context_window_used", None)
    if fraction is None:
        return None
    try:
        value = float(fraction)
    except (TypeError, ValueError):
        return None
    return value if value >= 0 else None


def to_message(raw: Any) -> Message:
    """One framework message as the decision sees it.

    Joins the parts into text. The decision is about roles and content; part kinds, tool call ids
    and metadata are things this design has no opinion about, and reading them here would make every
    test of the decision depend on the framework's message model.
    """
    # A literal rather than a plain string, because the decision's `Message` says what the three
    # roles are and a fourth would be a category the design has no behaviour for.
    role: Literal["user", "assistant", "system"] = (
        "user" if type(raw).__name__ == "ModelRequest" else "assistant")
    parts = getattr(raw, "parts", ()) or ()
    pieces: list[str] = []
    for part in parts:
        content = getattr(part, "content", None)
        if isinstance(content, str):
            pieces.append(content)
        elif content is not None:
            pieces.append(str(content))
    return Message(role, "\n".join(pieces))


def from_message(message: Message) -> Any:
    """The decision's message as a framework one.

    A user message, not a system part, even though it is a statement about the task. It is derived
    from what the agent did and read, and a system prompt carries an operator's authority: giving
    that authority to content the agent itself produced is how a retrieved document becomes a
    directive.
    """
    from pydantic_ai.messages import ModelRequest, UserPromptPart

    return ModelRequest(parts=[UserPromptPart(content=message.text)])


def install(gateway: Any, compactor: Any) -> None:
    """Give a gateway's agent a `ProcessHistory` capability bound to ``compactor``.

    Rebuilt rather than mutated: the agent is constructed once in `build_model_gateway`, and adding
    a capability afterwards is not supported by the framework. The agent is replaced, so a gateway
    that already made a call should be given the compactor before it is used.
    """
    from pydantic_ai import Agent as PydanticAgent
    from pydantic_ai.capabilities import ProcessHistory

    gateway._agent = PydanticAgent(gateway._model, output_type=str,
                                   model_settings=gateway._settings(),
                                   capabilities=[ProcessHistory(compactor)])
