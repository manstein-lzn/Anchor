"""Deterministic planner for the first Anchor Context Engine slice."""

from __future__ import annotations

import hashlib
import json
from collections.abc import Callable

from .models import ContextPlan, ContextRequest, ContextSegment, ContextSource, ContextView
from .models import PromptParts, prompt_segments, render


TokenEstimator = Callable[[str], int]


def _digest(value: object) -> str:
    encoded = json.dumps(value, ensure_ascii=False, sort_keys=True,
                         separators=(",", ":"), default=str).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


from .models import Capacity


def _capacity(request: ContextRequest, text: str,
              estimate_tokens: TokenEstimator | None) -> tuple[int | None, Capacity]:
    estimated = estimate_tokens(text) if estimate_tokens else None
    if estimated is not None and (type(estimated) is not int or estimated < 0):
        raise ValueError("token estimator must return a nonnegative integer")
    if request.context_window is None or estimated is None:
        return estimated, "unknown"
    return estimated, "exceeded" if estimated + (request.output_token_reservation or 0) > request.context_window else "within_budget"


def _plan(request: ContextRequest, parts: PromptParts,
          *, estimate_tokens: TokenEstimator | None = None) -> ContextPlan:
    if (request.objective != parts.objective or request.node_name != parts.node_name
            or dict(request.declared_input) != dict(parts.snapshot)):
        raise ValueError("context request and prompt parts disagree")
    rendered = prompt_segments(parts, prefix=request.instructions)
    user_prompt = render(rendered)
    estimated, capacity = _capacity(request, request.instructions + user_prompt, estimate_tokens)
    context_segments = (
        ContextSegment("instructions", request.instructions, None, True, 0),
        ContextSegment("declared_input", rendered.declared, "declared-input", True, 1),
        ContextSegment("working_set", rendered.working_set, "working-set", False, 2),
    )
    payload = {"request": request, "segments": context_segments,
               "system_prompt": request.instructions, "user_prompt": user_prompt,
               "estimated_input_tokens": estimated,
               "reserved_output_tokens": request.output_token_reservation,
               "capacity": capacity}
    return ContextPlan(request=request, segments=context_segments, omissions=(),
                       system_prompt=request.instructions, user_prompt=user_prompt,
                       estimated_input_tokens=estimated,
                       reserved_output_tokens=request.output_token_reservation,
                       capacity=capacity,
                       sources=(ContextSource("declared-input", "declared_input", user_prompt, True),),
                       views=(ContextView("declared-input", "declared-input", user_prompt, True),),
                       plan_hash=_digest(payload))


def plan_node_context(request: ContextRequest,
                      *, run_memory: tuple[str, ...] = (),
                      promoted_memory: tuple[tuple[str, str], ...] = (),
                      estimate_tokens: TokenEstimator | None = None) -> ContextPlan:
    """Build the current node prompt without changing its existing rendering."""
    parts = PromptParts(
        objective=request.objective,
        node_name=request.node_name,
        snapshot=dict(request.declared_input),
        run_memory=run_memory,
        promoted_memory=promoted_memory,
    )
    return _plan(request, parts, estimate_tokens=estimate_tokens)


def plan_prompt(request: ContextRequest, parts: PromptParts,
                *, estimate_tokens: TokenEstimator | None = None) -> ContextPlan:
    """Plan an already-resolved Anchor prompt while preserving its exact shape."""
    return _plan(request, parts, estimate_tokens=estimate_tokens)
