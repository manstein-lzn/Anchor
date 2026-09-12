"""What the prompt was made of, and what that costs.

The counter that looks alarming is not the bill. A tool loop re-sends its conversation on
every call, so its gross prompt count grows while the provider serves most of it from a
prefix cache — and on our provider a cache hit costs 1/50th of a miss, so *which* part of the
prompt moves decides the price far more than how large the prompt is.

Two things are checked here, because both are easy to get wrong in a way that reads as good
news: that a segment which never changed is reported as stable, and that the split of the
gross count into new and re-sent content actually adds up rather than merely looking plausible.
"""

from __future__ import annotations

from anchor.api.routes_usage import _prompt_shape


def call(*, input_tokens: int, prefix="p", declared="d", working_set="w",
         segments_recorded=True) -> dict:
    return {"attempt": 0, "input_tokens": input_tokens, "cached_tokens": 0,
            "output_tokens": 0, "prefix_hash": prefix, "declared_hash": declared,
            "working_set_hash": working_set, "prompt_chars": 0,
            "segments_recorded": segments_recorded}


def test_a_segment_that_never_changed_is_reported_stable():
    shape = _prompt_shape([call(input_tokens=100), call(input_tokens=140),
                           call(input_tokens=180)])["prompt_shape"]
    assert shape["prefix_stable"] is True
    assert shape["declared_stable"] is True
    assert shape["working_set_stable"] is True
    assert shape["prefix_hashes"] == 1


def test_a_segment_that_changed_on_every_call_is_reported_unstable():
    """The property that decides whether the provider's cache could work at all."""
    shape = _prompt_shape([call(input_tokens=100, prefix="a"),
                           call(input_tokens=140, prefix="b"),
                           call(input_tokens=180, prefix="c")])["prompt_shape"]
    assert shape["prefix_stable"] is False
    assert shape["prefix_hashes"] == 3
    assert shape["declared_stable"] is True, "only the prefix moved"


def test_the_new_and_resent_split_adds_up_to_the_gross():
    """Each call re-sends every earlier prompt, so resent + new is the gross count exactly.

    An approximation here would understate the cost of a large working set, which is the
    number a context policy is trying to reduce.
    """
    tokens = [100, 140, 190]
    shape = _prompt_shape([call(input_tokens=n) for n in tokens])["prompt_shape"]
    assert shape["new_input_tokens"] + shape["resent_input_tokens"] == sum(tokens)
    # Call 1 is all new; calls 2 and 3 re-send 100 and 140 respectively.
    assert shape["resent_input_tokens"] == 100 + 140
    assert shape["new_input_tokens"] == 190


def test_a_single_call_has_nothing_resent():
    shape = _prompt_shape([call(input_tokens=100)])["prompt_shape"]
    assert shape["resent_input_tokens"] == 0
    assert shape["new_input_tokens"] == 100


def test_a_shrinking_prompt_does_not_produce_a_negative_split():
    """A prompt can shrink — memory is trimmed, a snapshot is corrected. The decomposition
    must stay an accounting of what was sent, not a claim that growth is monotonic."""
    shape = _prompt_shape([call(input_tokens=200), call(input_tokens=150)])["prompt_shape"]
    assert shape["new_input_tokens"] == 200, "everything in the first call is new"
    assert shape["resent_input_tokens"] == 150, "the second call sent nothing that was new"
    assert shape["new_input_tokens"] + shape["resent_input_tokens"] == 350, \
        "the split must still account for every token sent"
    assert shape["new_input_tokens"] >= 0 and shape["resent_input_tokens"] >= 0


def test_absent_segments_are_reported_rather_than_implied():
    """A caller that did not supply segments must not look like a prompt with no parts."""
    shape = _prompt_shape([call(input_tokens=100, prefix=None, declared=None,
                                working_set=None, segments_recorded=False)])["prompt_shape"]
    assert shape["segments_recorded"] is False
    assert shape["prefix_hashes"] == 0, "no hash was recorded, so no claim is made"
    assert shape["prefix_stable"] is True


def test_no_calls_reports_nothing_rather_than_zeroes():
    assert _prompt_shape([])["prompt_shape"] is None
