from __future__ import annotations

import json
import threading
import os
from datetime import datetime, timedelta
from http.client import HTTPConnection
from http.server import ThreadingHTTPServer

import pytest

from anchor.serve import Handler, Scheduler
from anchor.runtime.secrets import load_dotenv
from anchor.simple.run import _config, _secret


def test_dotenv_loads_without_overriding_environment(tmp_path, monkeypatch):
    path = tmp_path / ".env"
    path.write_text("DOCMOST_API_KEY='from-file'\nNEW_KEY=value\n", encoding="utf-8")
    monkeypatch.setenv("DOCMOST_API_KEY", "explicit")
    monkeypatch.delenv("NEW_KEY", raising=False)
    load_dotenv(path)
    assert os.environ["DOCMOST_API_KEY"] == "explicit"
    assert os.environ["NEW_KEY"] == "value"


def test_env_model_is_single_source_over_runtime_models(tmp_path, monkeypatch):
    monkeypatch.setenv("ANCHOR_MODEL_URL", "https://llm.example/v1")
    monkeypatch.setenv("ANCHOR_MODEL_API_KEY", "one-secret")
    monkeypatch.setenv("ANCHOR_MODEL_NAME", "unified-model")
    config = tmp_path / "runtime.json"
    config.write_text('{"models": [{"ref": "models.old", "model": "old"}]}', encoding="utf-8")
    models, secret_file = _config(config)
    profile = models["models.default"]
    assert secret_file is None
    assert profile["base_url"] == "https://llm.example/v1"
    assert profile["model"] == "unified-model"
    assert _secret(None, profile) == "one-secret"


def test_model_aliases_select_names_without_changing_default_or_credentials(tmp_path, monkeypatch):
    monkeypatch.setenv("ANCHOR_MODEL_URL", "https://llm.example/v1")
    monkeypatch.setenv("ANCHOR_MODEL_API_KEY", "one-secret")
    monkeypatch.setenv("ANCHOR_MODEL_NAME", "fast")
    monkeypatch.setenv("ANCHOR_MODEL_ALIASES", '{"models.review":"strong"}')
    config = tmp_path / "runtime.json"
    config.write_text("{}")
    models, secret_file = _config(config)
    assert models["models.default"]["model"] == "fast"
    assert models["models.review"]["model"] == "strong"
    assert models["models.review"]["base_url"] == models["models.default"]["base_url"]
    assert _secret(secret_file, models["models.review"]) == "one-secret"
    assert "one-secret" not in json.dumps(models)
    # Legacy graphs such as models.academic still use the environment default.
    assert models["models.default"]["fallback_for_unknown_refs"] is True
    assert not models["models.review"].get("fallback_for_unknown_refs")
    from anchor.simple import run as runner
    from anchor.simple.graph import load
    graph_path = tmp_path / "graph.json"
    graph_path.write_text(json.dumps({
        "agents": {"legacy": {"model": "models.academic"}, "review": {"model": "models.review"}},
        "nodes": [{"id": "legacy", "agent": "legacy"}, {"id": "review", "agent": "review"}],
        "edges": [{"from": "legacy", "to": "review"}],
    }))
    graph = load(graph_path)
    selected = []
    monkeypatch.setattr(runner, "model_for", lambda profile, **kwargs: selected.append(profile["model"]))
    monkeypatch.setattr(runner, "Node", lambda **kwargs: kwargs)
    for name in ("legacy", "review"):
        runner._agent_for(graph, name, tmp_path / name, models, secret_file, config)
    assert selected == ["fast", "strong"]


@pytest.mark.parametrize("aliases", ['[]', '{"models.default":"other"}',
                                    '{"wrong":"strong"}', '{"models.review":""}'])
def test_invalid_model_aliases_fail_without_replacing_default(tmp_path, monkeypatch, aliases):
    monkeypatch.setenv("ANCHOR_MODEL_URL", "https://llm.example/v1")
    monkeypatch.setenv("ANCHOR_MODEL_API_KEY", "one-secret")
    monkeypatch.setenv("ANCHOR_MODEL_ALIASES", aliases)
    config = tmp_path / "runtime.json"
    config.write_text("{}")
    with pytest.raises(ValueError):
        _config(config)


def test_webhook_requires_a_key_and_rejects_busy_graph_without_a_run(tmp_path, monkeypatch):
    key = "k" * 40
    monkeypatch.setenv("ANCHOR_API_KEYS", json.dumps([key]))
    workspace = tmp_path / "workspaces" / "demo"
    workspace.mkdir(parents=True)
    (workspace / "graph.json").write_text(json.dumps({
        "entry": "only", "objective": "test", "agents": {"a": {"model": "m"}},
        "nodes": [{"id": "only", "agent": "a"}], "edges": [],
    }))
    scheduler = Scheduler(tmp_path, tmp_path / "runtime.json")
    starts = []
    scheduler._run = lambda *args, **kwargs: starts.append((args, kwargs))
    handler = type("TestHandler", (Handler,), {"scheduler": scheduler})
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    client = HTTPConnection(*server.server_address, timeout=5)

    def call(body, auth=None):
        headers = {"Content-Type": "application/json"}
        if auth is not None:
            headers["Authorization"] = auth
        client.request("POST", "/v1/webhooks/graphs/demo", json.dumps(body), headers)
        response = client.getresponse()
        return response.status, json.loads(response.read())

    try:
        assert call({"input": {} })[0] == 401
        assert call({"input": {}}, "Bearer wrong")[0] == 401
        assert call({"input": []}, f"Bearer {key}")[0] == 400
        scheduler.running["demo"] = "already-running"
        status, busy = call({"input": {"x": 1}}, f"Bearer {key}")
        assert status == 409 and busy["running"] == "already-running"
        assert not starts
        assert not (workspace / "runs").exists()
        scheduler.running.clear()
        status, accepted = call({"input": {"x": 1}}, f"Bearer {key}")
        assert status == 202 and accepted["graph"] == "demo"
        assert len(starts) == 1 and starts[0][1]["run_input"] == {"x": 1}
    finally:
        client.close()
        server.shutdown()
        server.server_close()
        thread.join()


