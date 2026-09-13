"""A transition certificate, and what a graph can check that a session could not.

The archived project's rule is the reason this exists: every previous active item gets exactly one
disposition, non-carry dispositions say why and where from, demote carries a recoverable
reference, and no item may disappear silently. Its certificate could confirm that an item had
been disposed of and nothing more, because an archived session had no operation ledger, no
verifiers and no content addressing.

A graph has all three, so a cited reference can be resolved rather than believed. These tests
separate the two halves deliberately: the ported rules hold with no resolver at all, and the
reference checks only run when one is supplied — a check that did not run must never look like a
check that passed.
"""

from __future__ import annotations

import pytest

from anchor.context_engine.cognition import (
    Cognition,
    CognitionItem,
    ItemDisposition,
    KnowledgeReference,
    TransitionCertificate,
    validate_transition,
)


def item(id_: str, *, evidence: tuple[str, ...] = ()) -> CognitionItem:
    return CognitionItem(id=id_, statement=f"{id_} is true", sources=("episode:1",),
                         relevance="changes the next action", evidence=evidence)


def cognition(*, facts: tuple[CognitionItem, ...] = (item("fact-1"),),
              decisions: tuple[CognitionItem, ...] = (item("decision-1"),),
              index: tuple[KnowledgeReference, ...] = ()) -> Cognition:
    return Cognition(situation={"confirmed_facts": list(facts), "active_hypotheses": [],
                                "unresolved_conflicts": [], "blockers": []},
                     experience={"decisions": list(decisions), "failed_paths": []},
                     intent={"open_questions": []},
                     knowledge_index=index)


class Resolver:
    """A stand-in for the graph's own facts, so each check can be exercised on its own."""

    def __init__(self, *, failed=(), succeeded=(), artifacts=(), verifiers=(), unknown=()):
        self._failed, self._succeeded = set(failed), set(succeeded)
        self._artifacts, self._verifiers, self._unknown = set(artifacts), set(verifiers), set(unknown)

    def _verdict(self, name: str, known: set) -> bool | None:
        return None if name in self._unknown else name in known

    def operation_failed(self, operation_id):
        return self._verdict(operation_id, self._failed)

    def operation_succeeded(self, operation_id):
        return self._verdict(operation_id, self._succeeded)

    def artifact_exists(self, ref):
        return self._verdict(ref, self._artifacts)

    def verifier_exists(self, ref):
        return self._verdict(ref, self._verifiers)


def codes(problems) -> list[str]:
    return [problem.code for problem in problems]


# -- the ported rules: these hold with no resolver ----------------------------------


def test_a_complete_certificate_has_no_problems():
    certificate = TransitionCertificate((ItemDisposition("fact-1", "carry"),
                                        ItemDisposition("decision-1", "archive",
                                                        reason="embodied by the code",
                                                        sources=("episode:1",))))
    assert validate_transition(certificate, cognition(), cognition()) == []


def test_a_missing_disposition_is_an_item_vanishing():
    """The rule the whole design exists for. An item with no disposition is not forgotten — it is
    unrecorded, which is worse, because nothing later can tell that it was ever there."""
    problems = validate_transition(TransitionCertificate((ItemDisposition("fact-1", "carry"),)),
                                   cognition(), cognition())
    assert codes(problems) == ["coverage_incomplete"]
    assert "decision-1" in problems[0].detail


def test_an_invented_id_is_refused():
    """Coverage in both directions: an extra id is a claim about a state that never existed."""
    certificate = TransitionCertificate((ItemDisposition("fact-1", "carry"),
                                        ItemDisposition("decision-1", "carry"),
                                        ItemDisposition("never-existed", "carry")))
    problems = codes(validate_transition(certificate, cognition(), cognition()))
    assert "unknown_previous_item" in problems
    # And the invented id is carried, so it is also absent from the submitted cognition — the
    # second, independent way the same fabrication shows up.
    assert "carry_not_present" in problems


