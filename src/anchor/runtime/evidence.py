"""One source-verification policy, shared by the gate and the validator.

The coverage gate decides which sources enter the ledger; the review validator
decides which of them a paper may cite. If those two disagree, a source can be
admitted and then be uncitable, and the writer is left with a contract it cannot
satisfy — cite it and the citation fails, omit it and a source is uncited. Both
sides now call `verify_source`, so admission and citation cannot drift apart.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass


def _title_tokens(value: object) -> set[str]:
    """Lower-cased alphanumeric words of a title, for same-document checks."""
    if not isinstance(value, str):
        return set()
    return {word for word in re.findall(r"[a-z0-9]{2,}", value.lower())}


def same_document(observed: dict, reading: dict) -> bool:
    """Whether a read studied the source, by URL or by title.

    One work is routinely reachable as a publisher record and as a preprint at a
    different URL: a reader who studies the preprint is reading the same paper.
    A title match is accepted as evidence of that, and never of a different one.
    """
    allowed = {url for url in [observed.get("url"), *(observed.get("fulltext_urls") or [])]
               if isinstance(url, str)}
    documents = reading.get("documents")
    entries: list = list(documents) if isinstance(documents, list) else [reading]
    requested = {item.get("requested_url") for item in entries if isinstance(item, dict)}
    if allowed & requested:
        return True
    wanted = _title_tokens(observed.get("title"))
    if len(wanted) < 4:
        return False
    for item in entries:
        if not isinstance(item, dict):
            continue
        got = _title_tokens(item.get("title"))
        if got and len(wanted & got) / len(wanted) >= 0.8:
            return True
    return False


@dataclass(frozen=True)
class SourceVerdict:
    canonical: dict | None
    reason: str | None

    @property
    def ok(self) -> bool:
        return self.canonical is not None


def verify_source(source: dict, *, number: int, identity: str, evidence_ref: str,
                  successful: dict, artifacts) -> SourceVerdict:
    """Verify one source against the Run's operation ledger and artifacts."""
    if successful.get(evidence_ref) not in ("scholarly.search", "scholarly.citations"):
        return SourceVerdict(None, "has no successful scholarly search or citation lookup")
    try:
        evidence = json.loads(artifacts.get_text(evidence_ref))
        observed = next((item for item in evidence["papers"] if item["id"] == identity), None)
        if observed is None:
            return SourceVerdict(None, "source id not present in search results")
        canonical = {**observed, "citation": number, "evidence_ref": evidence_ref,
                     "retrieved_at": evidence["retrieved_at"], "read_ref": None}
        read_ref = source.get("read_ref")
        if read_ref:
            if (not isinstance(read_ref, str)
                    or successful.get(read_ref) not in ("scholarly.read", "scholarly.read_many")):
                return SourceVerdict(None, "read_ref is not a successful document read in this Run")
            reading = json.loads(artifacts.get_text(read_ref))
            if not same_document(observed, reading):
                return SourceVerdict(None, "document read does not match the retrieved source URLs")
            canonical["read_ref"] = read_ref
            canonical["read_truncated"] = reading.get("truncated", False)
        return SourceVerdict(canonical, None)
    except (OSError, ValueError, KeyError, TypeError) as exc:
        return SourceVerdict(None, f"evidence rejected: {exc}")


def successful_operations(operations) -> dict[str, str]:
    """Map result reference to tool reference for everything that succeeded."""
    return {item.result_ref: item.tool_ref for item in operations
            if item.status.value == "succeeded" and item.result_ref}
