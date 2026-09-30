"""Graph calls use native pass identity and completion, without shell or model dispatch."""
from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace

import pytest

from anchor.node.recovery import read_completion_fact
from anchor.simple import graph, run as runner


def definition(call=None):
    return {"ops": {"invoke": {"call": call or {"graph": "child", "mode": "wait"}}},
            "nodes": [{"id": "invoke", "op": "invoke"}], "edges": []}


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value), encoding="utf-8")


@pytest.fixture
def configured(tmp_path, monkeypatch):
    workspace = tmp_path / "parent"
    write(workspace / "graph.json", definition())
    monkeypatch.setattr(runner, "_config", lambda _: ({}, None))
    monkeypatch.setattr(runner, "_agent_for", lambda *a, **kw: pytest.fail("call entered command/model factory"))
    return workspace


def accept(**kwargs):
    spec = kwargs["spec"]
    result = {"graph": spec["graph"], "run": f"child-{kwargs['invocation']}", "mode": spec["mode"],
              "status": "finished" if spec["mode"] == "wait" else "accepted"}
    write(kwargs["directory"] / "call.json", result)
    for name in spec.get("result", {}).get("files", ()):
        path = kwargs["directory"] / "result" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("selected child output")
    return result


def test_call_schema_roundtrip_advertises_result_files():
    call = {"graph": "child", "mode": "wait", "input": {"nested": {"a": 1}},
            "input_map": {"request": "/a~1b/~0key"}, "session": "chosen-session",
            "result": {"node": "finish", "files": ["report/a.md"]}}
    parsed = graph.parse(definition(call))
    assert parsed.ops["invoke"].run == ""
    assert parsed.writes("invoke") == ("call.json", "result/report/a.md")
    assert graph.parse(graph.to_dict(parsed)) == parsed
    assert "run" not in graph.to_dict(parsed)["ops"]["invoke"]
    call["input"]["nested"]["a"] = 2
    assert parsed.ops["invoke"].call["input"]["nested"]["a"] == 1


@pytest.mark.parametrize("call", [None, [], {}, {"graph": "child"},
    {"graph": "child", "mode": "sync"}, {"graph": "../child", "mode": "wait"},
    {"graph": "child/other", "mode": "wait"}, {"graph": "child\\other", "mode": "wait"},
    {"graph": "child", "mode": "wait", "unknown": 1},
    {"graph": "child", "mode": "wait", "input": []},
    {"graph": "child", "mode": "wait", "input_map": {"x": "x.y"}},
    {"graph": "child", "mode": "wait", "input_map": {"x": "/x~2"}},
    {"graph": "child", "mode": "wait", "files": {}},
    {"graph": "child", "mode": "wait", "files": [{"node": "a", "path": "../a", "as": "a"}]},
    {"graph": "child", "mode": "wait", "files": [{"node": "a", "path": ".git/config", "as": "a"}]},
    {"graph": "child", "mode": "wait", "files": [{"node": "a", "path": "a", "as": "/a"}]},
    {"graph": "child", "mode": "wait", "result": {"node": "a", "files": ["../a"]}},
    {"graph": "child", "mode": "detach", "result": {"node": "a"}},
    {"graph": "child", "mode": "wait", "session": ""},
])
def test_rejects_invalid_call_schema(call):
    raw = definition()
    raw["ops"]["invoke"]["call"] = call
    with pytest.raises(ValueError):
        graph.parse(raw)


@pytest.mark.parametrize("spec", [{}, {"run": "echo hello", "call": {"graph": "child", "mode": "wait"}},
                                  {"run": "", "call": {"graph": "child", "mode": "wait"}}])
def test_command_and_call_are_mutually_exclusive(spec):
    raw = definition()
    raw["ops"]["invoke"] = spec
    with pytest.raises(ValueError, match="exactly one"):
        graph.parse(raw)


def test_selected_files_require_visible_upstream_nodes():
    raw = definition({"graph": "child", "mode": "wait",
                      "files": [{"node": "unrelated", "path": "report.md", "as": "report.md"}]})
    raw["ops"]["source"] = {"run": "true"}
    raw["nodes"].append({"id": "unrelated", "op": "source"})
    raw["entry"] = "invoke"
    with pytest.raises(ValueError, match="not upstream"):
        graph.parse(raw)
    raw["entry"] = "unrelated"
    raw["edges"] = [{"from": "unrelated", "to": "invoke"}]
    graph.parse(raw)


