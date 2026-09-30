"""Independent calls exercise the real command runner and persisted service boundary."""
from __future__ import annotations

import json
import threading
import time

import pytest

from anchor.graph_calls import _file, _pointer
from anchor.serve import Scheduler
from anchor.simple import run as runner


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value))


def command(text="printf result > report.txt", **extra):
    return {"ops": {"work": {"run": text}}, "nodes": [{"id": "work", "op": "work"}],
            "edges": [], **extra}


@pytest.fixture
def setup(tmp_path):
    config = tmp_path / "config.json"
    write(config, {"models": []})
    for name in ("parent", "child"):
        write(tmp_path / "workspaces" / name / "graph.json", command())
    scheduler = Scheduler(tmp_path, config)
    source = scheduler.workspace("parent")
    parent = source / "runs" / "parent-run"
    parent.mkdir(parents=True)
    runner.RunState(objective="caller", started=runner._now(), status="finished").save(parent)
    return scheduler, source, parent


def invoke(setup, spec=None, invocation=1, **options):
    scheduler, source, parent = setup
    return scheduler.graph_calls.invoke(
        source_workspace=source, source_run_id=parent.name,
        spec=spec or {"graph": "child", "mode": "wait"}, node_id="invoke", invocation=invocation,
        directory=parent / "invoke", control=parent / "control" / f"invoke-{invocation}",
        run_input=options.get("run_input", {}), inputs=options.get("inputs", ()),
        cancelled=options.get("cancelled", lambda: False))


def settled(scheduler, identifier):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        directory = scheduler.run_dir(identifier)
        if directory is not None:
            state = runner.RunState.load(directory)
            if identifier not in scheduler.graph_calls.active and state.status != "running":
                return state
        time.sleep(.03)
    pytest.fail(f"Run did not settle: {identifier}")


def test_wait_runs_real_command_and_returns_pinned_result(setup):
    scheduler, _, parent = setup
    answer = invoke(setup, {"graph": "child", "mode": "wait",
                           "result": {"node": "work", "files": ["report.txt"]}})
    assert answer["status"] == "finished"
    assert (parent / "invoke/result/report.txt").read_text() == "result"
    assert answer["result"]["commit"]
    child = scheduler.run_dir(answer["run"])
    (child / "work/report.txt").write_text("later mutable output")
    second = invoke(setup, {"graph": "child", "mode": "wait"})
    assert second == answer
    assert (parent / "invoke/result/report.txt").read_text() == "result"
    assert len(list((child.parent).glob("*/run.json"))) == 1
    assert scheduler.run("parent", parent.name)["calls"][0]["status"] == "finished"


def test_selected_files_input_mapping_and_defaults(setup, tmp_path):
    scheduler, _, _ = setup
    write(scheduler.workspace("child") / "graph.json",
          command("cat /in/call/report.txt > report.txt", input={"default": 4}))
    source = tmp_path / "snapshot"
    source.mkdir()
    (source / "report.txt").write_text("frozen report")
    (source / "private.txt").write_text("never forward")
    answer = invoke(setup, {"graph": "child", "mode": "wait", "input": {"constant": True},
                           "input_map": {"code": "/request/code"},
                           "files": [{"node": "source", "path": "report.txt", "as": "report.txt"}],
                           "result": {"node": "work", "files": ["report.txt"]}},
                    run_input={"request": {"code": "X"}, "secret": "omit"},
                    inputs=({"node": "source", "tree": str(source), "commit": "trusted"},))
    child = scheduler.run_dir(answer["run"])
    assert runner.RunState.load(child).input == {"default": 4, "constant": True, "code": "X"}
    assert not (child / "call-inputs/private.txt").exists()
    assert (child / "work/report.txt").read_text() == "frozen report"


