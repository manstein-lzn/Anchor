"""Continuing a conversation a previous attempt left behind.

A trace ends with the synthetic message that says why the loop stopped, and resuming used to replay
it as though the model had said it. The provider refuses the role outright:

    BadRequestError: messages[120].role: unknown variant `exit`

Measured on a node resumed after `LimitsExceeded` — the one path resume exists for.
"""

from __future__ import annotations

import json
from types import SimpleNamespace

from anchor.simple.agent import TracingAgent


def _resuming_agent(tmp_path, seen: dict) -> TracingAgent:
    """A TracingAgent without a model, whose single step records what it was handed."""
    agent = TracingAgent.__new__(TracingAgent)
    agent.messages = []
    agent._trace = tmp_path / "trace.jsonl"
    agent.n_consecutive_format_errors = 0
    agent.config = SimpleNamespace(max_consecutive_format_errors=8)
    # `__init__` would have set these; the point of bypassing it is to avoid needing a model.
    agent.logger = SimpleNamespace(debug=lambda *args, **kwargs: None)

    def step() -> None:
        seen["messages"] = list(agent.messages)
        agent.add_messages({"role": "exit", "content": "done",
                            "extra": {"exit_status": "Submitted", "submission": "done"}})

    agent.step = step
    return agent


def test_a_stale_exit_marker_is_not_replayed(tmp_path):
    seen: dict = {}
    agent = _resuming_agent(tmp_path, seen)
    trace = [
        {"role": "system", "content": "instructions"},
        {"role": "assistant", "content": "working"},
        {"role": "tool", "content": "<returncode>0</returncode>"},
        {"role": "exit", "content": "LimitsExceeded",
         "extra": {"exit_status": "LimitsExceeded", "submission": ""}},
    ]

    outcome = agent.resume(trace)

    handed = seen["messages"]
    assert [message["content"] for message in handed] == ["instructions", "working",
                                                          "<returncode>0</returncode>"]
    assert all(message.get("role") != "exit" for message in handed)
    assert outcome["exit_status"] == "Submitted", "the new attempt's own exit is what is returned"


def test_a_conversation_that_does_not_end_in_exit_is_kept_whole(tmp_path):
    """Only the trailing marker is synthetic; nothing earlier is touched."""
    seen: dict = {}
    agent = _resuming_agent(tmp_path, seen)
    trace = [{"role": "system", "content": "instructions"},
             {"role": "assistant", "content": "halfway"}]

    agent.resume(trace)

    assert [message["content"] for message in seen["messages"]] == ["instructions", "halfway"]


def test_the_trimmed_conversation_is_what_the_trace_records(tmp_path):
    """Resuming appends; it does not rewrite the record of what the previous attempt did."""
    seen: dict = {}
    agent = _resuming_agent(tmp_path, seen)
    trace = [{"role": "assistant", "content": "working"},
             {"role": "exit", "content": "LimitsExceeded", "extra": {"exit_status": "LimitsExceeded"}}]

    agent.resume(trace)

    written = [json.loads(line) for line in agent._trace.read_text(encoding="utf-8").splitlines()]
    assert written == [{"role": "exit", "content": "done",
                        "extra": {"exit_status": "Submitted", "submission": "done"}}]
