from __future__ import annotations

import argparse
from copy import deepcopy
import hashlib
import json
from pathlib import Path
import sys

import pytest

sys.path.insert(0, str(Path(__file__).parents[1] / "scripts"))
import rust_native_pilot_mutation_smoke as smoke
from rust_low_cost_regression import invocation_digest


def save(path: Path, value) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value), encoding="utf-8")


def fixture_facts(root: Path, case: dict, session_id: str = "pilot-fixture") -> dict:
    run = "fixture-run"
    turn = {"id": "fixture-turn", "session": session_id, "request_id": case["request"]["request_id"],
            "prompt": case["request"]["message"], "status": "completed", "error": None,
            "native": {"scope": "b" * 64, "session": 7, "run": 11}, "runs": [run]}
    key = {"run_id": run, "graph_digest": "c" * 64, "node_id": "receipt", "invocation": 1}
    result = {"key": key, "commit": {"id": "artifact"}, "completion": {"submission": "", "route": None}}
    snapshot = {"objective": "Write one local mutation receipt", "input": {}, "entry": "receipt", "agents": {},
                "ops": {"receipt": {"run": f"sh -c 'printf {case['marker']} > receipt.txt'", "network": False,
                                    "wall_time_limit_seconds": 30}},
                "nodes": [{"id": "receipt", "op": "receipt", "agent": None, "with": None,
                           "plugins": [], "max_rounds": None}], "edges": [], "_module_rounds": {}}
    record = {"run_id": run, "graph_digest": key["graph_digest"], "snapshot": snapshot,
              "input": deepcopy(case["arguments"][2]["input"]), "status": "completed", "error": None,
              "invocations": {"receipt": 1}, "results": {"receipt": [result]}, "graph_calls": {}, "plugin_bindings": {}}
    detail = {"run": run, "graph": case["graph"], "active": False, "calls": [], "traces": {},
              "state": {"status": "completed", "input": record["input"], "objective": case["updated"]["objective"]}}
    metadata = {"run_id": run, "graph": case["graph"], "graph_digest": record["graph_digest"],
                "pilot": {"owner": "local", "session": session_id, "turn": turn["id"]}}
    expected = case["marker"].encode()
    artifact = root / "state/artifacts/artifact"
    save(artifact / "manifest.json", {"key": key, "completion": result["completion"], "files": {
        "receipt.txt": {"sha256": hashlib.sha256(expected).hexdigest(), "bytes": len(expected)},
    }})
    (artifact / "files").mkdir()
    (artifact / "files/receipt.txt").write_bytes(expected)
    workspace = root / "work" / run / invocation_digest(result)
    workspace.mkdir(parents=True)
    (workspace / "receipt.txt").write_bytes(expected)
    scope = root / "state/platform/pilot" / turn["native"]["scope"]
    locator = {"version": 1, "framework": "io-harness", "framework_version": "0.86.0",
               "root": str(scope), "session_id": turn["native"]["session"]}
    save(scope / "session.json", locator)
    (scope / "framework.sqlite3").write_bytes(b"SQLite format 3\0fixture-not-a-real-store")
    save(root / "state/runs" / f"{run}.json", record)
    save(root / "state/run-metadata" / f"{run}.json", metadata)
    return {"case": case, "turn": turn, "record": record, "detail": detail, "metadata": metadata,
            "session": {"id": session_id, "run_ids": [run]}, "reply": "已提交本地运行。",
            "locator": locator, "scope": scope, "workspace": workspace, "artifact": artifact}


def history(facts: dict) -> dict:
    return {"messages": [{"role": "user", "text": facts["case"]["request"]["message"]},
                         {"role": "assistant", "text": facts["reply"]}]}