def test_detach_accepts_concurrent_runs_and_retry_is_same_child(setup):
    scheduler, _, _ = setup
    write(scheduler.workspace("child") / "graph.json", command("sleep 0.5; printf done > report.txt"))
    spec = {"graph": "child", "mode": "detach"}
    first, second = invoke(setup, spec), invoke(setup, spec, invocation=2)
    assert first["status"] == second["status"] == "accepted"
    assert first["run"] != second["run"]
    assert set(scheduler.active_runs("child")) == {first["run"], second["run"]}
    assert invoke(setup, spec) == first
    assert scheduler.trigger("child", None)[1] == 409
    assert scheduler.save("child", command())[1] == 409
    assert scheduler.delete_graph("child")[1] == 409
    for item in (first, second):
        assert settled(scheduler, item["run"]).status == "finished"


def test_wait_child_failure_fails_call_but_detach_keeps_acceptance(setup):
    scheduler, _, _ = setup
    write(scheduler.workspace("child") / "graph.json", command("exit 7"))
    with pytest.raises(RuntimeError, match="failed"):
        invoke(setup)
    detached = invoke(setup, {"graph": "child", "mode": "detach"}, invocation=2)
    assert detached["status"] == "accepted"
    assert settled(scheduler, detached["run"]).status == "failed"


def test_wait_cancel_only_stops_its_own_child(setup):
    scheduler, _, _ = setup
    write(scheduler.workspace("child") / "graph.json", command("sleep 0.7; printf done > report.txt"))
    independent = invoke(setup, {"graph": "child", "mode": "detach"}, invocation=2)
    cancel = threading.Event()
    errors = []
    def wait():
        try:
            invoke(setup, cancelled=cancel.is_set)
        except RuntimeError as exc:
            errors.append(str(exc))
    worker = threading.Thread(target=wait)
    worker.start()
    deadline = time.monotonic() + 5
    while len(scheduler.active_runs("child")) < 2 and time.monotonic() < deadline:
        time.sleep(.01)
    own = next(identifier for identifier in scheduler.active_runs("child") if identifier != independent["run"])
    cancel.set()
    worker.join(5)
    assert errors and "cancelled" in errors[0]
    assert settled(scheduler, own).status == "stopped"
    assert settled(scheduler, independent["run"]).status == "finished"


def test_orphan_admission_recovers_same_snapshot_after_restart(setup, monkeypatch):
    scheduler, _, _ = setup
    original = runner.RunState.save
    def crash(state, path):
        if path.name.startswith("call-"):
            raise KeyboardInterrupt("crash after admission")
        return original(state, path)
    monkeypatch.setattr(runner.RunState, "save", crash)
    with pytest.raises(KeyboardInterrupt):
        invoke(setup, {"graph": "child", "mode": "detach", "input": {"original": 1}})
    monkeypatch.setattr(runner.RunState, "save", original)
    child = next((scheduler.workspace("child") / "runs").iterdir())
    assert (child / "admission.json").exists() and not (child / "run.json").exists()
    write(scheduler.workspace("child") / "graph.json", command("exit 9"))
    restarted = Scheduler(scheduler.root, scheduler.config)
    restarted.resume_all()
    assert settled(restarted, child.name).status == "finished"
    assert runner.RunState.load(child).input == {"original": 1}
    assert (child / "work/report.txt").read_text() == "result"
    assert len(list(child.parent.iterdir())) == 1


def test_save_targets_relations_and_referenced_runs_are_protected(setup):
    scheduler, source, parent = setup
    definition = {"ops": {"invoke": {"call": {"graph": "child", "mode": "wait"}}},
                  "nodes": [{"id": "invoke", "op": "invoke"}], "edges": []}
    assert scheduler.save("parent", definition)[1] == 200
    assert scheduler.graph_calls.relations()["calls"] == [{"graph": "parent", "node": "invoke",
                                                          "op": "invoke", "target": "child", "mode": "wait"}]
    assert scheduler.delete_graph("child")[1] == 409
    answer = invoke(setup)
    assert scheduler.delete_run(answer["run"])[1] == 409
    assert scheduler.delete_run(parent.name)[1] == 409
    assert scheduler.delete_graph(source.name)[1] == 409
    definition["ops"]["invoke"]["call"]["graph"] = "missing"
    assert scheduler.save("parent", definition)[1] == 400


