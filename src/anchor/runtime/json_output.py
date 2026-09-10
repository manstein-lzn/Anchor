"""Extract the JSON object a model meant to return.

A model told to answer with JSON sometimes prefixes it with a sentence, or wraps
it in a code fence. The intent is unambiguous and the object is well formed, so
the pipeline normalises it instead of failing the node.
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


def _candidates(text: str):
    if not isinstance(text, str):
        return
    yield text
    fenced = _FENCE.search(text)
    if fenced:
        yield fenced.group(1)
    for index, character in enumerate(text):
        if character == "{":
            yield text[index:]