def chunks(facts: dict) -> list[dict]:
    case = facts["case"]
    outputs = [{"graph": case["graph"], "definition": case["initial"]},
               {"graph": case["graph"], "definition": case["updated"]},
               {"run": facts["turn"]["runs"][0], "graph": case["graph"],
                "session": facts["turn"]["session"], "accepted": True}]
    values = []
    for index, name in enumerate(smoke.TOOL_NAMES):
        identifier = f"pilot-{facts['turn']['native']['run']}-tool-{index + 1}"
        values.extend([
            {"type": "tool-input-start", "toolName": name, "toolCallId": identifier},
            {"type": "tool-input-available", "toolName": name, "toolCallId": identifier,
             "input": deepcopy(case["arguments"][index])},
            {"type": "tool-output-available", "toolCallId": identifier,
             "output": [{"type": "text", "text": json.dumps(outputs[index])}]},
        ])
    values.append({"type": "text-delta", "delta": facts["reply"]})
    return values


def stream(values: list[dict], terminal: dict) -> str:
    return "".join(f"id: {index}\ndata: {json.dumps(value)}\n\n" for index, value in enumerate(values, 1)) + (
        f"event: turn\ndata: {json.dumps(terminal, indent=2).replace(chr(10), chr(10) + 'data: ')}\n\n")


def add_recording(facts: dict, index: int, complete: bool = True) -> Path:
    directory = facts["scope"] / "providers" / facts["turn"]["id"] / "provider.recordings" / f"{index:020d}"
    save(directory / "recording.json", {"exchanges": [{"request": {"messages": ["fixture-only"]}, "response": {
        "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5} if complete else None,
    }}]})
    save(directory / "outcome.json", {"status": "succeeded"})
    return directory


@pytest.fixture
def facts(tmp_path):
    case = smoke.mutation_case("a" * 32, "mutation-fixture")
    return fixture_facts(tmp_path, case)


def test_exact_native_inputs_outputs_order_and_terminal_are_required(facts):
    raw = stream(chunks(facts), facts["turn"])
    checked = smoke.check_turn(raw, facts["case"], facts["turn"])
    assert checked == {"tool_calls": 3, "run": "fixture-run", "native": facts["turn"]["native"], "reply": facts["reply"]}
    assert smoke.check_turn(raw.replace("\n", "\r\n"), facts["case"], facts["turn"]) == checked


def test_native_structured_json_content_is_also_supported(facts):
    values = chunks(facts)
    for index in (2, 5, 8):
        values[index]["output"] = [{"type": "json", "value": json.loads(values[index]["output"][0]["text"])}]
    assert smoke.check_turn(stream(values, facts["turn"]), facts["case"], facts["turn"])["tool_calls"] == 3


RESULT_CHANGES = {"fabricated-create", "fabricated-update", "fabricated-run", "unwrapped-output", "extra-content",
                  "wrong-run-id", "wrong-session", "accepted-not-bool", "association-error", "unsafe-run-id"}
INPUT_CHANGES = {"create-definition", "update-definition", "extra-input", "run-marker"}


def alter_tool_result(values, change):
    if change.startswith("fabricated-"):
        index = {"fabricated-create": 2, "fabricated-update": 5, "fabricated-run": 8}[change]
        values[index]["output"][0]["text"] = '{}'
    elif change == "unwrapped-output":
        values[2]["output"] = json.loads(values[2]["output"][0]["text"])
    elif change == "extra-content":
        values[2]["output"].append(values[2]["output"][0].copy())
    else:
        output = json.loads(values[8]["output"][0]["text"])
        edits = {"wrong-run-id": {"run": "invented-run"}, "wrong-session": {"session": "another-session"},
                 "accepted-not-bool": {"accepted": 1}, "association-error": {"association_error": "pending"},
                 "unsafe-run-id": {"run": "../escape"}}
        output.update(edits[change])
        values[8]["output"][0]["text"] = json.dumps(output)


def alter_tool_input(values, change):
    edits = {
        "create-definition": (values[1]["input"]["definition"]["ops"]["receipt"], "run", "printf fabricated"),
        "update-definition": (values[4]["input"]["definition"], "agents", {"hidden": {}}),
        "extra-input": (values[7]["input"], "network", True),
        "run-marker": (values[7]["input"]["input"], "marker", "invented"),
    }
    target, key, value = edits[change]
    target[key] = value