def test_disposing_of_one_item_twice_is_refused():
    certificate = TransitionCertificate((ItemDisposition("fact-1", "carry"),
                                        ItemDisposition("fact-1", "archive", reason="x",
                                                        sources=("episode:1",)),
                                        ItemDisposition("decision-1", "carry")))
    assert codes(validate_transition(certificate, cognition(), cognition())) == ["duplicate_disposition"]


@pytest.mark.parametrize("disposition", ["revise", "resolve", "supersede", "demote", "archive"])
def test_a_non_carry_disposition_must_say_why_and_where_from(disposition):
    """A carry is self-evident — the item is still there. Anything else has to be argued, because
    it is a claim that the state changed."""
    bare = TransitionCertificate((ItemDisposition("fact-1", "carry"),
                                 ItemDisposition("decision-1", disposition)))
    problems = codes(validate_transition(bare, cognition(), cognition()))
    assert "missing_reason" in problems and "missing_source" in problems


def test_demotion_without_a_reference_is_refused():
    """Without this, "demote" is another word for "delete" and the certificate records a loss as
    if it were a decision."""
    certificate = TransitionCertificate((ItemDisposition("fact-1", "carry"),
                                        ItemDisposition("decision-1", "demote", reason="audit only",
                                                        sources=("episode:1",))))
    assert "missing_reference" in codes(validate_transition(certificate, cognition(), cognition()))


@pytest.mark.parametrize("disposition", ["revise", "supersede"])
def test_a_replacement_disposition_must_name_its_replacement(disposition):
    certificate = TransitionCertificate((ItemDisposition("fact-1", "carry"),
                                        ItemDisposition("decision-1", disposition, reason="superseded",
                                                        sources=("episode:2",))))
    assert "missing_replacement" in codes(validate_transition(certificate, cognition(), cognition()))


def test_a_replacement_must_point_at_something_that_exists():
    certificate = TransitionCertificate((ItemDisposition("fact-1", "carry"),
                                        ItemDisposition("decision-1", "revise", reason="better",
                                                        sources=("episode:2",),
                                                        replacement_id="not-submitted")))
    assert "replacement_not_present" in codes(
        validate_transition(certificate, cognition(), next_cognition=cognition()))


def test_a_carried_item_must_still_be_in_the_submitted_cognition():
    """A carry asserts the item survived. If the cognition does not contain it, the certificate
    and the cognition disagree, and that has to surface here rather than as a missing item three
    compressions later."""
    certificate = TransitionCertificate((ItemDisposition("fact-1", "carry"),
                                        ItemDisposition("decision-1", "carry")))
    problems = validate_transition(certificate, cognition(),
                                   next_cognition=cognition(decisions=()))
    assert "carry_not_present" in codes(problems)
    assert problems[0].item_id in ("decision-1", "fact-1")


# -- what a graph adds: cited evidence can be resolved -------------------------------


def test_a_demotion_reference_must_appear_in_the_knowledge_index():
    """Ported rule, and it holds with no resolver: otherwise the certificate is the only place
    the reference was ever written down."""
    certificate = TransitionCertificate((ItemDisposition("fact-1", "carry"),
                                        ItemDisposition("decision-1", "demote",
                                                        reason="audit only",
                                                        sources=("episode:2",),
                                                        reference="artifact://sha256/4d4c7eee2e28d03cb2dbf3df639c3290ade66e18755e83caade2d8f37bd8c044")))
    # No knowledge index in the submitted cognition, so the reference was never written down.
    problems = validate_transition(certificate, cognition(), next_cognition=cognition())
    assert "demotion_missing_from_index" in codes(problems)


