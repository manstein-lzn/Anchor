"""Adapter contract for verifier logic, executed through pydantic-evals.

The online gate (typed claim, evidence record, completion semantics) is
covered by tests/test_verifier.py. This suite pins the *adapter rules*
themselves — deterministic evaluation and strict-JSON parsing — as an evals
Dataset so future adapter changes are scored, not just asserted.
"""

import pytest

pydantic_evals = pytest.importorskip("pydantic_evals")
from pydantic_evals import Case, Dataset

from anchor.runtime.eval_verifiers import VerdictEquals, deterministic_verdict, model_verdict


def run(dataset: Dataset, task) -> None:
    report = dataset.evaluate_sync(task, progress=False)
    failures = [
        f"{case.name}: {name}={result.value} ({result.reason})"
        for case in report.cases
        for name, result in case.assertions.items()
        if result.value is not True
    ]
    assert not failures, "\n".join(failures)
    assert len(report.cases) == len(dataset.cases)


def test_deterministic_adapter_matrix():
    dataset = Dataset(
        name="anchor-deterministic-adapter",
        cases=[
            Case(name="true-condition-passes",
                 inputs={"expression": "output.approved", "evidence": {"output": {"approved": True}}},
                 expected_output="passed", evaluators=(VerdictEquals(),)),
            Case(name="false-condition-rejects",
                 inputs={"expression": "output.approved", "evidence": {"output": {"approved": False}}},
                 expected_output="rejected", evaluators=(VerdictEquals(),)),
            Case(name="invalid-expression-errors",
                 inputs={"expression": "output..broken", "evidence": {"output": {}}},
                 expected_output="error", evaluators=(VerdictEquals(),)),
        ],
    )
    run(dataset, deterministic_verdict)


def test_model_adapter_matrix():
    dataset = Dataset(
        name="anchor-model-adapter",
        cases=[
            Case(name="strict-json-passes",
                 inputs={"model_text": '{"verdict": "passed", "reason": "artifact is exactly OK"}'},
                 expected_output="passed", evaluators=(VerdictEquals(),)),
            Case(name="prose-becomes-error",
                 inputs={"model_text": "looks good, I approve this result"},
                 expected_output="error", evaluators=(VerdictEquals(),)),
            Case(name="schema-violation-becomes-error",
                 inputs={"model_text": '{"verdict": "maybe", "reason": "unsure"}'},
                 expected_output="error", evaluators=(VerdictEquals(),)),
            Case(name="missing-reason-becomes-error",
                 inputs={"model_text": '{"verdict": "passed", "reason": ""}'},
                 expected_output="error", evaluators=(VerdictEquals(),)),
        ],
    )
    run(dataset, model_verdict)


def test_criteria_coverage_tripwire_matrix():
    from pydantic_evals import Case, Dataset
    from anchor.runtime.eval_semantics import CriteriaCoverage, coverage_task

    dataset = Dataset(
        name="anchor-coverage-baseline",
        cases=[
            Case(name="covered",
                 inputs={"criteria": ["output mentions artifact hashes"],
                         "output": "verified artifact hashes match"},
                 expected_output=True, evaluators=(CriteriaCoverage(),)),
            Case(name="uncovered",
                 inputs={"criteria": ["output mentions artifact hashes"],
                         "output": "something entirely different here"},
                 expected_output=False, evaluators=(CriteriaCoverage(),)),
            Case(name="empty-output",
                 inputs={"criteria": ["output mentions hashes"], "output": ""},
                 expected_output=False, evaluators=(CriteriaCoverage(),)),
        ],
    )

    def covered(inputs):
        return coverage_task(inputs)["score"] >= 1.0

    report = dataset.evaluate_sync(covered, progress=False)
    failures = [case.name for case in report.cases
                for result in case.assertions.values()
                if result.value != case.expected_output]
    assert not failures, failures
    assert len(report.cases) == 3