def test_responses_session_is_bound_to_its_bearer_key(tmp_path, monkeypatch):
    first, second = "a" * 40, "b" * 40
    monkeypatch.setenv("ANCHOR_API_KEYS", json.dumps([first, second]))
    workspace = tmp_path / "workspaces" / "demo"
    workspace.mkdir(parents=True)
    (workspace / "graph.json").write_text(json.dumps({
        "entry": "only", "objective": "test", "agents": {"a": {"model": "m"}},
        "nodes": [{"id": "only", "agent": "a"}], "edges": [],
    }))
    scheduler = Scheduler(tmp_path, tmp_path / "runtime.json")

    def fake_message(session, prompt, *, turn_id=None):
        scheduler.turns.append(turn_id, {"type": "text-delta", "delta": "hello"})
        return json.dumps({"message": "hello"}), 200

    scheduler.pilot_message = fake_message
    handler = type("TestHandler", (Handler,), {"scheduler": scheduler})
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    client = HTTPConnection(*server.server_address, timeout=5)

    def call(body, key):
        client.request("POST", "/v1/responses", json.dumps(body), {
            "Content-Type": "application/json", "Authorization": f"Bearer {key}"})
        response = client.getresponse()
        return response.status, json.loads(response.read())

    try:
        status, response = call({"model": "anchor-copilot", "input": "hello"}, first)
        assert status == 200 and response["output_text"] == "hello"
        status, denied = call({"input": "continue", "previous_response_id": response["id"]}, second)
        assert status == 404 and denied["error"] == "no such response"
        status, continued = call({"input": [{"role": "user", "content": "again"}],
                                  "previous_response_id": response["id"]}, first)
        assert status == 200 and continued["output_text"] == "hello"
        assert continued["id"] != response["id"]
        assert call({"input": [{"role": "assistant", "content": "not supported"}]}, first)[0] == 400
    finally:
        client.close()
        server.shutdown()
        server.server_close()
        thread.join()


def test_scheduled_trigger_skips_busy_without_creating_a_run(tmp_path, monkeypatch):
    monkeypatch.delenv("ANCHOR_API_KEYS", raising=False)
    workspace = tmp_path / "workspaces" / "demo"
    workspace.mkdir(parents=True)
    (workspace / "graph.json").write_text(json.dumps({
        "entry": "only", "objective": "test", "agents": {"a": {"model": "m"}},
        "nodes": [{"id": "only", "agent": "a"}], "edges": [],
    }))
    scheduler = Scheduler(tmp_path, tmp_path / "runtime.json")
    starts = []
    scheduler.trigger = lambda *args, **kwargs: starts.append((args, kwargs)) or ("{}", 202)
    now = datetime.now().replace(microsecond=0)
    body, status = scheduler.create_schedule("demo", {"type": "once", "at":
                                                    (now + timedelta(seconds=5)).isoformat()})
    assert status == 201
    schedule = json.loads(body)["schedule"]
    scheduler.running["demo"] = "busy"
    scheduler.tick_schedules(datetime.fromisoformat(schedule["next_at"]))
    assert not starts
    assert not (workspace / "runs").exists()


def test_timeline_marks_busy_occurrence_even_if_the_run_has_since_finished(tmp_path, monkeypatch):
    monkeypatch.delenv("ANCHOR_API_KEYS", raising=False)
    workspace = tmp_path / "workspaces" / "demo"
    runs = workspace / "runs" / "run-1"
    runs.mkdir(parents=True)
    now = datetime.now().replace(microsecond=0)
    due = now - timedelta(minutes=2)
    (workspace / "graph.json").write_text("{}")
    (runs / "run.json").write_text(json.dumps({
        "status": "finished", "started": (due - timedelta(minutes=5)).isoformat(),
        "updated": (due + timedelta(minutes=1)).isoformat(),
    }))
    scheduler = Scheduler(tmp_path, tmp_path / "runtime.json")
    scheduler.schedules = [{
        "id": "schedule-1", "graph": "demo", "rule": {"type": "once", "at": due.isoformat()},
        "created_at": (due - timedelta(days=1)).isoformat(), "next_at": due.isoformat(),
        "enabled": False, "input": {},
    }]

    occurrence = next(item for item in scheduler.timeline()["scheduled"]
                      if item["schedule"] == "schedule-1")
    assert occurrence["status"] == "missed_busy"
