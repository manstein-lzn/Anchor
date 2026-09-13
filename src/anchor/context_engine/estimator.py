"""Token estimation for the capacity check.

The provider rejects an over-long request outright, so "does this fit" has to be answered before
the call. Answering it needs a token count, and counting exactly means either a dependency on
the model's tokenizer or a round trip to ask it — neither is available where the plan is built.

So this estimates, calibrated from measurements rather than guessed. Four measurements, three
of them ours:

    plain ASCII  1,000,000 characters ->  125,032 tokens    0.125 tokens/character
    plain ASCII  8,500,000 characters -> 1,095,300 tokens    0.129 tokens/character
    the gather node's real prompt,  11,668 characters -> 3,360 tokens
    the write node's real prompt,   17,950 characters -> 7,181 tokens

A single ratio cannot serve: plain ASCII is one token per eight characters, while our prompts
include JSON, identifiers and Chinese. Solving the two real-prompt measurements for a
per-character-class ratio gives 0.735 tokens per CJK character and 0.211 per other character,
which are the numbers here rounded up by a safety factor of about 1.15. Note that `other` is
well above the plain-ASCII figure — punctuation, numbers and code tokenize worse than prose, and
the ASCII samples were prose.

The constants lean high on purpose. Over-estimating refuses a request that would have fit, which
is visible and recoverable; under-estimating sends a request the provider rejects after the whole
payload has been uploaded, which is neither. The first version of this file used the plain-ASCII
ratio for `other` and under-estimated real prompts by thirteen to twenty percent — the dangerous
direction — which is why the constants come from our own prompts and not from the ASCII samples.

`prompt_chars` elsewhere in the codebase is the *user* prompt only, while `input_tokens` counts
every call the tool loop has made. Their ratio is a loop amplification factor, not a token ratio,
and it was initially mistaken for one: it ranges from 0.5 to 25 across the recorded events.
"""

from __future__ import annotations

#: Above the basic multilingual plane, and the CJK blocks where a character is about a token.
_CJK_RANGES = ((0x3040, 0x30FF), (0x3400, 0x4DBF), (0x4E00, 0x9FFF), (0xAC00, 0xD7AF),
               (0xF900, 0xFAFF), (0x20000, 0x2FA1F))
#: Solved from two real prompts, rounded up by the safety factor. See the module docstring.
_TOKENS_PER_OTHER = 0.24
_TOKENS_PER_CJK = 0.85


def estimate_tokens(text: str) -> int:
    """A conservative token count for one request's text."""
    cjk = 0
    for character in text:
        point = ord(character)
        if any(low <= point <= high for low, high in _CJK_RANGES):
            cjk += 1
    other = len(text) - cjk
    return int(cjk * _TOKENS_PER_CJK + other * _TOKENS_PER_OTHER) + 1


def ratio_note() -> str:
    """What the constants are, for a report that has to say where a number came from."""
    return (f"estimated at {_TOKENS_PER_CJK} tokens/CJK char and {_TOKENS_PER_OTHER} "
            f"tokens/other char; both rounded up from measurements")
