"""The extraction of a JSON object from what a model actually writes.

Every case here is a shape a real model produces. The reason they are worth listing individually is
that the extractor previously took the text from each opening brace *to the end of the message*, so
it succeeded only when nothing followed the object. A model that signed off with "Let me know if you
need anything else" failed the node permanently, identically on every retry, and — because the output
was not retained — left nothing that recorded what it had said.
"""

from __future__ import annotations

import pytest

from anchor.runtime.json_output import extract_json_object, normalise_json_text


@pytest.mark.parametrize("name,text", [
    ("bare", '{"a": 1}'),
    ("a sentence first", 'Here is the output:\n{"a": 1}'),
    ("a closing remark after", '{"a": 1}\n\nLet me know if you need anything else.'),
    ("a closing remark in Chinese", '{"a": 1}\n\n如需调整请告诉我。'),
    ("a fence around it", '```json\n{"a": 1}\n```'),
    ("a preamble and a fence", 'Here you go:\n```json\n{"a": 1}\n```'),
    ("a fence then a remark", '{"a": 1}\n```\nNotes: I used three sources.'),
    ("a second object after", '{"a": 1}\n{"b": 2}'),
    ("braces in the preamble", 'see {step 1} below\n{"a": 1}\nDone.'),
    ("whitespace and a newline after", '{"a": 1}   \n'),
])
def test_the_object_is_found_whatever_surrounds_it(name, text):
    assert extract_json_object(text) == {"a": 1}, name


def test_a_nested_object_survives_a_trailing_remark():
    """Nesting is where a cut at the wrong brace does the most damage, because the inner braces are
    legitimate structure rather than a mistake to recover from."""
    assert extract_json_object('{"a": {"b": [1, 2]}}\n\nThat is all.') == {"a": {"b": [1, 2]}}


def test_a_brace_inside_a_string_does_not_end_the_object():
    """A closing brace inside a quoted value is a character, not structure. An extractor that cuts
    at the first `}` truncates the object into something unparseable, and source titles contain
    braces."""
    assert extract_json_object('{"title": "a } brace", "n": 1}\nDone.') == {
        "title": "a } brace", "n": 1}


def test_an_escaped_quote_does_not_end_the_string():
    assert extract_json_object('{"q": "he said \\"}\\" ok"}\nDone.') == {"q": 'he said "}" ok'}


def test_the_first_object_is_the_one_returned():
    """Not the longest and not the last. The first is what the model meant, and a second object in
    the same message is a remark about the first."""
    assert extract_json_object('{"a": 1}\n{"b": 2}') == {"a": 1}


def test_a_truncated_object_is_not_returned():
    """There is nothing valid to extract, and inventing a partial object would put a lie one step
    further down the pipeline. The caller reports the failure; that is the honest outcome."""
    assert extract_json_object('{"a": 1, "b": "unterminated') is None


def test_a_message_with_no_object_at_all_is_not_returned():
    assert extract_json_object("I could not complete the task.") is None


def test_a_bare_value_is_not_an_object():
    """A list or a number is not what the callers asked for, and returning one would move the
    failure to the schema check with a worse message."""
    assert extract_json_object("[1, 2, 3]") is None
    assert extract_json_object("42") is None


def test_non_text_input_is_not_returned():
    assert extract_json_object(None) is None  # type: ignore[arg-type]


def test_normalising_keeps_the_object_and_otherwise_the_original():
    assert normalise_json_text('{"a": 1}\nDone.') == '{"a": 1}'
    assert normalise_json_text("nothing here") == "nothing here"