def test_an_indexed_demotion_without_a_resolver_reports_that_it_was_not_checked():
    index = (KnowledgeReference(id="ref-1", cue="prior decision",
                                locator="artifact://sha256/4d4c7eee2e28d03cb2dbf3df639c3290ade66e18755e83caade2d8f37bd8c044", source="episode:2"),)
    certificate = TransitionCertificate((ItemDisposition("fact-1", "carry"),
                                        ItemDisposition("decision-1", "demote",
                                                        reason="audit only",
                                                        sources=("episode:2",),
                                                        reference="artifact://sha256/4d4c7eee2e28d03cb2dbf3df639c3290ade66e18755e83caade2d8f37bd8c044")))
    # The citation satisfies the ported rule, and the reference still cannot be resolved without
    # a resolver — so this reports rather than passing, which is the honest outcome.
    assert codes(validate_transition(certificate, cognition(index=index),
                                     cognition(index=index))) == ["evidence_unchecked"]
    # With a resolver, and the content present, it passes.
    assert validate_transition(certificate, cognition(index=index), cognition(index=index),
                               resolver=Resolver(artifacts={"artifact://sha256/4d4c7eee2e28d03cb2dbf3df639c3290ade66e18755e83caade2d8f37bd8c044"})) == []


def test_citing_evidence_without_a_resolver_is_reported_not_passed():
    """The distinction that keeps the checks honest. A caller that ran no reference checks has not
    verified anything, and silence would read as having done so."""
    next_cognition = cognition(facts=(item("fact-1", evidence=("operation:op-1",)),))
    certificate = TransitionCertificate((ItemDisposition("fact-1", "carry"),
                                        ItemDisposition("decision-1", "carry")))
    problems = codes(validate_transition(certificate, cognition(), next_cognition=next_cognition))
    assert "evidence_unchecked" in problems


def test_a_failed_path_must_name_an_operation_that_really_failed():
    """This is the check an archived session could not make and a graph can.

    "This approach already failed" is the assertion most worth verifying, because a state that
    gets it wrong sends the agent back down a path it has already paid for.
    """
    next_cognition = Cognition(
        situation={"confirmed_facts": [], "active_hypotheses": [],
                   "unresolved_conflicts": [], "blockers": []},
        experience={"decisions": [],
                    "failed_paths": [item("failed-1", evidence=("failed-operation:op-1",))]},
        intent={"open_questions": []})
    certificate = TransitionCertificate((ItemDisposition("fact-1", "archive", reason="superseded",
                                                         sources=("episode:2",)),
                                        ItemDisposition("decision-1", "archive",
                                                        reason="superseded",
                                                        sources=("episode:2",))))
    # The operation exists and did fail.
    ok = validate_transition(certificate, cognition(), next_cognition=next_cognition,
                             resolver=Resolver(failed={"op-1"}))
    assert codes(ok) == []
    # The same claim about an operation that succeeded.
    wrong = validate_transition(certificate, cognition(), next_cognition=next_cognition,
                                resolver=Resolver(succeeded={"op-1"}))
    assert "evidence_contradicts" in codes(wrong)


def test_an_unresolvable_reference_is_not_a_pass():
    next_cognition = cognition(facts=(item("fact-1", evidence=("operation:op-9",)),))
    certificate = TransitionCertificate((ItemDisposition("fact-1", "carry"),
                                        ItemDisposition("decision-1", "carry")))
    problems = codes(validate_transition(certificate, cognition(),
                                        next_cognition=next_cognition,
                                        resolver=Resolver(unknown={"op-9"})))
    assert "evidence_unresolved" in problems


def test_a_confirmed_fact_may_cite_an_operation_or_an_artifact():
    next_cognition = cognition(facts=(item("fact-1", evidence=("operation:op-1",
                                                              "artifact://sha256/4d4c7eee2e28d03cb2dbf3df639c3290ade66e18755e83caade2d8f37bd8c044")),))
    certificate = TransitionCertificate((ItemDisposition("fact-1", "carry"),
                                        ItemDisposition("decision-1", "carry")))
    assert validate_transition(certificate, cognition(), next_cognition=next_cognition,
                               resolver=Resolver(succeeded={"op-1"},
                                                 artifacts={"artifact://sha256/4d4c7eee2e28d03cb2dbf3df639c3290ade66e18755e83caade2d8f37bd8c044"})) == []


