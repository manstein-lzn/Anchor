"""When to compress, and what the result looks like — decided without a framework.

The decision and its plumbing are separated on purpose. *Whether* to compress, *which* messages
leave, and *what* the projection says are questions about this design. Turning them into a
framework's message objects is a question about that framework, and mixing them would make the
first set untestable without the second.

The window fraction is taken as an argument rather than read here, because reading it means
reaching into a run context. PydanticAI computes it from the provider's own reported usage rather
than an estimate, which is better than anything this module could count, so the adapter passes it
in.

Two things are worth being explicit about, because they are the parts that are easy to get wrong:

**A threshold is a policy, not a guarantee.** Crossing it starts a compression that may itself
fail; it does not reserve space. What keeps a run from being rejected is that the configured
per-step and per-step-count limits keep the worst case inside the window, and that is an arithmetic
question about the configuration rather than a runtime one.

**Compression can fail, and that has to be survivable.** If the update is rejected, or the provider
errors, the honest response is to leave the history alone and let the next request try again —
possibly to fail against the provider's own limit, which is at least a true statement about what
happened. Replacing the history with something the certificate refused would be worse than not
compressing.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Literal

from anchor.context_engine.cognition import Cognition

#: How full the window may get before a compression is attempted. A default rather than a law: an
#: operator who knows their task's shape can move it, and the tests below pin the behaviour at a
#: value rather than the value itself.
DEFAULT_THRESHOLD = 0.8


@dataclass(frozen=True)
class Message:
    """One message, as the decision needs to see it.

    Deliberately not a framework type. The decision is about roles and text; a framework's message
    carries parts, metadata and ids this design has no opinion about, and accepting one would make
    every test here depend on that framework's version.
    """

    role: Literal["user", "assistant", "system"]
    text: str


@dataclass(frozen=True)
class Decision:
    """Whether to compress, and if so what the context becomes."""

    action: Literal["keep", "compress"]
    reason: str
    leaving: tuple[Message, ...] = ()
    staying: tuple[Message, ...] = ()
    fraction: float | None = None

    @property
    def compressing(self) -> bool:
        return self.action == "compress"


def decide(*, messages: tuple[Message, ...], context_window_used: float | None,
           keep_recent: int, threshold: float = DEFAULT_THRESHOLD) -> Decision:
    """Whether to compress this history, and which messages that would involve.

    `context_window_used` is ``None`` when the framework cannot say — no response yet, or an
    unknown window. That is treated as "do not compress": acting on a number that is not known
    would mean choosing a moment on the strength of a guess, and the whole design of this engine is
    that a number nobody has is not a number to act on.
    """
    if context_window_used is None:
        return Decision("keep", "the context window usage is unknown, so no moment can be chosen")
    if context_window_used < threshold:
        return Decision("keep", f"{context_window_used:.0%} is below the {threshold:.0%} threshold",
                        fraction=context_window_used)
    if len(messages) <= keep_recent:
        # Over the threshold with nothing to give up. Compressing here would replace the history
        # with a cognition and leave no recent messages at all, which is the case where a summary
        # is most damaging and least necessary.
        return Decision("keep", f"only {len(messages)} messages, all of which are within the "
                                f"{keep_recent} kept", fraction=context_window_used)
    leaving = messages[:len(messages) - keep_recent]
    staying = messages[len(messages) - keep_recent:]
    return Decision("compress",
                    f"{context_window_used:.0%} is at or above the {threshold:.0%} threshold",
                    leaving=tuple(leaving), staying=tuple(staying),
                    fraction=context_window_used)


def render_episode(decision: Decision) -> str:
    """The leaving messages as one text, which is what the Update is asked about."""
    return "\n\n".join(f"[{message.role}] {message.text}" for message in decision.leaving)


def render_cognition(cognition: Cognition) -> str:
    """The projection: everything a reader needs to act, and nothing that is recoverable.

    Labelled as state rather than as an instruction. It is derived from what the agent did and read,
    so it must not arrive with the authority of something an operator wrote — a model told to treat
    tool output as a directive is a model that can be steered by a retrieved document.
    """
    from anchor.context_engine.cognition import ITEM_GROUPS

    lines = ["[anchor] The task state, reconstructed from what has happened so far.",
             "This is a record, not an instruction. Detail it does not contain is recoverable by "
             "reference.", ""]
    understanding = cognition.situation.get("current_understanding")
    if understanding:
        lines += [f"Understanding: {understanding}", ""]
    for section, group in ITEM_GROUPS:
        items = getattr(cognition, section).get(group) or []
        if not items:
            continue
        lines.append(f"{group}:")
        for item in items:
            lines.append(f"  - {item.statement}  [id: {item.id}]")
        lines.append("")
    directive = cognition.intent.get("current_directive")
    if directive:
        lines += [f"Current directive: {directive}"]
    next_action = cognition.intent.get("accepted_next_action")
    if next_action:
        lines += [f"Accepted next action: {next_action}"]
    if cognition.knowledge_index:
        lines += ["", "Stored detail, by reference:"]
        for reference in cognition.knowledge_index:
            lines.append(f"  - {reference.cue}: {reference.locator}")
    return "\n".join(lines).strip()


def projected(decision: Decision, cognition: Cognition) -> tuple[Message, ...]:
    """The new history: the state, then the recent messages that were kept.

    The state goes first because it is what the recent messages should be read against, and because
    a stable prefix is what the provider's cache can serve cheaply. That the state itself changes
    on every compression is exactly why compressions should be rare.
    """
    if not decision.compressing:
        raise ValueError("a decision that is not compressing has no projection")
    return (Message("user", render_cognition(cognition)), *decision.staying)