def test_selected_file_aliases_cannot_overlap():
    raw = definition({"graph": "child", "mode": "wait", "files": [
        {"node": "a", "path": "first", "as": "report"},
        {"node": "a", "path": "second", "as": "report/nested"}]})
    with pytest.raises(ValueError, match="overlapping"):
        graph.parse(raw)


def test_call_cannot_silently_drop_multiple_routes():
    raw = definition()
    raw["nodes"] += [{"id": name, "op": "invoke"} for name in ("a", "b")]
    raw["edges"] = [{"from": "invoke", "to": name} for name in ("a", "b")]
    with pytest.raises(ValueError, match="multiple outgoing"):
        graph.parse(raw)


def test_native_success_and_exact_keyword_contract(configured):
    seen = []
    def handler(**kwargs):
        seen.append(kwargs)
        return accept(**kwargs)
    state = runner.run(configured, config_path="unused", run_id="proof", run_input={"a": 1}, call_handler=handler)
    assert state.status == "finished"
    assert len(seen) == 1
    assert set(seen[0]) == {"spec", "node_id", "invocation", "directory", "control", "run_input", "inputs", "cancelled"}
    assert seen[0]["invocation"] == 1 and seen[0]["inputs"] == ()
    assert seen[0]["run_input"] == {"a": 1} and seen[0]["cancelled"]() is False
    assert state.result("invoke").submitted and state.result("invoke").commit
    fact = read_completion_fact(configured / "runs/proof/control/invoke", "invoke")
    assert fact is not None and fact.kind == "graph_call"
    assert json.loads(fact.submission)["run"] == "child-1"


def test_no_handler_and_callback_failure_are_failed_nodes(configured):
    state = runner.run(configured, config_path="unused", run_id="no-service")
    assert state.status == "failed"
    assert "service call_handler" in state.result("invoke").submission
    def fail(**kwargs):
        raise ValueError("child failed")
    state = runner.run(configured, config_path="unused", run_id="failed-child", call_handler=fail)
    assert state.status == "failed" and "child failed" in state.result("invoke").submission
    assert not (configured / "runs/failed-child/control/invoke/completion.json").exists()


def test_completion_recovers_gap_before_graph_commit_without_recalling(configured, monkeypatch):
    original = runner._record
    def crash(*args, **kwargs):
        raise RuntimeError("process interrupted after native completion")
    monkeypatch.setattr(runner, "_record", crash)
    with pytest.raises(RuntimeError):
        runner.run(configured, config_path="unused", run_id="proof", call_handler=accept)
    monkeypatch.setattr(runner, "_record", original)
    state = runner.run(configured, config_path="unused", resume=configured / "runs/proof",
                       call_handler=lambda **_: pytest.fail("already completed call repeated"))
    assert state.status == "finished" and state.runs == {"invoke": 1}


def test_resume_before_completion_keeps_invocation_and_snapshot(configured):
    seen = []
    def interrupted(**kwargs):
        seen.append((kwargs["invocation"], kwargs["spec"]["graph"], kwargs["control"]))
        raise KeyboardInterrupt("simulated process death after admission")
    with pytest.raises(KeyboardInterrupt):
        runner.run(configured, config_path="unused", run_id="proof", call_handler=interrupted)
    snapshot = (configured / "runs/proof/graph.json").read_bytes()
    (configured / "graph.json").unlink()
    def resumed(**kwargs):
        seen.append((kwargs["invocation"], kwargs["spec"]["graph"], kwargs["control"]))
        return accept(**kwargs)
    state = runner.run(configured, config_path="unused", resume=configured / "runs/proof",
                       definition=definition({"graph": "replacement", "mode": "wait"}), call_handler=resumed)
    assert state.status == "finished" and seen[0] == seen[1]
    assert (configured / "runs/proof/graph.json").read_bytes() == snapshot