def alter_tool_sequence(values, facts, change):
    if change == "extra-read":
        values.insert(9, {"type": "tool-input-available", "toolName": "graph_read", "toolCallId": "extra"})
    elif change == "extra-start":
        values.insert(9, {"type": "tool-input-start", "toolName": "run_status", "toolCallId": "extra"})
    elif change == "wrong-order":
        values[:] = values[3:6] + values[:3] + values[6:]
    elif change == "concurrent":
        values.insert(1, values.pop(3))
    elif change == "wrong-output-id":
        values[5]["toolCallId"] = "unrelated"
    elif change == "duplicate-call-id":
        for index in (3, 4, 5):
            values[index]["toolCallId"] = values[0]["toolCallId"]
    elif change == "missing-output":
        values.pop(2)
    elif change == "tool-error":
        values[5]["type"] = "tool-output-error"
    elif change == "missing-text":
        values[-1]["delta"] = "  "
    elif change == "early-text":
        values.insert(0, values.pop())
    elif change == "native-run":
        facts["turn"]["native"]["run"] += 1
    else:
        assert change == "stream-error"
        values.insert(9, {"type": "error", "errorText": "failed"})


@pytest.mark.parametrize("change", [
    "fabricated-create", "fabricated-update", "fabricated-run", "extra-read", "extra-start", "wrong-order",
    "concurrent", "wrong-output-id", "duplicate-call-id", "missing-output", "tool-error", "unwrapped-output",
    "extra-content", "create-definition", "update-definition", "extra-input", "run-marker", "missing-text",
    "early-text", "wrong-run-id", "wrong-session", "accepted-not-bool", "association-error", "unsafe-run-id",
    "native-run", "stream-error",
])
def test_fabricated_results_or_extra_actions_fail_closed(facts, change):
    values = chunks(facts)
    if change in RESULT_CHANGES:
        alter_tool_result(values, change)
    elif change in INPUT_CHANGES:
        alter_tool_input(values, change)
    else:
        alter_tool_sequence(values, facts, change)
    with pytest.raises(smoke.SmokeFailure):
        smoke.check_turn(stream(values, facts["turn"]), facts["case"], facts["turn"])


@pytest.mark.parametrize("change", ["failed", "native-missing", "native-scope", "native-bool", "native-overflow",
                                   "runs-missing", "runs-extra", "wrong-request", "wrong-prompt", "terminal-mismatch"])
def test_terminal_and_association_are_not_text_claims(facts, change):
    values = chunks(facts)
    terminal = deepcopy(facts["turn"])
    if change == "failed":
        terminal["status"] = "failed"
    elif change == "native-missing":
        terminal.pop("native")
    elif change == "native-scope":
        terminal["native"]["scope"] = "../private"
    elif change == "native-bool":
        terminal["native"]["session"] = True
    elif change == "native-overflow":
        terminal["native"]["run"] = 2 ** 63
    elif change == "runs-missing":
        terminal["runs"] = []
    elif change == "runs-extra":
        terminal["runs"].append("hidden-run")
    elif change == "wrong-request":
        terminal["request_id"] = "another-request"
    elif change == "wrong-prompt":
        terminal["prompt"] = "another-message"
    expected = facts["turn"] if change == "terminal-mismatch" else terminal
    if change == "terminal-mismatch":
        terminal["native"]["session"] += 1
    with pytest.raises(smoke.SmokeFailure):
        smoke.check_turn(stream(values, terminal), facts["case"], expected)


def test_terminal_must_be_last_unique_and_present(facts):
    raw = stream(chunks(facts), facts["turn"])
    for changed in (raw.split("event: turn")[0], raw + raw, raw + 'data: {"type":"text-delta","delta":"extra"}\n\n'):
        with pytest.raises(smoke.SmokeFailure):
            smoke.check_turn(changed, facts["case"], facts["turn"])