def test_ancestry_rejects_recursion_but_separate_invocations_are_allowed(setup):
    with pytest.raises(ValueError, match="recursive"):
        invoke(setup, {"graph": "parent", "mode": "detach"})
    scheduler, source, parent = setup
    state = runner.RunState.load(parent)
    state.trigger = {"source": "graph_call", "graph": "child", "run": "ancestor", "root_run": "ancestor"}
    state.save(parent)
    with pytest.raises(ValueError, match="recursive"):
        invoke(setup)


@pytest.mark.parametrize("path", ["../outside", "/absolute", ".git/config", "a/../b"])
def test_selected_files_reject_unsafe_paths(tmp_path, path):
    with pytest.raises(ValueError):
        _file(tmp_path, path)


def test_selected_files_reject_symlinks_and_invisible_sources(setup, tmp_path):
    (tmp_path / "real").write_text("secret")
    (tmp_path / "link").symlink_to(tmp_path / "real")
    with pytest.raises(ValueError, match="symlink"):
        _file(tmp_path, "link")
    with pytest.raises(ValueError, match="not visible"):
        invoke(setup, {"graph": "child", "mode": "detach",
                       "files": [{"node": "not-upstream", "path": "real", "as": "real"}]})


def test_json_pointer_escaping_and_missing_values():
    assert _pointer({"a/b": {"~x": [3]}}, "/a~1b/~0x/0") == 3
    for pointer in ("/missing", "/items/01", "/a~2"):
        with pytest.raises(ValueError):
            _pointer({"items": [1]}, pointer)


def test_parent_resume_reuses_only_child_stopped_by_parent(setup, monkeypatch):
    scheduler, _, parent = setup
    write(scheduler.workspace("child") / "graph.json", command("sleep .1; printf done > report.txt"))
    entered, release = threading.Event(), threading.Event()
    original_config = runner._config
    def config(path):
        entered.set()
        assert release.wait(5)
        return original_config(path)
    monkeypatch.setattr(runner, "_config", config)
    cancel = threading.Event()
    errors = []
    def waiting():
        try:
            invoke(setup, cancelled=cancel.is_set)
        except RuntimeError as exc:
            errors.append(str(exc))
    worker = threading.Thread(target=waiting)
    worker.start()
    deadline = time.monotonic() + 5
    while not scheduler.active_runs("child") and time.monotonic() < deadline:
        time.sleep(.01)
    identifier = scheduler.active_runs("child")[0]
    assert entered.wait(5)
    cancel.set()
    deadline = time.monotonic() + 5
    while scheduler.control.get(identifier) != "stopped" and time.monotonic() < deadline:
        time.sleep(.01)
    release.set()
    worker.join(5)
    assert errors
    assert settled(scheduler, identifier).status == "stopped"
    answer = invoke(setup)
    assert answer["run"] == identifier and answer["status"] == "finished"
    assert len(list((scheduler.workspace("child") / "runs").glob("*/run.json"))) == 1


def test_operator_stopped_child_is_not_resumed_by_parent(setup):
    scheduler, _, _ = setup
    write(scheduler.workspace("child") / "graph.json", command("sleep .5"))
    answer = invoke(setup, {"graph": "child", "mode": "detach"})
    scheduler.control_run(answer["run"], "stop")
    assert settled(scheduler, answer["run"]).status == "stopped"
    # Use its original wait semantics without changing child identity or injecting cancellation.
    path = setup[2] / "control/invoke-1/graph-call.json"
    record = json.loads(path.read_text())
    record["mode"] = record["spec"]["mode"] = "wait"
    write(path, record)
    with pytest.raises(RuntimeError, match="stopped"):
        invoke(setup)
    assert not scheduler.active_runs("child")


