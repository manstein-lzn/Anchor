"""Versioned, side-effect-free condition evaluation for Graph edges."""

from __future__ import annotations

import json
from importlib.metadata import version
from typing import Any, Mapping

import jmespath
from jmespath.exceptions import JMESPathError

from .context import input_hash


EVALUATOR_NAME = "jmespath"
EVALUATOR_VERSION = version("jmespath")


class ConditionError(ValueError):
    pass


def validate_condition(expression: str) -> None:
    if not expression.strip():
        raise ConditionError("condition must not be blank")
    try:
        jmespath.compile(expression)
    except JMESPathError as exc:
        raise ConditionError(f"invalid JMESPath condition: {exc}") from exc


def evaluate_condition(expression: str, context: Mapping[str, Any]) -> bool:
    """Evaluate a condition and require an explicit JSON boolean result."""
    validate_condition(expression)
    try:
        result = jmespath.search(expression, dict(context))
    except JMESPathError as exc:
        raise ConditionError(f"JMESPath condition failed: {exc}") from exc
    if type(result) is not bool:
        raise ConditionError(
            f"condition must evaluate to a boolean, got {type(result).__name__}"
        )
    return result


def build_condition_context(
    output_text: str,
    input_snapshot: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Build the documented evaluator input from a model response and snapshot."""
    try:
        output: Any = json.loads(output_text)
    except json.JSONDecodeError:
        output = output_text
    context = {"output": output, "inputs": dict(input_snapshot or {})}
    # Reject non-JSON values before routing depends on data that cannot be
    # reproduced or hashed canonically.
    input_hash(context)
    return context
