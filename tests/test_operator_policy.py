"""Operator policy is explicit, enforced, and pinned to a version.

`max_rounds` used to pass graph validation and be read by nothing. That is worse than an absent
option: an operator sets it, sees no error, and believes there is a ceiling. The first test
below is that the ceiling exists at all, and the rest are about the two properties that make it
a policy rather than a trap — it is recorded as its own reason rather than looking like a
condition that came out false, and it belongs to the pinned version so a later edit cannot reach
back and change what a running task is allowed to do.
"""

from __future__ import annotations

from uuid import uuid4

import pytest

from anchor.domain.graph import GraphDefinition, GraphVersion
from anchor.domain.models import EdgeDecisionReason
from anchor.domain.propagation import cycle_back_edges, decide_outgoing_edges


def looping(metadata: dict[str, str]) -> GraphDefinition:
    """A graph whose cycle crosses a loop node, which is what the validator requires."""
    return GraphDefinition.model_validate({
        "graph_id": "policy", "name": "Policy", "entry_node_id": "work", "metadata": metadata,
        "nodes": [
            {"id": "work", "type": "agent", "name": "Work", "agent_ref": "x"},
            {"id": "again", "type": "loop", "name": "Again", "exit_condition": "finished"},
            {"id": "tail", "type": "agent", "name": "Tail", "agent_ref": "x"},
            {"id": "done", "type": "artifact", "name": "Done"},
        ],
        "edges": [
            {"source": "work", "target": "again"},
            {"source": "again", "target": "tail"},
            {"source": "tail", "target": "work"},
            {"source": "again", "target": "done"},
        ],
    })


def back_edge_decision(version: GraphVersion, attempt: int):
    decisions = decide_outgoing_edges(
        version, run_id=uuid4(), source_node_id="tail", evaluation_context={},
        evidence_ref="artifact://x", source_attempt=attempt)
    back = cycle_back_edges(version)
    assert back, "the fixture must have a cycle"
    return next(item for item in decisions if item.edge_index in back)


def test_a_ceiling_stops_the_loop_and_says_so(tmp_path):
    """The defect this replaces: a validated option that no code read."""
    version = GraphVersion.publish(looping({"max_rounds": "3"}), 1)
    assert back_edge_decision(version, 0).selected is True
    assert back_edge_decision(version, 1).selected is True
    capped = back_edge_decision(version, 2)
    assert capped.selected is False
    assert capped.reason is EdgeDecisionReason.REVISION_CEILING


def test_the_reason_distinguishes_capping_from_a_false_condition():
    """An operator reading the decision must not have to guess which happened."""
    version = GraphVersion.publish(looping({"max_rounds": "2"}), 1)
    capped = back_edge_decision(version, 5)
    assert capped.reason is EdgeDecisionReason.REVISION_CEILING
    assert capped.reason is not EdgeDecisionReason.CONDITION_FALSE
    assert capped.reason.value == "revision_ceiling"


def test_absence_means_unbounded():
    """A healthy task must not be stopped by a round count nobody chose."""
    version = GraphVersion.publish(looping({}), 1)
    for attempt in (0, 1, 50, 1000):
        assert back_edge_decision(version, attempt).selected is True


def test_the_ceiling_belongs_to_the_pinned_version(tmp_path):
    """Changing the policy later must not reach a run that is already going.

    Two versions of the same graph, one capped and one not. A run pins one of them, so its
    ceiling is a fact about that version rather than a fact about the graph's current state.
    """
    capped = GraphVersion.publish(looping({"max_rounds": "2"}), 1)
    unbounded = GraphVersion.publish(looping({}), 2)
    assert back_edge_decision(capped, 5).selected is False
    assert back_edge_decision(unbounded, 5).selected is True
    # And re-reading the capped version still caps, whatever was published since.
    assert back_edge_decision(capped, 5).selected is False


@pytest.mark.parametrize("value", ["0", "-1", "not a number"])
def test_a_ceiling_that_is_not_a_positive_integer_is_refused(value):
    """Graph validation already rejects these; propagation must not accept them either, because
    a run could be pinned to a version published before the rule existed."""
    from anchor.domain.propagation import PropagationError

    with pytest.raises((ValueError, PropagationError)):
        GraphVersion.publish(looping({"max_rounds": value}), 1)


def test_the_run_console_has_a_state_for_every_way_a_run_can_wait_or_stop():
    """A surface that cannot show a state cannot be used to act on it.

    Checked against the console source rather than a running browser, which is the limit of what
    this suite can do — but it is the difference between "we intended to show it" and "the
    string is not there at all".
    """
    from pathlib import Path

    source = (Path(__file__).resolve().parents[1] / "apps" / "web" / "src"
              / "RunConsole.tsx").read_text(encoding="utf-8")
    for state in ("waiting_approval", "waiting_event", "paused", "unknown"):
        assert state in source, f"the console does not surface {state}"
    # Reconciling an unknown side effect is the one operator action with no automatic path.
    assert "reconcil" in source
