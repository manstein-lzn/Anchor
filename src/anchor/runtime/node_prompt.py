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


def assemble_prompt(parts: PromptParts) -> str:
    """Build the prompt a node is executed with.

    The snapshot is serialized canonically so the same declared input always
    produces the same text, whatever order a store happened to return it in.
    """
    return (
        f"Task objective:\n{parts.objective}\n\n"
        f"Execute graph node: {parts.node_name}\n"
        f"\nDurable input snapshot:\n{canonical_json(parts.snapshot)}"
        f"\n\nRun memory:\n{run_memory_block(parts.run_memory)}"
        f"\n\nPromoted organizational knowledge:\n"
        f"{promoted_memory_block(parts.promoted_memory)}")
