"""Research campaign bookkeeping: merge evidence rounds and decide saturation.

A campaign has no round cap. It continues while a round adds evidence and the
gatherer does not judge the picture saturated, and it ends when a round adds
nothing or the gatherer reports saturation. This module owns that policy so the
academic paper policy in `academic.py` stays about the deliverable.
"""

from __future__ import annotations

import json

from anchor.runtime.evidence import successful_operations, verify_source


# A campaign has no round budget, but it must be able to end. Research stops when
# further rounds stop paying: when the last few rounds each added less than this
# share of the corpus, the field's load-bearing work is already covered and more
# searching only adds marginal references. This is a convergence criterion, not
# a budget: it fires on diminishing returns, never on a pre-set count.
MIN_MARGINAL_GAIN = 0.05
SATURATION_WINDOW = 3


def merge_ledger(previous: dict | None, new: dict) -> dict:
    """Merge evidence rounds, renumbering citations by first appearance.

    Each round numbers its own sources from one, so the merge is keyed by source
    identity and the numbering is rebuilt. Notes, coverage and tensions are
    remapped onto the merged numbering so the writer sees one coherent ledger.
    """
    merged: dict = {"sources": [], "search_log": [], "evidence_notes": [],
                    "coverage": [], "tensions": [], "unresolved": []}
    order: list[str] = []
    by_id: dict[str, dict] = {}
    notes: dict[str, dict] = {}
    for ledger in (previous or {}, new):
        remap: dict[object, str] = {}
        for source in ledger.get("sources", []) or []:
            identity = source.get("id")
            if not isinstance(identity, str):
                continue
            if identity not in by_id:
                by_id[identity] = dict(source)
                order.append(identity)
            else:
                # A later round may repair a source, for example by reading the
                # URL that matches its search result. Freezing the first version
                # would make a defective source permanently uncorrectable and the
                # campaign could never converge.
                by_id[identity].update(source)
            remap[source.get("citation")] = identity
        for note in ledger.get("evidence_notes", []) or []:
            identity = remap.get(note.get("citation"))
            if identity:
                notes[identity] = dict(note)
        merged["search_log"].extend(ledger.get("search_log", []) or [])
        merged["unresolved"].extend(ledger.get("unresolved", []) or [])
        for entry in ledger.get("coverage", []) or []:
            merged["coverage"].append({
                **entry,
                "evidence_ids": [identity for identity in
                                 (remap.get(number) for number in entry.get("evidence_ids", []) or [])
                                 if identity]})
        for entry in ledger.get("tensions", []) or []:
            merged["tensions"].append({
                **entry,
                "evidence_ids": [identity for identity in
                                 (remap.get(number) for number in entry.get("evidence_ids", []) or [])
                                 if identity]})
    renumber = {identity: index for index, identity in enumerate(order, start=1)}
    for identity in order:
        source = by_id[identity]
        source["citation"] = renumber[identity]
        merged["sources"].append(source)
    for identity, note in notes.items():
        if identity in renumber:
            merged["evidence_notes"].append({**note, "citation": renumber[identity]})
    merged["evidence_notes"].sort(key=lambda note: note["citation"])
    return merged


def ledger_index(ledger: dict) -> dict:
    """A compact view of what is already covered, for the next round."""
    return {"covered": [{"id": source.get("id"), "citation": source.get("citation"),
                         "title": (source.get("title") or "")[:120],
                         "year": source.get("year"),
                         "read": bool(source.get("read_ref"))}
                        for source in ledger.get("sources", [])],
            "unresolved": ledger.get("unresolved", [])}


def unverifiable_sources(ledger: dict, operations) -> list[dict]:
    """Round sources whose provenance cannot be checked in this Run.

    A source must come from a search or citation lookup: those operations return
    the metadata and the id. A document read proves the text was fetched, not
    that the paper was retrieved with an identity, so it cannot be a source's
    evidence_ref.
    """
    successful = {item.result_ref: item.tool_ref for item in operations
                  if item.status.value == "succeeded" and item.result_ref}
    bad: list[dict] = []
    for source in ledger.get("sources", []) or []:
        reference = source.get("evidence_ref")
        if successful.get(reference) not in ("scholarly.search", "scholarly.citations"):
            bad.append({"citation": source.get("citation"), "id": source.get("id"),
                        "evidence_ref": reference, "retrieved_by": successful.get(reference)})
    return bad


