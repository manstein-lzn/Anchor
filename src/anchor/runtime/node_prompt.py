"""Assemble a node's prompt in exactly one place.

The worker builds every agent prompt here, and so does the node harness. That
sharing is the whole point: a harness that assembled its own prompt would measure a
different one from the prompt under study, and every experiment it ran would be
about nothing. A single function makes "the harness measures the real prompt" a
property of the code rather than a promise.

The parts are explicit because they are the surface a context policy acts on:

- the task objective and the node's name, which frame the work;
- the durable input snapshot, which is the declared input and nothing else (I8);
- run memory, which is what this run has learned;
- promoted organizational knowledge, which is what earlier runs had approved.

Copying the rendering rules matters as much as the sharing: an empty memory list
must still print ``(none)``, because a prompt that silently drops the block is a
different prompt, not a shorter one.
"""

from __future__ import annotations

import hashlib
from collections.abc import Sequence
from dataclasses import dataclass
from typing import Any

from anchor.domain.context import canonical_json


#: Printed in place of an empty memory block. A block that vanishes when it has
#: nothing to say makes the prompt change shape between runs, which is precisely the
#: kind of drift a context policy is supposed to be measured against.
EMPTY_BLOCK = "(none)"


@dataclass(frozen=True)
class PromptParts:
    """Everything a node's prompt is built from."""

    objective: str
    node_name: str
    snapshot: dict[str, Any]
    run_memory: Sequence[str] = ()
    promoted_memory: Sequence[tuple[str, str]] = ()


def run_memory_block(parts: Sequence[str]) -> str:
    """Render this run's memories, or ``(none)``."""
    return "\n".join(f"- {item}" for item in parts) or EMPTY_BLOCK


def promoted_memory_block(parts: Sequence[tuple[str, str]]) -> str:
    """Render promoted knowledge, tagged with its domain, or ``(none)``."""
    return "\n".join(f"- [{domain or 'general'}] {content}"
                     for domain, content in parts) or EMPTY_BLOCK


@dataclass(frozen=True)
class PromptSegments:
    """A node's prompt split into the parts a context policy can act on.

    The split is what makes the cost report answerable. A stable prefix is served from the
    provider's cache and is nearly free; anything that changes invalidates it and is billed
    at the full rate, which on our provider is fifty times the cached price. Token counts
    alone cannot say which segment moved, so each is hashed separately and compared between
    calls.

    - ``prefix`` is the agent's instructions, sent as the system prompt. It is not part of
      the user prompt, which is why it is supplied rather than derived.
    - ``declared`` is the task frame and the input snapshot: what the graph declared (I8).
    - ``working_set`` is run memory and promoted knowledge: the projection.
    """

    prefix: str
    declared: str
    working_set: str

    def hashes(self) -> dict[str, str]:
        return {"prefix_hash": _digest(self.prefix),
                "declared_hash": _digest(self.declared),
                "working_set_hash": _digest(self.working_set)}


def _digest(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def prompt_segments(parts: PromptParts, *, prefix: str = "") -> PromptSegments:
    """Split the prompt where a policy can act, then join it in one place.

    ``assemble_prompt`` is defined in terms of this, so the segments and the prompt cannot
    drift: a policy that measures a segment is measuring the text that was actually sent.
    """
    return PromptSegments(
        prefix=prefix,
        declared=(f"Task objective:\n{parts.objective}\n\n"
                  f"Execute graph node: {parts.node_name}\n"
                  f"\nDurable input snapshot:\n{canonical_json(parts.snapshot)}"),
        working_set=(f"\n\nRun memory:\n{run_memory_block(parts.run_memory)}"
                     f"\n\nPromoted organizational knowledge:\n"
                     f"{promoted_memory_block(parts.promoted_memory)}"))


def render(segments: PromptSegments) -> str:
    """The *user* prompt as sent: the declared input, then the projection.

    The prefix is deliberately not included. It travels as the system prompt, and folding it
    in here would change the prompt every existing caller sends.
    """
    return f"{segments.declared}{segments.working_set}"


def assemble_prompt(parts: PromptParts) -> str:
    """Build the prompt a node is executed with.

    The snapshot is serialized canonically so the same declared input always
    produces the same text, whatever order a store happened to return it in.
    """
    return render(prompt_segments(parts))
