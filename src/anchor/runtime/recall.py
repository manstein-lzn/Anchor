"""Reading back what the state points at.

Compression replaces old messages with a state that names its detail by reference. That promise —
"a demoted item keeps a recoverable reference" — is only kept if something can follow the
reference, and this is that thing. Without it the reference is a footnote nobody can act on.

**The permission set is what the run already points at.** A recall tool that resolved any reference
would be a way to read content this node was never given, which is the same hidden-global-visibility
failure the declared-input rule exists to prevent. So the question asked is not "does this content
exist" but "does this run already reference it", and the answer comes from the same reachability
computation retention uses to decide what is reclaimable — one mechanism, two uses.

**Checked when asked, not when the tool is built.** The state is created part-way through an
attempt, so a permission set computed at the start would not contain the references that
compression itself just wrote. Evaluating the question per call is both simpler and correct.

**Refusal is a message, not a crash.** A reference the run does not hold, or content that cannot be
read, returns text the model can act on. The model asked a question; a vanished process would not
be an answer.
"""

from __future__ import annotations

import json
import logging
from typing import Any
from uuid import UUID

logger = logging.getLogger("anchor.recall")

#: How much of a stored artifact one recall returns. Bounded, because recall is called from inside
#: a loop whose growth is the thing being managed: a tool that can return an unbounded answer
#: reintroduces on one call what compression just removed.
DEFAULT_CHARS = 4000


def referenced_by_run(store: Any, run_id: UUID) -> set[str]:
    """Every content reference this run holds.

    Uses the store's own reachability listing and filters to the run. That listing already exists
    because retention needs it — deciding what must not be reclaimed — and the same answer is what
    makes "this run may read this" true. Two questions, one computation, so they cannot disagree.
    """
    try:
        return {ref for owner, ref in store.list_artifact_references() if owner == run_id}
    except Exception:  # noqa: BLE001 - an unlistable store means nothing is readable
        logger.exception("could not list the references of run %s", run_id)
        return set()


def reference_of(question: str) -> str | None:
    """The reference a question is about, from a JSON tool call.

    Accepts both the plain form and a `{"ref": "..."}` object, because a model that has been told
    the tool takes a reference will sometimes pass the bare string.
    """
    text = (question or "").strip()
    if not text:
        return None
    if text.startswith("{"):
        try:
            parsed = json.loads(text)
        except json.JSONDecodeError:
            return None
        value = parsed.get("ref") or parsed.get("locator") or parsed.get("reference")
        return str(value) if value else None
    return text


def resolve(store: Any, artifacts: Any, *, run_id: UUID, reference: str,
            chars: int = DEFAULT_CHARS) -> str:
    """What the reference points at, or why it cannot be read.

    Every failure is text. A refusal and an unreadable artifact are different answers — one says the
    run does not hold this, the other says it does and the bytes are gone — and a caller deciding
    what to do next needs to tell them apart.
    """
    if not reference:
        return "REFUSED: no reference was given"
    from anchor.domain.content import ContentRefError, parse as parse_content_ref

    try:
        parsed = parse_content_ref(reference)
    except ContentRefError:
        # Operation and verifier references are answers about the ledger rather than stored
        # content, and recall is about content. Saying so is more useful than a silent miss.
        return (f"REFUSED: {reference} is not a content reference; recall reads stored content, "
                f"not ledger entries")
    if not parsed.is_artifact:
        return (f"REFUSED: {reference} names a workspace revision; recall reads stored artifacts. "
                f"A workspace is read through its own tools.")
    if reference not in referenced_by_run(store, run_id):
        return (f"REFUSED: this run does not reference {reference}, so reading it would reach "
                f"content this node was never given")
    try:
        text = artifacts.get_text(reference)
    except Exception as exc:  # noqa: BLE001 - reported, never raised into the loop
        logger.warning("reading %s failed: %s", reference, exc)
        return f"UNAVAILABLE: {reference} is referenced but cannot be read ({type(exc).__name__})"
    if len(text) <= chars:
        return text
    # Truncated rather than refused: the reference resolved, so the content exists, and the caller
    # can ask again for a different part. A refusal would claim the run does not hold it.
    return (text[:chars] + f"\n[truncated: {len(text) - chars} of {len(text)} characters omitted; "
                           f"this is stored content, and the whole of it exists at {reference}]")
