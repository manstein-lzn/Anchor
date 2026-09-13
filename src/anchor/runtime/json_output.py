"""Extract the JSON object a model meant to return.

A model told to answer with JSON sometimes prefixes it with a sentence, wraps it
in a code fence, or appends a closing remark. The intent is unambiguous and the
object is well formed, so the pipeline normalises it instead of failing the node.
"""

from __future__ import annotations

import json
import re

_FENCE = re.compile(r"```(?:json)?\s*(.+?)```", re.DOTALL)


def extract_json_object(text: str) -> dict | None:
    """Return the first JSON object in ``text``, or None when there is none."""
    for candidate in _candidates(text):
        try:
            value = json.loads(candidate)
        except (json.JSONDecodeError, ValueError, TypeError):
            continue
        if isinstance(value, dict):
            return value
    return None


def normalise_json_text(text: str) -> str:
    """Canonical text for a JSON-format response: the object, or the original."""
    value = extract_json_object(text)
    return json.dumps(value, ensure_ascii=False) if value is not None else text


def _closed_from(text: str, start: int) -> str | None:
    """The object beginning at ``start``, cut at the brace that closes it.

    Scanned rather than searched for the last brace, because the text after a complete object is
    exactly what this function exists to ignore. Quoting is tracked because a brace inside a string
    is a character, not structure: a source title containing one would otherwise end the object
    early and truncate it into something unparseable.
    """
    depth = 0
    in_string = False
    escaped = False
    for index in range(start, len(text)):
        character = text[index]
        if in_string:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                in_string = False
        elif character == '"':
            in_string = True
        elif character == "{":
            depth += 1
        elif character == "}":
            depth -= 1
            if depth == 0:
                return text[start:index + 1]
    return None


def _candidates(text: str):
    if not isinstance(text, str):
        return
    yield text
    fenced = _FENCE.search(text)
    if fenced:
        yield fenced.group(1)
    # Each opening brace yields the object it begins, cut at its own closing brace. Taking the text
    # from the brace to the end of the message instead — which is what this did — succeeds only
    # when nothing follows the object, so a model that signed off with "Let me know if you need
    # anything else" failed the node permanently and identically on every retry. The failure was
    # survivable only in the sense that it was invisible: the output was not retained, so nothing
    # recorded what the model had actually said.
    for index, character in enumerate(text):
        if character != "{":
            continue
        closed = _closed_from(text, index)
        if closed is not None:
            yield closed
        yield text[index:]