def test_frozen_graph_artifact_and_workspace_use_exact_marker(tmp_path, facts):
    checked = smoke.check_run(tmp_path, facts["case"], facts["turn"], facts["detail"], facts["record"], facts["metadata"])
    assert checked[0]["sha256"] == hashlib.sha256(facts["case"]["marker"].encode()).hexdigest()


def test_expected_snapshot_matches_independent_canonical_dto(facts):
    assert smoke.expected_snapshot(facts["case"]["updated"]) == facts["record"]["snapshot"]
    assert facts["record"]["snapshot"]["ops"]["receipt"]["network"] is False
    assert facts["record"]["snapshot"]["ops"]["receipt"]["wall_time_limit_seconds"] == 30
    facts["record"]["snapshot"]["ops"]["receipt"]["run"] = "true"
    assert smoke.expected_snapshot(facts["case"]["updated"]) != facts["record"]["snapshot"]


@pytest.mark.parametrize("field,value", [("network", 0), ("wall_time_limit_seconds", 3600)])
def test_tool_definitions_reject_incorrect_canonical_defaults(facts, field, value):
    values = chunks(facts)
    values[4]["input"]["definition"]["ops"]["receipt"][field] = value
    with pytest.raises(smoke.SmokeFailure):
        smoke.check_turn(stream(values, facts["turn"]), facts["case"], facts["turn"])
    values = chunks(facts)
    output = json.loads(values[5]["output"][0]["text"])
    output["definition"]["ops"]["receipt"][field] = value
    values[5]["output"][0]["text"] = json.dumps(output)
    with pytest.raises(smoke.SmokeFailure):
        smoke.check_turn(stream(values, facts["turn"]), facts["case"], facts["turn"])


def test_run_completion_waits_for_active_handle_cleanup(tmp_path, monkeypatch):
    details = iter([{"state": {"status": "completed"}, "active": True},
                    {"state": {"status": "completed"}, "active": False}])
    observed = []

    class FixtureApi:
        def expect(self, method, path):
            assert (method, path) == ("GET", "/runs/fixture-run")
            detail = next(details)
            observed.append(detail)
            return detail

    class FixtureService:
        def check(self):
            return None

    monkeypatch.setattr(smoke.time, "sleep", lambda _seconds: None)
    result = smoke.wait_run(FixtureApi(), FixtureService(), "fixture-run", smoke.Evidence(tmp_path, ()), 1)
    assert result["active"] is False
    assert len(observed) == 2
    assert smoke.read_json(tmp_path / "run-detail.json") == result


@pytest.mark.parametrize("change", ["snapshot", "agent", "extra-op", "input", "run-id", "http-id", "active",
                                   "status", "invocation", "plugin", "child-run", "metadata-session", "metadata-turn",
                                   "metadata-owner", "artifact-key", "artifact-marker", "workspace-marker", "manifest-hash"])
def test_run_and_receipt_checks_fail_closed(tmp_path, facts, change):
    record, detail, metadata = facts["record"], facts["detail"], facts["metadata"]
    if change == "snapshot":
        record["snapshot"]["ops"]["receipt"]["run"] = "true"
    elif change == "agent":
        record["snapshot"]["agents"] = {"hidden": {}}
    elif change == "extra-op":
        record["snapshot"]["ops"]["hidden"] = {"run": "true"}
    elif change == "input":
        record["input"] = {"marker": "invented"}
    elif change in {"run-id", "http-id"}:
        target, key = (record, "run_id") if change == "run-id" else (detail, "run")
        target[key] = "invented"
    elif change == "active":
        detail["active"] = True
    elif change == "status":
        record["status"] = "failed"
    elif change == "invocation":
        record["invocations"]["receipt"] = 2
    elif change == "plugin":
        record["plugin_bindings"] = {"hidden": {}}
    elif change == "child-run":
        detail["calls"] = [{"run": "hidden"}]
    elif change.startswith("metadata-"):
        metadata["pilot"][change.removeprefix("metadata-")] = "another"
    elif change == "artifact-key":
        record["results"]["receipt"][0]["key"]["run_id"] = "invented"
    elif change == "artifact-marker":
        (facts["artifact"] / "files/receipt.txt").write_bytes(b"invented")
    elif change == "workspace-marker":
        (facts["workspace"] / "receipt.txt").write_bytes(b"invented")
    else:
        path = facts["artifact"] / "manifest.json"
        manifest = smoke.read_json(path)
        manifest["files"]["receipt.txt"]["sha256"] = "invented"
        save(path, manifest)
    with pytest.raises(smoke.SmokeFailure):
        smoke.check_run(tmp_path, facts["case"], facts["turn"], detail, record, metadata)


