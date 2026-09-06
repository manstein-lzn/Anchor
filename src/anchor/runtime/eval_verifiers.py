"""pydantic-evals harness around Anchor verifier adapters.

Boundary: the online completion gate stays in ``verifier.py`` plus
``VerificationCheckpointSink`` with zero new dependencies. This module
evaluates the *same* adapter logic offline over datasets — adapter matrices
for regression today, promotion-review scoring for experience distillation
later. It lives behind the ``evals`` extra, not core dependencies.
"""

from __future__ import annotations

from pydantic_evals.evaluators import EvaluationReason, Evaluator, EvaluatorContext

from anchor.domain.conditions import ConditionError, evaluate_condition
from anchor.runtime.verifier import parse_model_verdict


def deterministic_verdict(inputs: dict) -> str:
    """Run Anchor's deterministic JMESPath adapter; return the verdict name."""
    try:
        passed = evaluate_condition(inputs["expression"], inputs["evidence"])
    except ConditionError:
        return "error"
    return "passed" if passed else "rejected"


def model_verdict(inputs: dict) -> str:
    """Run Anchor's strict-JSON verdict rule over model text."""
    return parse_model_verdict(inputs["model_text"])[0].value


class VerdictEquals(Evaluator):
    """Assert the adapter verdict equals the case's expected verdict."""

    def evaluate(self, ctx: EvaluatorContext) -> EvaluationReason:
        ok = ctx.output == ctx.expected_output
        return EvaluationReason(
            value=ok,
            reason=f"output={ctx.output!r} expected={ctx.expected_output!r}",
        )