def test_an_accepted_criterion_must_name_a_verifier_that_exists():
    """The Contract's acceptance criteria are prose in the archived design. Here a verifier is a
    declared object, so the criterion can point at it and the pointer can be checked."""
    next_cognition = Cognition(
        situation={"confirmed_facts": [item("c-1", evidence=("verifier:verifiers.check",))],
                   "active_hypotheses": [], "unresolved_conflicts": [], "blockers": []},
        experience={"decisions": [], "failed_paths": []},
        intent={"open_questions": []})
    certificate = TransitionCertificate((ItemDisposition("fact-1", "archive", reason="moved into "
                                                         "the contract", sources=("episode:2",)),
                                        ItemDisposition("decision-1", "archive",
                                                        reason="moved", sources=("episode:2",))))
    assert validate_transition(certificate, cognition(), next_cognition=next_cognition,
                               resolver=Resolver(verifiers={"verifiers.check"})) == []
    missing = validate_transition(certificate, cognition(), next_cognition=next_cognition,
                                  resolver=Resolver(verifiers=set()))
    assert "evidence_contradicts" in codes(missing)


def test_a_locator_that_does_not_resolve_is_reported():
    """Content addressing is what makes this checkable: the locator either resolves or it does
    not, and no version number has to be trusted."""
    index = (KnowledgeReference(id="ref-1", cue="cue", locator="artifact://sha256/283bb9deef02e6843abfb538efa1eca70801bd8a701c3f98191e123496339247",
                                source="episode:2"),)
    certificate = TransitionCertificate((ItemDisposition("fact-1", "carry"),
                                        ItemDisposition("decision-1", "demote",
                                                        reason="audit", sources=("episode:2",),
                                                        reference="artifact://sha256/283bb9deef02e6843abfb538efa1eca70801bd8a701c3f98191e123496339247")))
    problems = codes(validate_transition(certificate, cognition(index=index),
                                        next_cognition=cognition(index=index),
                                        resolver=Resolver()))
    assert "locator_missing" in problems
    assert "reference_missing" in problems


def test_a_reference_scheme_we_do_not_understand_is_not_assumed_to_be_fine():
    next_cognition = cognition(facts=(item("fact-1", evidence=("ftp://elsewhere",)),))
    certificate = TransitionCertificate((ItemDisposition("fact-1", "carry"),
                                        ItemDisposition("decision-1", "carry")))
    assert "evidence_unresolved" in codes(
        validate_transition(certificate, cognition(), next_cognition=next_cognition,
                            resolver=Resolver()))


def test_an_empty_cognition_needs_no_resolver():
    """Nothing cited means nothing to check, and reporting `evidence_unchecked` for an empty
    state would make the code noisier than the situation warrants."""
    empty = Cognition(situation={"confirmed_facts": [], "active_hypotheses": [],
                                 "unresolved_conflicts": [], "blockers": []},
                      experience={"decisions": [], "failed_paths": []},
                      intent={"open_questions": []})
    certificate = TransitionCertificate((ItemDisposition("fact-1", "archive", reason="done",
                                                         sources=("episode:2",)),
                                        ItemDisposition("decision-1", "archive", reason="done",
                                                        sources=("episode:2",))))
    assert validate_transition(certificate, cognition(), next_cognition=empty) == []


def test_every_problem_is_reported_rather_than_the_first():
    """A caller fixing one problem at a time, one model call per problem, is a bad loop."""
    certificate = TransitionCertificate((
        ItemDisposition("fact-1", "demote"),                       # no reason, source or reference
        ItemDisposition("decision-1", "revise"),                   # no reason, source or replacement
        ItemDisposition("invented", "carry"),                      # not in the previous cognition
    ))
    problems = codes(validate_transition(certificate, cognition(), cognition()))
    assert {"unknown_previous_item", "missing_reason", "missing_source",
            "missing_reference", "missing_replacement"} <= set(problems)
    assert len(problems) > 5, "all of them, not the first few"


def test_the_submitted_cognition_is_required_so_no_check_can_silently_skip():
    """It was optional once, and three checks returned nothing without it — which reads as a pass.
    Required is the only shape where that cannot happen."""
    import inspect

    signature = inspect.signature(validate_transition)
    assert signature.parameters["next_cognition"].default is inspect.Parameter.empty
