"""Anchor's graph-aware, deterministic node invocation context boundary.

The first version plans and describes context; PydanticAI remains responsible
for the model/tool loop, and the Anchor runtime remains responsible for commit
and recovery.
"""

from .models import (ContextPlan, ContextRequest, ContextSegment, ContextSource, ContextView, EMPTY_BLOCK, Omission,
                     PromptParts, PromptSegments, assemble_prompt, prompt_segments, render)
from .planner import plan_node_context, plan_prompt

__all__ = [
    "ContextPlan",
    "ContextRequest",
    "ContextSegment",
    "ContextSource",
    "ContextView",
    "Omission",
    "EMPTY_BLOCK", "PromptParts", "PromptSegments", "assemble_prompt", "prompt_segments", "render",
    "plan_node_context",
    "plan_prompt",
]