def test_mismatched_persisted_call_source_fails_closed(setup):
    answer = invoke(setup, {"graph": "child", "mode": "detach"})
    settled(setup[0], answer["run"])
    path = setup[2] / "control/invoke-1/graph-call.json"
    record = json.loads(path.read_text())
    record["trigger"]["invocation"] = 9
    write(path, record)
    with pytest.raises(ValueError, match="different source invocation"):
        invoke(setup)


def test_pending_session_admission_protects_deletion(setup):
    scheduler, _, _ = setup
    session = scheduler.sessions.create("protected", graph="child", channel={"source": "wecom"})
    path = scheduler.workspace("child") / "runs/pending/admission.json"
    write(path, {"spec": {"session": session.id}, "session_pending": True})
    assert scheduler.delete_session(session.id)[1] == 409
    assert scheduler.sessions.get(session.id)


def test_failed_file_selection_retry_does_not_expose_unselected_bytes(setup, tmp_path):
    scheduler, _, _ = setup
    snapshot = tmp_path / "upstream"
    snapshot.mkdir()
    (snapshot / "first").write_text("first")
    spec = {"graph": "child", "mode": "detach", "files": [
        {"node": "source", "path": "first", "as": "stale"},
        {"node": "source", "path": "missing", "as": "missing"}]}
    with pytest.raises(ValueError, match="does not exist"):
        invoke(setup, spec, inputs=({"node": "source", "tree": str(snapshot)},))
    answer = invoke(setup, {"graph": "child", "mode": "detach"})
    child = scheduler.run_dir(answer["run"])
    assert not (child / "call-inputs/stale").exists()
    settled(scheduler, answer["run"])


def test_real_parent_graph_call_uses_native_completion_and_child_run(setup):
    scheduler, source, _ = setup
    write(source / "graph.json", {
        "ops": {"invoke": {"call": {"graph": "child", "mode": "wait",
                                      "result": {"node": "work", "files": ["report.txt"]}}}},
        "nodes": [{"id": "invoke", "op": "invoke"}], "edges": []})
    state = runner.run(source, config_path=scheduler.config, run_id="actual-parent",
                       call_handler=scheduler.graph_calls.factory(source, "actual-parent"))
    assert state.status == "finished"
    result = state.result("invoke")
    assert result.submitted and result.commit
    payload = json.loads(result.submission)
    child = runner.RunState.load(scheduler.run_dir(payload["run"]))
    assert child.trigger["run"] == "actual-parent"
    assert child.trigger["root_run"] == "actual-parent"
    assert child.trigger["node"] == "invoke"
    assert child.status == "finished"
    projected = scheduler.run("parent", "actual-parent")["calls"]
    assert projected[0]["result"] == payload["result"]
    assert projected[0]["result"]["files"] == ["report.txt"]


def test_interrupted_command_child_resumes_same_run_but_stays_uncertain(setup):
    scheduler, _, _ = setup
    write(scheduler.workspace("child") / "graph.json",
          command("printf started > side-effect.txt; sleep 10; printf done > report.txt"))
    cancel = threading.Event()
    errors = []
    def waiting():
        try:
            invoke(setup, cancelled=cancel.is_set)
        except RuntimeError as exc:
            errors.append(str(exc))
    worker = threading.Thread(target=waiting)
    worker.start()
    deadline = time.monotonic() + 10
    child = None
    while time.monotonic() < deadline:
        runs = list((scheduler.workspace("child") / "runs").glob("*/work/side-effect.txt"))
        if runs:
            child = runs[0].parent.parent
            break
        time.sleep(.02)
    assert child is not None
    cancel.set()
    worker.join(5)
    assert errors
    assert settled(scheduler, child.name).status == "stopped"
    with pytest.raises(RuntimeError, match="Uncertain"):
        invoke(setup)
    assert len(list(child.parent.glob("*/run.json"))) == 1
    assert not (child / "work/report.txt").exists()
