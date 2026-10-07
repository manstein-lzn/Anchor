from __future__ import annotations

import json
from pathlib import Path
import sys

import pytest

sys.path.insert(0, str(Path(__file__).parents[1] / "scripts"))
import rust_native_pilot_smoke as smoke


def stream(chunks):
    return "".join(f"id: {index}\ndata: {json.dumps(chunk)}\n\n" for index, chunk in enumerate(chunks, 1)) + 'event: turn\ndata: {"status":"completed"}\n\n'


def chunks():
    return [{"type": "tool-input-available", "toolName": "graph_read", "toolCallId": "call"},
            {"type": "tool-output-available", "toolCallId": "call", "output": {"objective": "fixture-marker"}},
            {"type": "text-delta", "delta": "fixture-marker"}]


def test_exact_read_only_tool_result_and_text_are_required():
    assert smoke.check_first_turn(stream(chunks()), "fixture-marker") == {"tool_calls": 1, "reply": "fixture-marker"}
    assert smoke.sse_chunks(stream(chunks()).replace("\n", "\r\n")) == chunks()


@pytest.mark.parametrize("change", ["missing-tool", "wrong-tool", "wrong-id", "missing-result", "fabricated-text", "extra-tool"])
def test_fabricated_or_extra_actions_fail_closed(change):
    values = chunks()
    if change == "missing-tool":
        values.pop(0)
    elif change == "wrong-tool":
        values[0]["toolName"] = "graph_run"
    elif change == "wrong-id":
        values[1]["toolCallId"] = "unrelated"
    elif change == "missing-result":
        values[1]["output"] = {}
    elif change == "fabricated-text":
        values[2]["delta"] = "invented"
    else:
        values.append(values[0].copy())
    with pytest.raises(smoke.SmokeFailure):
        smoke.check_first_turn(stream(values), "fixture-marker")


def test_pilot_usage_uses_native_recordings_and_does_not_count_call_id_sidecar(tmp_path):
    root = tmp_path / "platform/pilot" / ("a" * 64)
    call = root / "providers/turn/provider.recordings/00000000000000000001"
    call.mkdir(parents=True)
    (call / "recording.json").write_text(json.dumps({"exchanges": [{"response": {"usage": {
        "prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5}}}]}))
    (call / "outcome.json").write_text('{"status":"completed"}')
    (root / "providers/turn/provider.call-ids.json").write_text('{}')
    usage = smoke.collect_usage(tmp_path, "platform/pilot/*/providers/*/provider.recordings/*")
    assert usage["provider_attempts"] == 1 and usage["usage_complete"]
    assert usage["reported_tokens"]["total_tokens"] == 5
