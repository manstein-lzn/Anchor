"""Responses ownership applies to every HTTP view of its underlying Session."""

from concurrent.futures import ThreadPoolExecutor
from http.client import HTTPConnection
import json
import threading

import pytest

from anchor import pilot
from anchor.serve import Scheduler
from test_platform_rust_backend import platform  # noqa: F401

KEY_ONE = "owner-one-fixture-secret-000000000001"
KEY_TWO = "owner-two-fixture-secret-000000000002"


def http(server, method, path, body=None, key=None):
    client = HTTPConnection(*server.server_address, timeout=5)
    headers = {"Content-Type": "application/json"}
    if key is not None:
        headers["Authorization"] = f"Bearer {key}"
    try:
        client.request(method, path, json.dumps(body) if body is not None else None, headers)
        response = client.getresponse()
        payload = response.read()
        value = json.loads(payload) if response.getheader("Content-Type", "").startswith("application/json") else payload
        return response.status, value
    finally:
        client.close()


def install_dialogue(scheduler, monkeypatch, calls):
    def message(session, prompt, *, turn_id=None):
        calls.append((session, prompt))
        if turn_id is not None:
            scheduler.turns.append(turn_id, {"type": "text-delta", "delta": "Private response text"})
        return json.dumps({"message": "Private response text"}), 200

    monkeypatch.setattr(scheduler, "pilot_message", message)
    monkeypatch.setattr(pilot, "history", lambda _store, _session: [
        {"role": "user", "text": "Private request text"},
        {"role": "assistant", "text": "Private response text"},
    ])


@pytest.fixture
def access(platform, monkeypatch):  # noqa: F811
    scheduler, _runtime, frontend, _forbidden = platform
    monkeypatch.setenv("ANCHOR_API_KEYS", json.dumps([KEY_ONE, KEY_TWO]))
    scheduler.api_keys = (KEY_ONE, KEY_TWO)
    calls = []
    install_dialogue(scheduler, monkeypatch, calls)
    yield scheduler, frontend, calls


def create_response(scheduler, frontend, key):
    status, response = http(frontend, "POST", "/v1/responses", {"input": "Private request text"}, key)
    assert status == 200, response
    assert response["output_text"] == "Private response text"
    ref = scheduler.response_refs[response["id"]]
    return response, ref["session"], ref["turn"]


def test_other_key_cannot_enumerate_read_or_mutate_a_responses_session(access):
    scheduler, frontend, calls = access
    response, session, turn = create_response(scheduler, frontend, KEY_ONE)
    scheduler.sessions.create("ordinary")
    scheduler.sessions.set_pending(session, [{"tool_call_id": "private-approval", "key": "private-approval",
                                             "action": "graph_delete", "target": "demo",
                                             "proposal": {"graph": "demo"},
                                             "precondition": {"graph": "demo", "expected_sha256": "frozen"}}])
    owner_listing = http(frontend, "GET", "/sessions", key=KEY_ONE)[1]["sessions"]
    other_listing = http(frontend, "GET", "/sessions", key=KEY_TWO)[1]["sessions"]
    assert session in {item["id"] for item in owner_listing}
    assert session not in {item["id"] for item in other_listing}
    assert "ordinary" in {item["id"] for item in other_listing}
    assert http(frontend, "GET", "/sessions/ordinary", key=KEY_TWO)[0] == 200

    for suffix in ["", "/messages", "/events", "/turns", f"/turns/{turn}/events"]:
        status, _value = http(frontend, "GET", f"/sessions/{session}{suffix}", key=KEY_ONE)
        assert status == 200
        assert http(frontend, "GET", f"/sessions/{session}{suffix}", key=KEY_TWO)[0] == 404
    encoded = "%72" + session[1:]
    assert http(frontend, "GET", f"/sessions/{encoded}/messages", key=KEY_TWO)[0] == 404
    before = scheduler.sessions.get(session).model_dump(mode="json")
    before_calls = list(calls)

    mutations = [
        ("POST", "/turns", {"request_id": "intruder", "message": "Different request"}),
        ("POST", "/messages", {"message": "Different request"}), ("POST", "/resume", {}),
        ("POST", "/stop", {}), ("POST", "/status", {"status": "archived"}),
        ("POST", "/confirm", {"action": "graph_delete", "approval_key": "private-approval"}),
        ("POST", "/reject", {"action": "graph_delete", "approval_key": "private-approval"}),
        ("POST", "/runs", {"run": "missing"}), ("PUT", "", {"title": "Changed by another owner"}),
        ("DELETE", "", None),
    ]
    for method, suffix, body in mutations:
        assert http(frontend, method, f"/sessions/{session}{suffix}", body, KEY_TWO)[0] == 404
    assert scheduler.sessions.get(session).model_dump(mode="json") == before
    assert calls == before_calls
    assert scheduler.turns.find_request(session, "intruder") is None
    assert http(frontend, "POST", "/v1/responses", {"input": "Continue",
                                                   "previous_response_id": response["id"]}, KEY_TWO)[0] == 404