@pytest.mark.parametrize("change", [None, "session-id", "session-runs", "locator-session", "locator-root",
                                   "locator-framework", "store-header", "history-reply", "history-prompt"])
def test_public_history_locator_and_native_association_agree(tmp_path, facts, change):
    messages = history(facts)
    if change == "session-id":
        facts["session"]["id"] = "another"
    elif change == "session-runs":
        facts["session"]["run_ids"] = ["another-run"]
    elif change in {"locator-session", "locator-root", "locator-framework"}:
        key, value = {"locator-session": ("session_id", 8), "locator-root": ("root", "/another"),
                      "locator-framework": ("framework", "invented")}[change]
        facts["locator"][key] = value
        save(facts["scope"] / "session.json", facts["locator"])
    elif change == "store-header":
        (facts["scope"] / "framework.sqlite3").write_bytes(b"invented")
    elif change == "history-reply":
        messages["messages"][1]["text"] = "fabricated"
    elif change == "history-prompt":
        messages["messages"][0]["text"] = "fabricated"
    if change is not None:
        with pytest.raises(smoke.SmokeFailure):
            smoke.check_association(tmp_path, facts["turn"], facts["session"], messages, facts["case"], facts["reply"])
    else:
        assert smoke.check_association(tmp_path, facts["turn"], facts["session"], messages,
                                       facts["case"], facts["reply"])["private_sql_read"] is False


@pytest.mark.parametrize("count,complete", [(4, True), (3, True), (5, True), (4, False)])
def test_usage_requires_four_complete_actual_attempts(tmp_path, facts, count, complete):
    for index in range(count):
        add_recording(facts, index + 1, complete)
    save(facts["scope"] / "providers/fixture-turn/provider.call-ids.json", {"not": "an attempt"})
    evidence = smoke.Evidence(tmp_path, ())
    usage = smoke.usage_snapshot(tmp_path, evidence, "usage.json")
    assert usage["provider_attempts"] == count
    if count == 4 and complete:
        smoke.check_usage(usage, facts["turn"])
        assert usage["reported_tokens"]["total_tokens"] == 20
    else:
        with pytest.raises(smoke.SmokeFailure):
            smoke.check_usage(usage, facts["turn"])


@pytest.mark.parametrize("status", ["completed", "failed", "incomplete", "cancelled"])
def test_usage_rejects_non_native_success_status_even_with_metered_tokens(tmp_path, facts, status):
    for index in range(4):
        add_recording(facts, index + 1)
    directory = facts["scope"] / "providers/fixture-turn/provider.recordings/00000000000000000001"
    save(directory / "outcome.json", {"status": status})
    usage = smoke.usage_snapshot(tmp_path, smoke.Evidence(tmp_path, ()), "usage.json")
    assert usage["usage_complete"]
    with pytest.raises(smoke.SmokeFailure):
        smoke.check_usage(usage, facts["turn"])


def test_usage_snapshot_detects_append_and_rewrite_without_private_sql(tmp_path, facts):
    for index in range(4):
        add_recording(facts, index + 1)
    evidence = smoke.Evidence(tmp_path, ())
    before = smoke.usage_snapshot(tmp_path, evidence, "before.json")
    directory = facts["scope"] / "providers/fixture-turn/provider.recordings/00000000000000000001"
    save(directory / "outcome.json", {"status": "succeeded", "rewritten": True})
    assert smoke.usage_snapshot(tmp_path, evidence, "rewritten.json") != before
    add_recording(facts, 5)
    assert smoke.usage_snapshot(tmp_path, evidence, "appended.json")["provider_attempts"] == 5


