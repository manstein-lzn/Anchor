from uuid import uuid4

from anchor.context_engine import ContextRequest, plan_node_context
from anchor.runtime.node_prompt import PromptParts, assemble_prompt


def test_context_plan_preserves_existing_prompt_bytes():
    request = ContextRequest(
        run_id=uuid4(), node_run_id=uuid4(), node_id="write", node_name="Write", attempt=1,
        model_ref="models.test", objective="Do the work.", instructions="Follow the node contract.",
        declared_input={"evidence": {"items": [1, 2]}},
    )
    plan = plan_node_context(request)
    expected = assemble_prompt(PromptParts(
        objective=request.objective, node_name=request.node_name, snapshot=request.declared_input))
    assert plan.user_prompt == expected
    assert plan.system_prompt == request.instructions
    assert plan.capacity == "unknown"
    assert len(plan.segments) == 3
    assert plan.sources[0].ref == "declared-input"
    assert plan.views[0].source_ref == "declared-input"


def test_context_plan_is_deterministic_and_hash_changes_with_input():
    base = dict(run_id=None, node_run_id=None, node_id="n", node_name="N", attempt=0,
                model_ref=None, objective="O", instructions="I", declared_input={"x": 1})
    first = plan_node_context(ContextRequest(**base))
    second = plan_node_context(ContextRequest(**base))
    changed = plan_node_context(ContextRequest(**{**base, "declared_input": {"x": 2}}))
    assert first.plan_hash == second.plan_hash
    assert first.plan_hash != changed.plan_hash


def test_context_plan_reports_capacity_without_changing_prompt():
    request = ContextRequest(
            run_id=None, node_run_id=None, node_id="n", node_name="N", attempt=0, model_ref=None,
        objective="O", instructions="I", declared_input={"x": "long"},
        output_token_reservation=10, context_window=1,
    )
    plan = plan_node_context(request, estimate_tokens=lambda text: 2)
    assert plan.capacity == "exceeded"
    assert '"x":"long"' in plan.user_prompt


def test_context_plan_rejects_disagreement_and_bad_estimator():
    request = ContextRequest(run_id=None, node_run_id=None, node_id="n", node_name="N",
                             attempt=0, model_ref=None, objective="O", instructions="I",
                             declared_input={"x": 1})
    import pytest
    with pytest.raises(ValueError, match="disagree"):
        from anchor.context_engine import PromptParts, plan_prompt
        plan_prompt(request, PromptParts(objective="other", node_name="N", snapshot={"x": 1}))
    with pytest.raises(ValueError, match="nonnegative"):
        plan_node_context(request, estimate_tokens=lambda _text: -1)

# -- capacity: the one judgement that guards the hard wall ---------------------------


def test_a_declared_window_makes_capacity_meaningful():
    """Without a window the plan can only say `unknown`, which is honest and useless.

    The provider rejects an over-long request outright, so the question has to be answered
    before the call rather than discovered from a 400 after the whole payload is uploaded.
    """
    request = ContextRequest(
        run_id=None, node_run_id=None, node_id="n", node_name="N", attempt=0, model_ref=None,
        objective="O", instructions="I", declared_input={"x": "y"},
        context_window=1000, output_token_reservation=200)
    assert plan_node_context(request, estimate_tokens=lambda _t: 700).capacity == "within_budget"
    # The reservation counts against the window, because the provider subtracts it too.
    assert plan_node_context(request, estimate_tokens=lambda _t: 801).capacity == "exceeded"


def test_an_undeclared_window_is_unknown_rather_than_assumed():
    """A default here would be a guessed budget, which this runtime refuses to invent."""
    request = ContextRequest(
        run_id=None, node_run_id=None, node_id="n", node_name="N", attempt=0, model_ref=None,
        objective="O", instructions="I", declared_input={"x": 1})
    for estimate in (0, 10, 10_000_000):
        assert plan_node_context(request, estimate_tokens=lambda _t, e=estimate: e) \
            .capacity == "unknown"


def test_the_estimator_leans_high_on_every_measurement_it_was_calibrated_from():
    """Under-estimating sends a request the provider rejects after the upload; over-estimating
    refuses one that would have fit. Only one of those is recoverable, so the constants must sit
    above every measured point rather than below the average of them.
    """
    from anchor.context_engine.estimator import estimate_tokens

    # Plain ASCII, measured: 1,000,000 -> 125,032 and 8,500,000 -> 1,095,300.
    assert estimate_tokens("x" * 1_000_000) > 125_032
    assert estimate_tokens("x" * 8_500_000) > 1_095_300
    # Our own prompts, measured at 11,668 characters -> 3,360 tokens and 17,950 -> 7,181.
    # Reproduce their character mix rather than the text: 14.7% and 36.1% CJK respectively.
    gather_like = "中" * int(11_668 * 0.147) + "x" * (11_668 - int(11_668 * 0.147))
    write_like = "中" * int(17_950 * 0.361) + "x" * (17_950 - int(17_950 * 0.361))
    assert estimate_tokens(gather_like) > 3_360
    assert estimate_tokens(write_like) > 7_181


def test_the_estimator_counts_cjk_higher_than_latin():
    """A single ratio would be wrong by up to threefold: Chinese is about a token per character
    while ASCII averages an eighth.
    """
    from anchor.context_engine.estimator import estimate_tokens

    assert estimate_tokens("中" * 1000) > estimate_tokens("x" * 1000) * 2


def test_the_plan_reports_its_reservation_and_estimate():
    request = ContextRequest(
        run_id=None, node_run_id=None, node_id="n", node_name="N", attempt=0, model_ref=None,
        objective="O", instructions="I", declared_input={"x": 1},
        context_window=10_000, output_token_reservation=4_000)
    plan = plan_node_context(request, estimate_tokens=lambda _t: 123)
    assert plan.estimated_input_tokens == 123
    assert plan.reserved_output_tokens == 4_000
    assert plan.capacity_scope == "initial_invocation"
