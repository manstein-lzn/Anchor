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
from dataclasses import dataclass
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
# Imported at runtime, not under TYPE_CHECKING: this module uses `from __future__ import
# annotations`, and Pydantic AI resolves the signature at runtime to decide whether to pass a run
# context. A string annotation whose name is not importable is not a hint to it — it is an error,
# and the failure mode without the import is the processor being called with the message list where
# its context should be.
from pydantic_ai import RunContext

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

    async def __call__(self, ctx: "RunContext[Any]", messages: list[Any]) -> list[Any]:
        """The `ProcessHistory` processor. Returns what the next model request should send.

        The first parameter is annotated `RunContext` and not merely documented as one: the
        framework decides whether to pass a context by inspecting that annotation, and a second
        parameter named `messages` is no help if the first says `Any`. Getting it wrong is silent
        until the first request of a real run, where it surfaces as the processor receiving the
        message list as its context and the message list being missing.
        """
        ours = tuple(to_message(item) for item in messages)
        decision = decide(messages=ours, context_window_used=_window_fraction(ctx),
                          keep_recent=self.keep_recent, threshold=self.threshold)
        if not decision.compressing:
            # Logged because the fraction is otherwise invisible, and a threshold that can never be
            # reached looks exactly like one that is working: nothing happens either way.
            logger.info("no compression for %s: %s", self.node_id, decision.reason)
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
        result = [from_message(item) for item in projected(decision, cognition)]
        # Logged because nothing durable records a compression otherwise: the call goes through a
        # different gateway path than the one the worker reports usage for, and the counter is in
        # memory. An operator asking "did it compress, and did the history get shorter" would
        # otherwise have no answer.
        logger.info(
            "compressed %s: %d messages -> %d, %d characters folded away, %d items carried",
            self.node_id, len(messages), len(result),
            sum(len(m.text) for m in decision.leaving), len(cognition.item_ids()))
        return result

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
    """Give every agent this gateway builds a `ProcessHistory` capability bound to ``compactor``.

    Sets the gateway's capability list rather than replacing its tool-less agent, because
    `generate_with_tools` builds its own agent and a node with tools is exactly the one whose
    history grows unboundedly. Replacing only the tool-less agent would have looked like it worked
    and compressed nothing that mattered.

    Called per attempt, with a fresh compactor: the cognition belongs to one attempt and must not
    leak into the next, which starts from the node's declared input rather than from a history.
    """
    from pydantic_ai.capabilities import ProcessHistory

    gateway._capabilities = [ProcessHistory(compactor)]


@dataclass(frozen=True)
class CompactionSettings:
    """Whether to compress, and how. Absence of this object means no compression at all.

    Off unless configured, for the same reason the content cache is: a compression spends a model
    call and changes what the agent sees, and a behaviour nobody chose is a behaviour nobody can
    account for. The worker builds one of these only when the operator has asked for it.
    """

    keep_recent: int = DEFAULT_KEEP_RECENT
    threshold: float = DEFAULT_THRESHOLD

    def safe_threshold(self, *, window: int, reservation: int) -> float:
        """The highest fraction at which compressing can still happen before the provider refuses.

        The reservation counts against the window — the provider subtracts it too — so the largest
        input that will be accepted is ``window - reservation``. A threshold above that fraction is
        not merely late: it is unreachable, because the request that would have crossed it is
        rejected first. A default of 0.8 is fine against a half-million-token window and wrong
        against a hundred-and-thirty-thousand one, so the number has to be derived rather than
        chosen.
        """
        if window <= 0:
            raise ValueError("window must be positive")
        if reservation < 0 or reservation >= window:
            raise ValueError("the output reservation must be smaller than the window")
        return (window - reservation) / window

    def install(self, gateway: Any, *, run_id: Any, node_id: str) -> "Compactor":
        compactor = Compactor(gateway, run_id=run_id, node_id=node_id,
                              keep_recent=self.keep_recent, threshold=self.threshold)
        install(gateway, compactor)
        return compactor