def test_missing_usage_wrong_turn_and_hidden_graph_requests_fail_closed(tmp_path, facts):
    for index in range(4):
        add_recording(facts, index + 1)
    evidence = smoke.Evidence(tmp_path, ())
    usage = smoke.usage_snapshot(tmp_path, evidence, "usage.json")
    changes = [{"graph_provider_attempts": 1}, {"usage_complete": False}]
    for change in changes:
        with pytest.raises(smoke.SmokeFailure):
            smoke.check_usage({**usage, **change}, facts["turn"])
    usage["attempts"][0]["recording"] = "platform/pilot/another/providers/other/provider.recordings/1"
    with pytest.raises(smoke.SmokeFailure):
        smoke.check_usage(usage, facts["turn"])


def test_prepare_uses_pinned_binary_isolated_roots_and_only_explicit_env(tmp_path, monkeypatch):
    for name in smoke.MODEL_KEYS:
        monkeypatch.setenv(name, "fixture-configured")
    monkeypatch.setenv("ANCHOR_RUNNER_STATE_ROOT", "/production/state")
    monkeypatch.setenv("ANCHOR_RUNNER_ALLOWED_COMMANDS", "curl,python")
    monkeypatch.setenv("WECOM_SECRET", "must-not-inherit")
    source = tmp_path / "source-host"
    source.write_bytes(b"fixture-binary-not-executed")
    root = tmp_path / "evidence"
    root.mkdir()
    binary, case, env, _port = smoke.prepare(argparse.Namespace(binary=source, timeout=1), smoke.Evidence(root, ()))
    assert smoke.file_sha256(binary) == smoke.file_sha256(source)
    assert binary.stat().st_mode & 0o777 == 0o700
    assert "WECOM_SECRET" not in env
    assert env["ANCHOR_RUNNER_ALLOWED_COMMANDS"] == "sh,cat,printf,cp,git,cmp"
    assert env["ANCHOR_RUNNER_STATE_ROOT"] == str(root / "state")
    assert env["ANCHOR_RUNNER_CATALOG_ROOT"] == str(root / "catalog")
    assert case["initial"]["agents"] == case["updated"]["agents"] == {}
    assert case["initial"]["ops"]["receipt"] == {"run": "true", "network": False, "wall_time_limit_seconds": 30}
    assert case["updated"]["ops"]["receipt"]["run"] == f"sh -c 'printf {case['marker']} > receipt.txt'"
    assert case["updated"]["ops"]["receipt"]["network"] is False
    assert case["updated"]["ops"]["receipt"]["wall_time_limit_seconds"] == 30
    assert "ANCHOR_RUNNER_LIBRARY_ROOT" not in env
    assert smoke.read_json(root / "bundle/manifest.json")["plugins"] == []


class FixtureHost:
    def __init__(self, root, deviation):
        self.root = root
        self.deviation = deviation
        self.state = {"services": [], "submissions": 0, "facts": None}

    def raw(self, method, path, body=None):
        state, root, deviation = self.state, self.root, self.deviation
        if path == "/health":
            value, status = {}, 200
        elif path == "/sessions" and method == "POST":
            state["facts"] = fixture_facts(root, smoke.read_json(root / "case.json"), body["id"])
            value, status = {"session": state["facts"]["session"]}, 201
        elif method == "POST" and path.endswith("/turns"):
            state["submissions"] += 1
            if state["submissions"] == 1:
                for index in range(4):
                    add_recording(state["facts"], index + 1, deviation != "incomplete-usage")
            elif deviation == "replay-request":
                add_recording(state["facts"], 5)
            value, status = {"turn": state["facts"]["turn"]}, 202
        elif path.endswith("/events"):
            values = chunks(state["facts"])
            if deviation == "extra-tool":
                values.insert(9, {"type": "tool-input-start", "toolName": "graph_read", "toolCallId": "extra"})
            return 200, stream(values, state["facts"]["turn"]).encode()
        elif "/turns/" in path:
            turn = deepcopy(state["facts"]["turn"])
            if deviation == "restart-association" and len(state["services"]) == 2:
                turn["native"]["run"] += 1
            value, status = {"turn": turn}, 200
        elif path.endswith("/messages"):
            value, status = history(state["facts"]), 200
        elif path.startswith("/sessions/"):
            value, status = {"session": state["facts"]["session"]}, 200
        elif path.startswith("/graphs/"):
            case = state["facts"]["case"]
            value, status = {"graph": case["graph"], "definition": case["updated"]}, 200
        elif path.startswith("/runs/"):
            value, status = state["facts"]["detail"], 200
        else:
            pytest.fail(f"Unexpected fake HTTP request: {method} {path}")
        return status, json.dumps(value).encode()


