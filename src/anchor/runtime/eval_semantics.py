"""Semantic drift tripwire, offline scoring only (baseline).

The mechanical gate (`integrity.py`) proves references are intact; this
module scores whether outputs still *address* the pinned success criteria.
v1 is a deterministic keyword-coverage signal — deliberately weak, honestly
labeled. It establishes the evaluator interface and regression datasets so
an LLM judge (`llm_as_a_judge` with the model gateway) can replace the
scoring function later without changing callers.
"""

from __future__ import annotations

import re

from pydantic_evals.evaluators import EvaluationReason, Evaluator, EvaluatorContext


_WORD = re.compile(r"[a-z0-9]+")
_STOPWORDS = frozenset({
    "the", "a", "an", "and", "or", "of", "to", "in", "is", "are", "be", "for",
    "with", "on", "by", "it", "as", "at", "that", "this", "must", "only",
    "exactly", "word", "nothing", "else", "return", "output",
})


def content_words(text: str) -> set[str]:
    return set(_WORD.findall(text.lower())) - _STOPWORDS


def coverage_task(inputs: dict) -> dict:
    """Score criteria coverage of an output text. Pure and deterministic."""
    criteria: list[str] = inputs.get("criteria", [])
    output: str = inputs.get("output", "")
    available = content_words(output)
    uncovered = [criterion for criterion in criteria
                 if not (content_words(criterion) & available)]
    score = (len(criteria) - len(uncovered)) / len(criteria) if criteria else 1.0
    return {"score": score, "uncovered": uncovered}


class CriteriaCoverage(Evaluator):
    """Assert output coverage of success criteria meets the case threshold."""

    def evaluate(self, ctx: EvaluatorContext) -> EvaluationReason:
        result = coverage_task(ctx.inputs)
        threshold = (ctx.metadata or {}).get("threshold", 1.0)
        ok = result["score"] >= threshold
        return EvaluationReason(
            value=ok,
            reason=f"coverage={result['score']:.2f} uncovered={result['uncovered']}",
        )