def test_owner_can_continue_after_restart_and_revoked_key_does_not_expose_session(access, monkeypatch):
    scheduler, frontend, calls = access
    response, session, turn = create_response(scheduler, frontend, KEY_ONE)
    reopened = Scheduler(scheduler.root, scheduler.config)
    install_dialogue(reopened, monkeypatch, calls)
    frontend.RequestHandlerClass.scheduler = reopened
    assert http(frontend, "GET", f"/sessions/{session}", key=KEY_ONE)[0] == 200
    assert http(frontend, "GET", f"/sessions/{session}/turns/{turn}/events", key=KEY_ONE)[0] == 200
    assert http(frontend, "GET", f"/sessions/{session}", key=KEY_TWO)[0] == 404
    status, next_response = http(frontend, "POST", "/v1/responses", {
        "input": "Continue", "previous_response_id": response["id"],
    }, KEY_ONE)
    assert status == 200
    assert reopened.response_refs[next_response["id"]]["session"] == session
    reopened.api_keys = (KEY_TWO,)
    assert http(frontend, "GET", f"/sessions/{session}", key=KEY_ONE)[0] == 401
    assert http(frontend, "GET", f"/sessions/{session}/messages", key=KEY_TWO)[0] == 404
    assert session not in {item["id"] for item in http(frontend, "GET", "/sessions", key=KEY_TWO)[1]["sessions"]}
    reopened.api_keys = ()
    assert http(frontend, "GET", f"/sessions/{session}/messages", key=KEY_ONE)[0] == 404
    assert http(frontend, "GET", f"/sessions/{session}/messages")[0] == 404
    assert session not in {item["id"] for item in http(frontend, "GET", "/sessions")[1]["sessions"]}


def test_loopback_without_keys_retains_one_consistent_responses_owner(access):
    scheduler, frontend, _calls = access
    scheduler.api_keys = ()
    response, session, _turn = create_response(scheduler, frontend, None)
    assert http(frontend, "GET", f"/sessions/{session}")[0] == 200
    assert http(frontend, "GET", f"/sessions/{session}", key=KEY_ONE)[0] == 200
    assert session in {item["id"] for item in http(frontend, "GET", "/sessions")[1]["sessions"]}
    assert http(frontend, "POST", "/v1/responses", {"input": "Continue",
                                                  "previous_response_id": response["id"]})[0] == 200
    assert http(frontend, "POST", "/v1/responses", {"input": "Continue",
                                                  "previous_response_id": response["id"]}, KEY_TWO)[0] == 200


@pytest.mark.parametrize("key", [KEY_ONE, KEY_TWO])
def test_unknown_and_unregistered_responses_sessions_are_not_visible(access, key):
    scheduler, frontend, _calls = access
    orphan = "responses-orphan-fixture"
    scheduler.sessions.create(orphan)
    for session in ["unknown-session", "responses-unknown-fixture", orphan]:
        assert http(frontend, "GET", f"/sessions/{session}", key=key)[0] == 404
        assert http(frontend, "POST", f"/sessions/{session}/turns", {
            "request_id": "unregistered", "message": "Do not run",
        }, key)[0] == 404
    assert orphan not in {item["id"] for item in http(frontend, "GET", "/sessions", key=key)[1]["sessions"]}
    assert scheduler.turns.find_request(orphan, "unregistered") is None


def test_response_creation_window_is_hidden_until_owner_reference_is_registered(access, monkeypatch):
    scheduler, frontend, _calls = access
    entered = threading.Event()
    released = threading.Event()
    identifiers = []
    create_turn = scheduler.create_turn

    def gated_turn(session, request_id, prompt, channel_input=None):
        result = create_turn(session, request_id, prompt, channel_input)
        identifiers.append(session)
        entered.set()
        assert released.wait(5)
        return result

    monkeypatch.setattr(scheduler, "create_turn", gated_turn)
    with ThreadPoolExecutor(max_workers=1) as worker:
        pending = worker.submit(http, frontend, "POST", "/v1/responses", {"input": "Private request text"}, KEY_ONE)
        try:
            assert entered.wait(3)
            session, = identifiers
            assert scheduler.sessions.get(session).id == session
            assert not scheduler.response_refs
            for key in [KEY_ONE, KEY_TWO]:
                assert http(frontend, "GET", f"/sessions/{session}", key=key)[0] == 404
                assert session not in {item["id"] for item in http(frontend, "GET", "/sessions", key=key)[1]["sessions"]}
        finally:
            released.set()
        status, response = pending.result(timeout=5)
        assert status == 200, response
        assert scheduler.response_refs[response["id"]]["session"] == session
        assert http(frontend, "GET", f"/sessions/{session}", key=KEY_ONE)[0] == 200
        assert http(frontend, "GET", f"/sessions/{session}", key=KEY_TWO)[0] == 404


@pytest.mark.parametrize("graph", ["", "demo"])
@pytest.mark.parametrize("action", ["confirm", "reject"])
def test_channel_approval_is_rejected_without_rewriting_historical_session(access, graph, action):
    scheduler, frontend, calls = access
    session = "channel-approval-fixture"
    scheduler.sessions.create(session, graph=graph, channel={"platform": "wecom"})
    scheduler.sessions.set_pending(session, [{"tool_call_id": "old-approval", "key": "old-approval",
                                             "action": "graph_delete", "target": "demo",
                                             "proposal": {"graph": "demo"}}])
    directory = scheduler.sessions.sessions_dir / session
    before = {name: (directory / name).read_bytes() for name in ["session.json", "events.jsonl"]}
    assert http(frontend, "POST", f"/sessions/{session}/{action}", {
        "action": "graph_delete", "approval_key": "old-approval",
    }, KEY_ONE)[0] == 501
    assert http(frontend, "POST", f"/sessions/{session}/{action}", {}, KEY_ONE)[0] == 400
    assert {name: (directory / name).read_bytes() for name in before} == before
    assert not calls