@pytest.mark.parametrize("deviation", [None, "extra-tool", "replay-request", "restart-association", "incomplete-usage"])
def test_driver_replay_restart_and_failures_without_any_provider(tmp_path, monkeypatch, deviation):
    for name in smoke.MODEL_KEYS:
        monkeypatch.setenv(name, "fixture-configured")
    source = tmp_path / "source-host"
    source.write_bytes(b"fixture-binary-never-executed")
    root = tmp_path / "evidence"
    root.mkdir()
    host = FixtureHost(root, deviation)
    state = host.state

    class FixtureService:
        def __init__(self, label, command, env, evidence):
            state["services"].append(label)

        def check(self):
            return None

        def stop(self):
            return None

    monkeypatch.setattr(smoke, "Service", FixtureService)
    monkeypatch.setattr(smoke.Api, "raw", lambda _api, method, path, body=None: host.raw(method, path, body))
    report = smoke.run(argparse.Namespace(binary=source, timeout=1), smoke.Evidence(root, ("fixture-configured",)))
    assert report["status"] == ("passed" if deviation is None else "failed")
    assert report["model_retry"] is False
    assert (root / "turn-sse.json").is_file()
    assert (root / "usage-final.json").is_file()
    assert (root / "provider-01-recording.json").is_file()
    assert "fixture-configured" not in (root / "host-config.json").read_text()
    assert smoke.read_json(root / "evidence.json") == report
    if deviation is None:
        assert state["services"] == ["host", "host-restarted"]
        assert state["submissions"] == 3
        assert report["provider_attempts"] == 4 and report["idempotency"] and report["restart"]
        assert smoke.read_json(root / "usage-before-replay.json") == smoke.read_json(root / "usage-after-restart.json")
    elif deviation in {"extra-tool", "incomplete-usage"}:
        assert state["submissions"] == 1


@pytest.mark.parametrize("option", ["help", "missing-binary", "missing-model", "existing-root", "bad-timeout"])
def test_cli_rejects_unsafe_configuration_before_starting_host(tmp_path, monkeypatch, capsys, option):
    binary = tmp_path / "host"
    binary.write_bytes(b"fixture-only")
    root = tmp_path / "evidence"
    arguments = ["mutation-smoke", "--binary", str(binary), "--evidence-root", str(root)]
    for name in smoke.MODEL_KEYS:
        monkeypatch.setenv(name, "fixture-configured")
    if option == "help":
        arguments = ["mutation-smoke", "--help"]
    elif option == "missing-binary":
        arguments = ["mutation-smoke"]
    elif option == "missing-model":
        monkeypatch.delenv(smoke.MODEL_KEYS[0])
    elif option == "existing-root":
        root.mkdir()
    else:
        arguments += ["--timeout", "nan"]
    monkeypatch.setattr(sys, "argv", arguments)
    monkeypatch.setattr(smoke, "run", lambda *_args: pytest.fail("Host/provider must not start"))
    with pytest.raises(SystemExit) as error:
        smoke.main()
    assert error.value.code == (0 if option == "help" else 2)
    assert root.exists() == (option == "existing-root")
    if option == "help":
        assert "--binary" in capsys.readouterr().out
