from __future__ import annotations

import json
from datetime import datetime
from pathlib import Path

from anchor.simple.graph import load


ROOT = Path(__file__).resolve().parents[1]


def test_rsi_graph_has_evidence_feedback_and_publish_gate():
    graph = load(ROOT / "examples/graphs/rsi.json")
    assert graph.entry() == "collect"
    assert graph.routes("gate") == ("analyze", "audit-context", "publish")
    assert graph.routes("publish") == ()
    assert "evidence/runs.json" in graph.reads("run-audit")
    assert "research/ecosystem.json" in graph.reads("dependency-audit")
    assert graph.parallel_regions()['audit-fanout'].branches == (
        ('run-audit',), ('code-audit',), ('graph-audit',), ('plugin-audit',), ('research', 'dependency-audit'))
    assert graph.parallel_regions()['review-fanout'].branches == (('fact-review',), ('proposal-review',))
    assert not graph.max_rounds
    assert all(agent.max_steps is None for agent in graph.agents.values())
    assert 'github_repos' not in graph.input
    assert graph.ops["research"].network is True
    assert graph.ops["collect"].network is False


def test_rsi_collector_projects_runs_and_source_without_copying_history(tmp_path):
    from scripts.rsi.collect import collect

    anchor = tmp_path / "anchor"
    source = tmp_path / "source"
    run = anchor / "workspaces" / "demo" / "runs" / "r1"
    run.mkdir(parents=True)
    source.mkdir()
    (anchor / "workspaces/demo/graph.json").write_text(json.dumps({
        "objective": "demo", "nodes": [{"id": "x", "agent": "a"}],
        "agents": {"a": {"model": "m"}},
    }), encoding="utf-8")
    (run / "run.json").write_text(json.dumps({
        "status": "finished", "started": "2026-09-28T00:00:00Z",
        "updated": "2026-09-28T01:00:00Z", "nodes": {
            "x": {"submission": "ok", "files": ["answer.md"]}
        },
    }), encoding="utf-8")
    (source / "pyproject.toml").write_text("[project]\nname='demo'\n", encoding="utf-8")

    result = collect(anchor, source, tmp_path / "out",
                     end=datetime.fromisoformat("2026-10-01T09:00:00+08:00"), lookback_days=7)
    assert result["runs"] == 1
    evidence = json.loads((tmp_path / "out/runs.json").read_text())
    assert evidence[0]["status"] == "finished"
    assert (tmp_path / "out/source_snapshot/pyproject.toml").is_file()
    assert not (tmp_path / "out/runs.jsonl").exists()


def test_rsi_public_research_keeps_partial_failures_and_dependency_versions(tmp_path, monkeypatch):
    from scripts.rsi import research

    source = tmp_path / "source"
    source.mkdir()
    (source / "pyproject.toml").write_text(
        "[project]\ndependencies=['pydantic-ai-slim==2.46.0']\n", encoding="utf-8")
    (source / "apps/web").mkdir(parents=True)
    (source / "apps/web/package.json").write_text(
        json.dumps({"dependencies": {"react": "^19.0.0"}}), encoding="utf-8")

    def fake_get(url):
        if "pypi.org" in url:
            return ({"info": {"name": "pydantic-ai-slim", "version": "2.47.0",
                               "summary": "test", "project_urls": {}}, "releases": {"2.47.0": []}}, {})
        if "registry.npmjs.org" in url:
            return ({"name": "react", "dist-tags": {"latest": "19.1.0"},
                     "time": {"modified": "2026-10-01T00:00:00Z", "19.1.0": "2026-10-01T00:00:00Z"}}, {})
        return ({"error": "rate limited"}, {})

    monkeypatch.setattr(research, "_get", fake_get)
    result = research.research(source, tmp_path / "out", {})
    assert result["pypi"] == 1 and result["npm"] == 1
    data = json.loads((tmp_path / "out/ecosystem.json").read_text())
    assert data["pypi"][0]["data"]["info"]["version"] == "2.47.0"
    assert data.get('limitations')


def test_rsi_setup_is_idempotent_and_writes_read_only_grants(tmp_path):
    from scripts.setup_rsi import install

    first = install(tmp_path / "anchor", ROOT, weekday=3, time="09:00")
    second = install(tmp_path / "anchor", ROOT, weekday=3, time="09:00")
    assert first["schedule_created"] is True
    assert second["schedule_created"] is False
    assert len(json.loads((tmp_path / "anchor/state/schedules.json").read_text())) == 1
    grants = json.loads((tmp_path / "anchor/workspaces/rsi/local-inputs.json").read_text())
    assert set(grants) == {"collect", "research", "review", "gate"}
    assert grants["collect"]["anchor"] == str((tmp_path / "anchor").resolve())
