"""Plugins are references, lazy instructions and real read-only resources in the existing sandbox."""

import json
import sys
import venv

import pytest

from anchor.library import Library, record_bindings, _mount
from anchor.runtime.execenv import NodeSandbox
from anchor.serve import Scheduler
from anchor.simple import graph as graph_module
from anchor.simple import run as runner


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value), encoding="utf-8")


def library_at(root):
    plugin = root / "library/plugins/method"
    write_json(plugin / "plugin.json", {"name": "Method", "description": "Find the evidence", "tools": []})
    (plugin / "instructions.md").write_text("ONLY_READ_WHEN_NEEDED\n", encoding="utf-8")
    return Library(root / "library")


def definition(plugins=None):
    return {"agents": {"a": {"model": "test", "instructions": "Use your Plugin."}},
            "nodes": [{"id": "a", "agent": "a", "plugins": plugins or ["method"]}], "edges": []}


def test_plugin_schema_roundtrip_and_node_ownership():
    parsed = graph_module.parse(definition())
    assert graph_module.parse(graph_module.to_dict(parsed)).nodes["a"].plugins == ("method",)
    raw = definition()
    raw["graphs"] = {"inner": {"entry": "a", "exit": "a", "nodes": raw["nodes"], "edges": []}}
    raw["nodes"] = [{"id": "scope", "graph": "inner"}]
    assert graph_module.parse(raw).nodes["scope/a"].plugins == ("method",)
    for invalid in ("method", ["../other"], ["/root"], [False]):
        with pytest.raises(ValueError):
            graph_module.parse(definition(invalid))
    with pytest.raises(ValueError, match="only.*AgentNode"):
        graph_module.parse({"ops": {"x": {"run": "true"}},
                            "nodes": [{"id": "x", "op": "x", "plugins": ["method"]}]})
    raw = definition()
    raw["agents"]["a"]["plugins"] = ["method"]
    with pytest.raises(ValueError, match="not the shared role"):
        graph_module.parse(raw)


def test_lazy_disclosure_missing_resources_and_history(tmp_path):
    library = library_at(tmp_path)
    attached = library.attach(("method",))
    assert "Find the evidence" in attached.instructions
    assert "/plugins/method/instructions.md" in attached.instructions
    assert "ONLY_READ_WHEN_NEEDED" not in attached.instructions
    assert "ONLY_READ_WHEN_NEEDED" in library.detail("method")["instructions"]
    assert library.file("method", "instructions.md").read_text() == "ONLY_READ_WHEN_NEEDED\n"
    with pytest.raises(ValueError):
        library.file("method", "../../tools/secret")
    assert library.attach(()).binds == ()
    with pytest.raises(ValueError):
        library.attach(("unknown",))
    run = tmp_path / "run"
    run.mkdir()
    record_bindings(run, {"a": attached}, resume=False)
    record_bindings(run, {"a": attached}, resume=True)
    original = (run / "plugins.json").read_bytes()
    (library.root / "plugins/method/instructions.md").write_text("changed")
    with pytest.raises(ValueError, match="changed"):
        record_bindings(run, {"a": library.attach(("method",))}, resume=True)
    assert (run / "plugins.json").read_bytes() == original
    assert "ONLY_READ_WHEN_NEEDED" not in original.decode()
    (library.root / "plugins/method/escape").symlink_to(tmp_path)
    with pytest.raises(ValueError, match="symlinks"):
        library.attach(("method",))
    assert library.catalog()[0]["available"] is False