def test_definition_override_pins_initial_admission(configured):
    state = runner.run(configured, config_path="unused", run_id="admitted", call_handler=accept,
                       definition=definition({"graph": "admitted-target", "mode": "detach"}))
    assert state.status == "finished"
    assert json.loads(state.result("invoke").submission)["graph"] == "admitted-target"
    assert graph.load(configured / "runs/admitted/graph.json").ops["invoke"].call["mode"] == "detach"


def test_module_reentry_uses_global_invocation_and_roundtrips_snapshot(configured):
    raw = {"ops": {"invoke": {"call": {"graph": "child", "mode": "detach"}}},
           "entry": "outer", "max_rounds": 2,
           "graphs": {"inner": {"entry": "call", "exit": "call", "max_rounds": 1,
                                  "nodes": [{"id": "call", "op": "invoke"}]}},
           "nodes": [{"id": "outer", "op": "invoke"}, {"id": "module", "graph": "inner", "max_rounds": 2}],
           "edges": [{"from": "outer", "to": "module"}, {"from": "module", "to": "outer"}]}
    write(configured / "graph.json", raw)
    parsed = graph.parse(raw)
    assert graph.parse(graph.to_dict(parsed)) == parsed
    seen = []
    def handler(**kwargs):
        seen.append((kwargs["node_id"], kwargs["invocation"], kwargs["control"].name))
        return accept(**kwargs)
    state = runner.run(configured, config_path="unused", run_id="proof", call_handler=handler)
    assert seen == [("outer", 1, "outer"), ("module/call", 1, "call"),
                    ("outer", 2, "outer-2"), ("module/call", 2, "call-2")]
    assert state.passes["module/call"] == 1 and state.runs["module/call"] == 2


def test_result_file_interface_and_upstream_snapshot(configured):
    raw = definition({"graph": "child", "mode": "wait", "result": {"node": "done", "files": ["report.md"]}})
    raw["ops"]["next"] = {"call": {"graph": "other", "mode": "detach", "files": [
        {"node": "invoke", "path": "result/report.md", "as": "report.md"}]},
        "reads": ["call.json", "result/report.md"]}
    raw["nodes"].append({"id": "next", "op": "next"})
    raw["edges"] = [{"from": "invoke", "to": "next"}]
    write(configured / "graph.json", raw)
    def handler(**kwargs):
        if kwargs["node_id"] == "next":
            given, = kwargs["inputs"]
            assert given["node"] == "invoke" and len(given["commit"]) == 40
            assert (Path(given["tree"]) / "result/report.md").read_text() == "selected child output"
            assert Path(given["tree"]) != configured / "runs/proof/invoke"
        return accept(**kwargs)
    state = runner.run(configured, config_path="unused", run_id="proof", call_handler=handler)
    assert state.status == "finished" and state.executed == ["invoke", "next"]


def test_missing_selected_result_never_records_completion(configured):
    raw = definition({"graph": "child", "mode": "wait", "result": {"node": "done", "files": ["report.md"]}})
    write(configured / "graph.json", raw)
    def missing(**kwargs):
        result = accept(**kwargs)
        (kwargs["directory"] / "result/report.md").unlink()
        return result
    state = runner.run(configured, config_path="unused", run_id="proof", call_handler=missing)
    assert state.status == "failed" and "required file" in state.result("invoke").submission
    assert not (configured / "runs/proof/control/invoke/completion.json").exists()


def test_admitted_call_input_bundle_is_mounted_on_resume(configured, monkeypatch):
    raw = {"ops": {"work": {"run": "true"}}, "nodes": [{"id": "work", "op": "work"}]}
    write(configured / "graph.json", raw)
    bundle = configured / "runs/proof/call-inputs"
    bundle.mkdir(parents=True)
    (bundle / "report.md").write_text("input")
    observed = []
    def factory(*args, **kwargs):
        observed.append(kwargs["resources"])
        return SimpleNamespace(env=SimpleNamespace(route=None),
            run=lambda **_: {"submission": "done", "exit_status": "Submitted"})
    monkeypatch.setattr(runner, "_agent_for", factory)
    state = runner.run(configured, config_path="unused", run_id="proof", stop_request=lambda: "paused")
    assert state.status == "paused"
    state = runner.run(configured, config_path="unused", resume=configured / "runs/proof")
    assert state.status == "finished" and observed == [((str(bundle), "/in/call"),)]