def drop_unverifiable(ledger: dict, operations, artifacts=None) -> tuple[dict, list[dict]]:
    """Remove sources whose provenance cannot be verified in this Run.

    The ledger is what the writer must cite in full, so a source whose evidence
    or reading does not resolve makes the contract unsatisfiable: cite it and the
    citation fails, omit it and the source is uncited. Such a source cannot be
    evidence, so it is removed here and reported, never left to stall the loop.
    """
    successful = successful_operations(operations)
    kept: list[dict] = []
    dropped: list[dict] = []
    for source in ledger.get("sources", []) or []:
        number = source.get("citation")
        identity, evidence = source.get("id"), source.get("evidence_ref")
        if not isinstance(identity, str) or not isinstance(evidence, str):
            dropped.append({"id": identity, "problem": "source has no id or evidence_ref"})
            continue
        if artifacts is None:
            # Without artifacts the full check is impossible; the cheap check is
            # still better than none, and callers in production always pass them.
            verdict_ok = successful.get(evidence) in ("scholarly.search", "scholarly.citations")
            reason = None if verdict_ok else "evidence_ref is not a successful lookup"
        else:
            verdict = verify_source(source, number=number, identity=identity,
                                    evidence_ref=evidence, successful=successful,
                                    artifacts=artifacts)
            verdict_ok, reason = verdict.ok, verdict.reason
        if not verdict_ok:
            dropped.append({"id": identity, "problem": reason})
            continue
        kept.append(source)
    if not dropped:
        return ledger, []
    # Citation numbers are deliberately NOT reassigned. Renumbering shifts every
    # number after a dropped source, while the writer is revising a manuscript
    # that already cites the old numbers: the manuscript then points at the wrong
    # sources and no revision can repair it. Numbers stay stable, and a gap is
    # left where a source was removed.
    survivors = {source.get("citation") for source in kept}
    removed_ids = {source.get("id") for source in ledger.get("sources", []) or []} - {
        source.get("id") for source in kept}
    return ({"sources": kept,
             "evidence_notes": [note for note in ledger.get("evidence_notes", []) or []
                                if note.get("citation") in survivors],
             "search_log": ledger.get("search_log", []),
             "coverage": ledger.get("coverage", []),
             "tensions": [entry for entry in ledger.get("tensions", []) or []
                          if not (set(entry.get("evidence_ids") or []) & removed_ids)],
             "unresolved": ledger.get("unresolved", [])}, dropped)


def evaluate_coverage(snapshot: dict, *, store, artifacts, run_id, node_id: str) -> dict:
    """Merge this round and decide whether research continues.

    There is no round cap: research continues while a round adds evidence and
    the gatherer does not judge the picture saturated. A round that adds nothing
    is convergence, not exhaustion of a budget.
    """
    round_ledger = snapshot.get("round_ledger")
    if not isinstance(round_ledger, dict):
        round_ledger = {}
    prior_runs = [item for item in store.list_node_runs(run_id)
                  if item.node_id == node_id and item.status.value == "completed" and item.output_ref]
    prior = None
    if prior_runs:
        try:
            prior = json.loads(artifacts.get_text(
                max(prior_runs, key=lambda item: item.attempt).output_ref))
        except (OSError, ValueError, TypeError):
            prior = None
    # Re-entering after a published paper means the reviewer asked for more
    # evidence: start another campaign rather than re-deciding saturation.
    seed = not isinstance(prior, dict) or prior.get("decision") == "write"
    operations = store.list_tool_operations(run_id)
    previous = prior.get("ledger") if isinstance(prior, dict) else None
    if previous:
        # Growth is measured against verifiable evidence only, so a dropped
        # source neither counts as progress nor keeps a round alive.
        previous, _ = drop_unverifiable(previous, operations, artifacts)
    ledger = merge_ledger(previous, round_ledger)
    ledger, removed = drop_unverifiable(ledger, operations, artifacts)
    before = len((previous or {}).get("sources", []) or [])
    added = len(ledger["sources"]) - before
    gains = [float(gain) for gain in (prior.get("gains") or [])] if isinstance(prior, dict) else []
    if seed:
        gains = []
    elif ledger["sources"]:
        gains.append(round(added / len(ledger["sources"]), 4))
    gains = gains[-SATURATION_WINDOW:]
    marginal = len(gains) >= SATURATION_WINDOW and all(
        gain < MIN_MARGINAL_GAIN for gain in gains)
    saturation = bool(round_ledger.get("saturation"))
    converged = saturation or marginal or added <= 0
    rounds = (int(prior.get("round", 0)) if isinstance(prior, dict) else 0) + (0 if seed else 1)
    decision = "continue" if seed else ("write" if converged else "continue")
    bad = unverifiable_sources(ledger, operations)
    index = ledger_index(ledger)
    if bad:
        # Surface them to the next round and to the operator instead of
        # letting an unverifiable source poison the paper's citations.
        index["unverified_provenance"] = bad
    return {**snapshot, "ledger": ledger, "covered": index,
            "decision": decision, "round": rounds, "added": added,
            "gains": gains, "marginal": marginal, "saturation": saturation,
            "unverifiable": bad, "dropped_unverifiable": removed}


class CoverageGateBehavior:
    """Deterministic merge and continuation decision for evidence rounds."""

    def preflight(self, snapshot: dict, *, store, artifacts, run_id) -> dict | None:
        return None

    def validate_output(self, text: str) -> None:
        return None

    def execute_control(self, snapshot: dict, *, store, artifacts, run_id, node_id) -> dict:
        return evaluate_coverage(snapshot, store=store, artifacts=artifacts,
                                 run_id=run_id, node_id=node_id)