def test_graph_runs_with_shared_plugin_and_restores_catalog_after_pause(tmp_path):
    library = library_at(tmp_path)
    workspace = tmp_path / "workspaces/research"
    raw = definition()
    raw["nodes"].append({"id": "b", "agent": "a", "plugins": ["method"]})
    raw["edges"] = [{"from": "a", "to": "b"}]
    write_json(workspace / "graph.json", raw)
    config = tmp_path / "runtime.json"
    write_json(config, {"models": []})
    attached = library.attach(("method",))
    model = runner.scripted_models({"a": ["true"]})["a"]
    node = runner._agent_for(graph_module.parse(raw), "a", tmp_path, {}, None, config,
                            scripted_model=model, plugins=attached)
    assert "Find the evidence" in node._instructions
    assert "ONLY_READ_WHEN_NEEDED" not in node._instructions
    commands = [
        "cat /plugins/method/instructions.md > evidence.txt; "
        "if echo corrupt >> /plugins/method/instructions.md; then exit 1; fi; "
        "test ! -d /workspace/plugins && test ! -d /plugins/unmounted",
        "anchor-done --summary 'used shared Plugin'",
    ]
    state = runner.run(workspace, config_path=config, run_id="proof", model_script={"a": commands, "b": commands},
                       stop_request=lambda: "paused" if (workspace / "runs/proof/a/evidence.txt").exists() else None)
    assert state.status == "paused"
    state = runner.run(workspace, config_path=config, resume=workspace / "runs/proof", model_script={"b": commands})
    assert state.status == "finished", state.error
    for node_id in ("a", "b"):
        assert (workspace / f"runs/proof/{node_id}/evidence.txt").read_text() == "ONLY_READ_WHEN_NEEDED\n"
        assert not (workspace / f"runs/proof/{node_id}/plugins").exists()
    scheduler = Scheduler(tmp_path, config)
    detail = scheduler.run("research", "proof")
    assert detail["plugins"]["a"] == detail["plugins"]["b"]
    assert (library.root / "plugins/method/instructions.md").read_text() == "ONLY_READ_WHEN_NEEDED\n"
    before = (workspace / "runs/proof/graph.json").read_bytes()
    raw["nodes"][1]["with"] = "different task"
    write_json(workspace / "graph.json", raw)
    with pytest.raises(ValueError, match="Graph definition changed"):
        runner.run(workspace, config_path=config, resume=workspace / "runs/proof", model_script={"b": commands})
    assert (workspace / "runs/proof/graph.json").read_bytes() == before


def test_tools_use_separate_reused_python_environments(tmp_path):
    library = library_at(tmp_path)
    tools = []
    for name in ("one", "two"):
        env = library.root / "environments" / name
        venv.EnvBuilder(with_pip=False, symlinks=True).create(env)
        site = env / f"lib/python{sys.version_info.major}.{sys.version_info.minor}/site-packages"
        (site / "conflicting_dependency.py").write_text(f"value = {name!r}\n")
        command = env / "bin/check"
        command.write_text(f"#!{env}/bin/python\nimport conflicting_dependency as d\nprint(d.value)\n")
        command.chmod(0o755)
        write_json(library.root / f"tools/{name}/tool.json", {"entrypoint": str(command), "environment": str(env)})
        tools.append(name)
    write_json(library.root / "plugins/method/plugin.json",
               {"name": "Method", "description": "Two conflicting dependencies", "tools": tools})
    attached = library.attach(("method",))
    for node_id in ("a", "b"):
        workspace = tmp_path / node_id
        workspace.mkdir()
        sandbox = NodeSandbox(workspace, node_id, False, 30, inputs=attached.binds)
        output = sandbox.run("/tools/one/run && /tools/two/run")
        assert output.returncode == 0, output.output
        assert output.output == "one\ntwo\n"
        assert not list(workspace.iterdir())
    assert len(list((library.root / "environments").iterdir())) == 2
    run = tmp_path / "recorded"
    run.mkdir()
    record_bindings(run, {"a": attached}, resume=False)
    dependency = library.root / f"environments/one/lib/python{sys.version_info.major}.{sys.version_info.minor}/site-packages/conflicting_dependency.py"
    dependency.write_text("value = 'changed dependency'\n")
    with pytest.raises(ValueError, match="changed"):
        record_bindings(run, {"a": library.attach(("method",))}, resume=True)


def test_api_rejects_unavailable_plugin_without_starting_run(tmp_path):
    library_at(tmp_path)
    write_json(tmp_path / "workspaces/research/graph.json", definition())
    scheduler = Scheduler(tmp_path, tmp_path / "config.json")
    assert scheduler.save("research", definition())[1] == 200
    assert scheduler.save("research", definition(["unknown"]))[1] == 400
    write_json(tmp_path / "workspaces/research/graph.json", definition(["unknown"]))
    assert scheduler.trigger("research", None)[1] == 400
    assert not scheduler.running
    assert not (tmp_path / "workspaces/research/runs").exists()


def test_tool_mounts_cannot_hide_reserved_paths_with_parent_segments(tmp_path):
    from pathlib import Path
    with pytest.raises(ValueError, match="reserved"):
        _mount(Path("/usr/../proc"))
    alias = tmp_path / "processes"
    alias.symlink_to("/proc")
    with pytest.raises(ValueError, match="reserved"):
        _mount(alias)
