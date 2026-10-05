"""Channel coordination uses public Rust facts, never Python Graph or Run files."""

from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import base64
import copy
import hashlib
from http.client import IncompleteRead
import io
import json
from pathlib import Path
import threading
import time

import pytest

from anchor.channel import assistant
from anchor.channel.runtime import graph_plugins
from anchor.channel.supervisor import ChannelSupervisor
from anchor.pilot_turns import TurnStore
from anchor.runtime_http import RuntimeHTTPError
from anchor.session import SessionStore


class Library:
    def __init__(self, root):
        self.root = root
        self.attached = []

    def attach(self, identifiers):
        self.attached.append(tuple(identifiers))
        if any(identifier != "wecom" for identifier in identifiers):
            raise ValueError("missing Plugin")

    def channels(self, identifier):
        if identifier != "wecom":
            return ()
        return ({"platform": "wecom", "plugin": "wecom", "entrypoint": "ws_gateway.py",
                 "required_environment": []},)


class RuntimeFacts:
    def __init__(self):
        self.lock = threading.RLock()
        self.records = {}
        self.admissions = []
        self.controls = []
        self.auto_complete = True
        self.unavailable = False
        self.admission_error = None
        self.lost_response = False
        self.stop_released = threading.Event()
        self.stop_released.set()
        self.admitted = threading.Event()
        self.admission_released = threading.Event()
        self.admission_released.set()

    def check(self):
        if self.unavailable:
            raise RuntimeHTTPError("fixture Runtime unavailable", 503)

    def snapshot(self, identifier):
        self.check()
        with self.lock:
            value = self.records.get(identifier)
            if value and value["control_requested"] == "stop" and self.stop_released.is_set():
                value["state"]["status"] = "stopped"
                value["active"] = False
            return copy.deepcopy(value)

    def admit(self, graph, identifier, session, reply_node, run_input, previous, attachments=None):
        self.check()
        if self.admission_error:
            return json.dumps({"error": "fixture admission refused"}), self.admission_error
        with self.lock:
            if identifier in self.records:
                return json.dumps({"run": identifier, "graph": graph}), 202
            if previous is not None:
                record = self.records[previous]
                assert record["active"] is False and record["state"]["status"] != "running"
            self.admissions.append({"graph": graph, "run": identifier, "session": session,
                                    "reply_node": reply_node, "input": copy.deepcopy(run_input),
                                    "previous_run": previous,
                                    **({"attachments": copy.deepcopy(attachments)} if attachments else {})})
            self.records[identifier] = {
                "graph": graph, "run": identifier, "backend": "rust", "active": not self.auto_complete,
                "control_requested": None, "channel_reply": False,
                "state": {"status": "completed" if self.auto_complete else "running", "input": copy.deepcopy(run_input),
                          "trigger": {"source": "channel", "session": session, "reply_node": reply_node,
                                      "previous_run": previous},
                          "nodes": {reply_node: {"submitted": self.auto_complete,
                                                 "submission": "Reply: " + run_input["message"]}}},
            }
            if attachments:
                self.records[identifier]["attachments"] = [
                    {"name": item["name"], "sha256": hashlib.sha256(base64.b64decode(item["data_base64"])).hexdigest(),
                     "size": len(base64.b64decode(item["data_base64"])), "media_type": item["media_type"]}
                    for item in attachments]
        self.admitted.set()
        assert self.admission_released.wait(3)
        if self.lost_response:
            raise RuntimeHTTPError("fixture admission response was lost", 503)
        return json.dumps({"run": identifier, "graph": graph}), 202

    def control(self, identifier, what):
        self.check()
        with self.lock:
            self.controls.append((identifier, what))
            self.records[identifier]["control_requested"] = what
        return json.dumps({"run": identifier, "asked": what}), 202

    def complete(self, identifier, text):
        with self.lock:
            record = self.records[identifier]
            record["active"] = False
            record["state"]["status"] = "completed"
            record["state"]["nodes"][record["state"]["trigger"]["reply_node"]] = {
                "submitted": True, "submission": text,
            }


class RuntimeDownloads:
    def __init__(self):
        self.requests = []
        self.responses = {}

    @contextmanager
    def download(self, path):
        self.requests.append(path)
        response = self.responses[path]
        if isinstance(response, Exception):
            raise response
        with io.BytesIO(response) if isinstance(response, bytes) else response as stream:
            yield stream


class PublicPorts:
    def __init__(self, root, facts=None):
        self.runtime = RuntimeDownloads()
        self.root = root
        self.sessions = SessionStore(root)
        self.turns = TurnStore(root)
        self.turns.interrupt_running()
        self.library = Library(root / "library")
        self.wecom_graph = "assistant-graph"
        self.wecom_reply_node = "module/assistant"
        self.wecom_users = {"alice", "bob"}
        self.lock = threading.RLock()
        self.facts = facts or RuntimeFacts()
        self.graph_response = None
        self.tails = {}
        self.workers = []
        self.released = {}
        self.private_reads = []

    def __getattr__(self, name):
        if name in {"workspace", "workspaces", "run_dir", "running", "control", "config", "graph_calls"}:
            self.private_reads.append(name)
            raise AssertionError(f"private Runtime attribute read: {name}")
        raise AttributeError(name)

    def graph(self, name):
        self.facts.check()
        if self.graph_response is not None:
            value, status = self.graph_response
            return json.dumps(value), status
        return json.dumps({"graph": name, "definition": {"nodes": []},
                           "node_plugins": {self.wecom_reply_node: ["wecom"]}}), 200

    def run(self, graph, identifier):
        assert graph == ""
        return self.facts.snapshot(identifier)

    def runs(self):
        self.facts.check()
        with self.facts.lock:
            return [{"run": identifier, "backend": value["backend"],
                     "trigger": copy.deepcopy(value["state"]["trigger"])}
                    for identifier, value in self.facts.records.items()]

    def control_run(self, identifier, action):
        return self.facts.control(identifier, action)

    def conversation_run(self, graph, run, session, reply_node, run_input, previous_run, attachments=None):
        return self.facts.admit(graph, run, session, reply_node, run_input, previous_run, attachments)

    def create_turn(self, session, request_id, prompt, channel_input=None):
        with self.lock:
            existing = self.turns.find_request(session, request_id)
            if existing:
                if existing["prompt"] != prompt:
                    return json.dumps({"error": "different input"}), 409
                return json.dumps({"turn": existing}), 202
            predecessor = None
            if session in self.tails:
                prior, predecessor = self.tails[session]
                self.turns.finish(prior, "stopped", "superseded by a newer message")
            turn, _created = self.turns.create(session, request_id, prompt, channel_input)
            released = threading.Event()
            self.released[turn["id"]] = released
            self.tails[session] = (turn["id"], released)
            worker = threading.Thread(target=self.execute_turn, args=(turn, predecessor, released), daemon=True)
            self.workers.append(worker)
            worker.start()
        return json.dumps({"turn": turn}), 202

    def execute_turn(self, turn, predecessor, released):
        try:
            if predecessor:
                assert predecessor.wait(4)
            body, status = assistant.execute(self, turn)
            value = json.loads(body)
            self.turns.finish(turn["id"], "completed" if status == 200 else
                              "stopped" if value.get("stopped") else "failed", value.get("error", ""))
        finally:
            released.set()
            with self.lock:
                if self.tails.get(turn["session"], (None,))[0] == turn["id"]:
                    self.tails.pop(turn["session"])

    def close(self):
        self.facts.stop_released.set()
        self.facts.admission_released.set()
        for turn, _released in list(self.tails.values()):
            self.turns.finish(turn, "stopped", "fixture cleanup")
        for worker in self.workers:
            worker.join(timeout=4)
            assert not worker.is_alive()
        assert not self.private_reads


@pytest.fixture
def ports(tmp_path, monkeypatch):
    def forbidden(*_args, **_kwargs):
        raise AssertionError("Rust channel cannot use the Python parser or Runner")

    monkeypatch.setattr("anchor.channel.assistant.graph_module.load", forbidden)
    monkeypatch.setattr("anchor.channel.assistant.runner.run", forbidden)
    monkeypatch.setattr("anchor.channel.assistant.runner.RunState.load", forbidden)
    port = PublicPorts(tmp_path)
    yield port
    port.close()
    assert not (tmp_path / "workspaces").exists()


def event(identifier="event-1", sender="alice", text="Hello", **values):
    return {"source": "wecom", "event_id": identifier, "sender_id": sender,
            "conversation_id": sender + "-private", "text": text, **values}


def receive(ports, value=None):
    body, status = assistant.receive(ports, value or event(), wait=False)
    assert status == 202, body
    return json.loads(body)["turn"]


def settled(ports, turn):
    assert ports.released[turn["id"]].wait(4)
    return ports.turns.get(turn["session"], turn["id"])


def wait_until(condition):
    deadline = time.monotonic() + 3
    while not condition() and time.monotonic() < deadline:
        time.sleep(0.01)
    assert condition()


def test_text_turn_uses_remote_metadata_and_repeated_event_returns_original_reply(ports):
    turn = receive(ports)
    assert settled(ports, turn)["status"] == "completed"
    body, status = assistant.result(ports, turn)
    value = json.loads(body)
    assert status == 200 and value["text"] == "Reply: Hello"
    identifier = assistant.run_id(turn)
    assert ports.sessions.get(turn["session"]).run_ids == [identifier]
    assert ports.facts.admissions == [{"graph": ports.wecom_graph, "run": identifier,
                                      "session": turn["session"], "reply_node": ports.wecom_reply_node,
                                      "input": {"message": "Hello", "channel": {"source": "wecom", "sender_id": "alice",
                                                                                 "conversation_id": "alice-private",
                                                                                 "attachments": []},
                                                "session": turn["session"], "interrupted_messages": []},
                                      "previous_run": None}]
    repeat = receive(ports)
    assert repeat["id"] == turn["id"] and len(ports.facts.admissions) == 1
    assert assistant.result(ports, repeat) == (body, status)
    assert assistant.history(ports, turn["session"]) == [
        {"role": "user", "text": "Hello"}, {"role": "assistant", "text": "Reply: Hello"},
    ]
    assert ports.library.attached and not ports.private_reads
    assert not ports.runtime.requests


def test_different_users_are_concurrent_and_do_not_share_predecessors(ports):
    ports.facts.auto_complete = False
    with ThreadPoolExecutor(max_workers=2) as workers:
        first = workers.submit(receive, ports, event(sender="alice")).result(timeout=3)
        second = workers.submit(receive, ports, event(identifier="event-2", sender="bob")).result(timeout=3)
    wait_until(lambda: len(ports.facts.admissions) == 2)
    assert first["session"] != second["session"]
    assert all(value["previous_run"] is None for value in ports.facts.admissions)
    assert all(value["active"] for value in ports.facts.records.values())
    for turn in [first, second]:
        ports.facts.complete(assistant.run_id(turn), "Independent reply")
        assert settled(ports, turn)["status"] == "completed"


def test_three_message_burst_skips_middle_turn_and_waits_for_rust_release_before_handoff(ports):
    ports.facts.auto_complete = False
    ports.facts.stop_released.clear()
    first = receive(ports, event(text="First instruction"))
    assert ports.facts.admitted.wait(2)
    middle = receive(ports, event(identifier="event-2", text="Second instruction"))
    newest = receive(ports, event(identifier="event-3", text="Third instruction"))
    wait_until(lambda: bool(ports.facts.controls))
    assert len(ports.facts.admissions) == 1
    assert not ports.released[first["id"]].is_set()
    assert not ports.released[middle["id"]].is_set()
    ports.facts.stop_released.set()
    wait_until(lambda: len(ports.facts.admissions) == 2)
    second_admission = ports.facts.admissions[1]
    assert second_admission["run"] == assistant.run_id(newest)
    assert second_admission["previous_run"] == assistant.run_id(first)
    assert second_admission["input"]["interrupted_messages"] == ["First instruction", "Second instruction"]
    assert assistant.run_id(middle) not in ports.facts.records
    assert ports.facts.records[assistant.run_id(first)]["active"] is False
    ports.facts.complete(assistant.run_id(newest), "Latest reply only")
    settled(ports, first)
    settled(ports, middle)
    settled(ports, newest)
    assert json.loads(assistant.result(ports, first)[0])["superseded"] is True
    assert json.loads(assistant.result(ports, middle)[0])["superseded"] is True
    history = assistant.history(ports, first["session"])
    assert [item["text"] for item in history if item["role"] == "assistant"] == ["Latest reply only"]


def test_stop_racing_with_admission_stops_actual_accepted_run_before_thread_release(ports):
    ports.facts.auto_complete = False
    ports.facts.admission_released.clear()
    ports.facts.stop_released.clear()
    old = receive(ports)
    assert ports.facts.admitted.wait(2)
    new = receive(ports, event(identifier="event-2", text="New input"))
    ports.facts.admission_released.set()
    wait_until(lambda: bool(ports.facts.controls))
    assert ports.facts.controls[0][0] == assistant.run_id(old)
    assert not ports.released[old["id"]].is_set() and len(ports.facts.admissions) == 1
    ports.facts.stop_released.set()
    wait_until(lambda: len(ports.facts.admissions) == 2)
    assert ports.facts.admissions[1]["previous_run"] == assistant.run_id(old)
    ports.facts.complete(assistant.run_id(new), "New reply")
    settled(ports, old)
    settled(ports, new)


def test_explicit_stop_waits_until_remote_run_is_inactive(ports, monkeypatch):
    ports.facts.auto_complete = False
    ports.facts.stop_released.clear()
    turn = receive(ports)
    assert ports.facts.admitted.wait(2)
    read_run = ports.run
    stopped_polls = []

    def count_stop_polls(graph, identifier):
        snapshot = read_run(graph, identifier)
        if snapshot and snapshot["control_requested"] == "stop":
            stopped_polls.append(identifier)
        return snapshot

    monkeypatch.setattr(ports, "run", count_stop_polls)
    ports.turns.finish(turn["id"], "stopped", "stopped by operator")
    wait_until(lambda: len(stopped_polls) >= 3)
    assert not ports.released[turn["id"]].is_set()
    ports.facts.stop_released.set()
    settled(ports, turn)
    body, status = assistant.result(ports, turn)
    assert status == 409 and json.loads(body)["stopped"] is True
    assert ports.facts.records[assistant.run_id(turn)]["active"] is False
    assert ports.facts.controls.count((assistant.run_id(turn), "stop")) == 1


def test_result_rechecks_turn_after_remote_projection_before_returning_reply(ports, monkeypatch):
    session = ports.sessions.create(graph=ports.wecom_graph, reply_node=ports.wecom_reply_node,
                                    channel={"source": "wecom", "sender_id": "alice",
                                             "conversation_id": "alice-private"})
    turn, _ = ports.turns.create(session.id, "result-race", "Hello")
    ports.facts.admit(session.graph, assistant.run_id(turn), session.id, session.reply_node,
                      {"message": "Hello"}, None)
    ports.facts.records[assistant.run_id(turn)]["channel_reply"] = True
    read_run = ports.run

    def supersede_after_projection(graph, identifier):
        snapshot = read_run(graph, identifier)
        with ports.lock:
            ports.turns.finish(turn["id"], "stopped", "superseded by a newer message")
        return snapshot

    monkeypatch.setattr(ports, "run", supersede_after_projection)
    body, status = assistant.result(ports, turn)
    assert status == 200 and json.loads(body)["superseded"] is True
    assert not ports.runtime.requests


def reply_turn(ports):
    session = ports.sessions.create(graph=ports.wecom_graph, reply_node=ports.wecom_reply_node,
                                    channel={"source": "wecom", "sender_id": "alice",
                                             "conversation_id": "alice-private"})
    turn, _ = ports.turns.create(session.id, "rich-reply", "Hello")
    identifier = assistant.run_id(turn)
    ports.facts.admit(session.graph, identifier, session.id, session.reply_node, {"message": "Hello"}, None)
    ports.facts.records[identifier]["channel_reply"] = True
    return turn, f"/runs/{identifier}/channel-reply"


def test_completed_rich_reply_downloads_public_payload_and_preserves_image_bytes(ports):
    from anchor.channel.media import make_image_item

    turn, path = reply_turn(ports)
    image = png_bytes()
    items = [make_image_item(image)]
    ports.runtime.responses[path] = json.dumps(items).encode()
    first = assistant.result(ports, turn)
    assert first[1] == 200
    value = json.loads(first[0])
    assert value["text"] == "Reply: Hello" and value["msg_item"] == items
    assert base64.b64decode(value["msg_item"][0]["image"]["base64"]) == image
    assert assistant.result(ports, turn) == first
    assert ports.runtime.requests == [path, path]
    assert len(ports.facts.admissions) == 1


@pytest.mark.parametrize("projection", [{}, {"channel_reply": False}], ids=["missing", "false"])
def test_text_reply_does_not_download_without_public_rich_reply_fact(ports, projection):
    turn, _path = reply_turn(ports)
    record = ports.facts.records[assistant.run_id(turn)]
    record.pop("channel_reply")
    record.update(projection)
    body, status = assistant.result(ports, turn)
    assert status == 200 and json.loads(body)["text"] == "Reply: Hello"
    assert "msg_item" not in json.loads(body)
    assert not ports.runtime.requests


def test_legacy_reply_does_not_download_from_rust_even_with_rich_reply_fact(ports):
    turn, _path = reply_turn(ports)
    record = ports.facts.records[assistant.run_id(turn)]
    record["backend"] = "legacy"
    record["state"]["status"] = "finished"
    body, status = assistant.result(ports, turn)
    assert status == 200 and json.loads(body)["text"] == "Reply: Hello"
    assert not ports.runtime.requests


@pytest.mark.parametrize("run_status,active,submitted", [
    ("running", True, True), ("completed", True, True),
    ("failed", False, True), ("completed", False, False),
])
def test_rich_reply_is_not_downloaded_before_run_and_reply_are_complete(ports, run_status, active, submitted):
    turn, _path = reply_turn(ports)
    record = ports.facts.records[assistant.run_id(turn)]
    record["active"] = active
    record["state"]["status"] = run_status
    record["state"]["nodes"][ports.wecom_reply_node]["submitted"] = submitted
    body, status = assistant.result(ports, turn)
    assert status == 502 and "error" in json.loads(body)
    assert not ports.runtime.requests


@pytest.mark.parametrize("reason,status,key", [
    ("stopped by operator", 409, "stopped"),
    ("superseded by a newer message", 200, "superseded"),
])
def test_cancelled_rich_reply_is_not_downloaded(ports, reason, status, key):
    turn, _path = reply_turn(ports)
    ports.turns.finish(turn["id"], "stopped", reason)
    body, actual_status = assistant.result(ports, turn)
    value = json.loads(body)
    assert actual_status == status and value[key] is True
    assert "msg_item" not in value
    assert not ports.runtime.requests


@pytest.mark.parametrize("status", [404, 409, 422, 503])
def test_rich_reply_download_errors_return_structured_original_status(ports, status):
    turn, path = reply_turn(ports)
    ports.runtime.responses[path] = RuntimeHTTPError("fixture reply download failed", status)
    body, actual_status = assistant.result(ports, turn)
    value = json.loads(body)
    assert actual_status == status and value["error"] == "fixture reply download failed"
    assert value["backend"] == "rust"
    assert "text" not in value and "msg_item" not in value
    assert ports.runtime.requests == [path]


@pytest.mark.parametrize("payload", [b"not JSON", b"\xff", b"[]", b"{}", b"null", b"["],
                         ids=["invalid-json", "invalid-utf8", "empty-list", "object", "null", "truncated"])
def test_invalid_rich_reply_payload_is_reported_without_silent_text_fallback(ports, payload):
    turn, path = reply_turn(ports)
    ports.runtime.responses[path] = payload
    body, status = assistant.result(ports, turn)
    value = json.loads(body)
    assert status == 502 and value.get("error")
    assert "text" not in value and "msg_item" not in value
    assert ports.runtime.requests == [path]


def test_oversized_rich_reply_is_bounded_and_reported_as_bad_gateway(ports):
    turn, path = reply_turn(ports)
    limit = 16 * 1024 * 1024
    read_sizes = []

    class BoundedRead(io.BytesIO):
        def read(self, size=-1):
            read_sizes.append(size)
            return super().read(size)

    ports.runtime.responses[path] = BoundedRead(b"x" * (limit + 1))
    body, status = assistant.result(ports, turn)
    value = json.loads(body)
    assert status == 502 and value.get("error")
    assert "text" not in value and "msg_item" not in value
    assert read_sizes == [limit + 1]


@pytest.mark.parametrize("error", [OSError("fixture read failed"), IncompleteRead(b"partial")],
                         ids=["os-error", "incomplete-http-response"])
def test_interrupted_rich_reply_read_returns_structured_bad_gateway(ports, error):
    turn, path = reply_turn(ports)

    class InterruptedRead(io.BytesIO):
        def read(self, size=-1):
            raise error

    ports.runtime.responses[path] = InterruptedRead()
    body, status = assistant.result(ports, turn)
    value = json.loads(body)
    assert status == 502 and value.get("error")
    assert "text" not in value and "msg_item" not in value
    assert ports.runtime.requests == [path]


def test_rich_reply_download_cannot_publish_a_turn_superseded_during_read(ports):
    turn, path = reply_turn(ports)

    class SupersededRead(io.BytesIO):
        def read(self, size=-1):
            with ports.lock:
                ports.turns.finish(turn["id"], "stopped", "superseded by a newer message")
            return super().read(size)

    ports.runtime.responses[path] = SupersededRead(b'[{"msgtype":"image"}]')
    body, status = assistant.result(ports, turn)
    value = json.loads(body)
    assert status == 200 and value["superseded"] is True
    assert value["text"] == "" and "msg_item" not in value
    assert ports.runtime.requests == [path]


def test_lost_admission_response_queries_exact_id_and_does_not_resubmit_or_duplicate(ports):
    ports.facts.lost_response = True
    turn = receive(ports)
    assert settled(ports, turn)["status"] == "completed"
    assert len(ports.facts.admissions) == 1
    assert ports.sessions.get(turn["session"]).run_ids == [assistant.run_id(turn)]


def test_backend_outage_and_rejected_admission_create_no_run_or_session_association(ports):
    ports.facts.unavailable = True
    body, status = assistant.receive(ports, event(), wait=False)
    assert status == 503 and "unavailable" in json.loads(body)["error"]
    assert not ports.sessions.list() and not ports.facts.admissions
    ports.facts.unavailable = False
    ports.facts.admission_error = 409
    turn = receive(ports)
    assert settled(ports, turn)["status"] == "failed"
    assert not ports.sessions.get(turn["session"]).run_ids and not ports.facts.admissions
    assert "graph.turn.started" not in [item.kind for item in ports.sessions.events(turn["session"])]


@pytest.mark.parametrize("attachments", [[{"kind": "image", "path": "/etc/passwd"}], [{"kind": "file"}]])
def test_invalid_attachments_are_rejected_before_graph_lookup_or_turn_creation(ports, attachments):
    ports.facts.unavailable = True
    body, status = assistant.receive(ports, event(message_type="mixed", attachments=attachments), wait=False)
    assert status == 400 and "attachment" in json.loads(body)["error"]
    assert not ports.sessions.list() and not ports.facts.admissions


def media_item(ports, name, data, **metadata):
    path = ports.root / "state/channels/wecom/events/attachment-event" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    return {"path": str(path), "name": name, "kind": "file", **metadata}


def png_bytes():
    from PIL import Image

    buffer = io.BytesIO()
    Image.new("RGB", (8, 8), (64, 160, 90)).save(buffer, "PNG")
    return buffer.getvalue()


def test_text_and_image_attachments_share_frozen_bytes_and_virtual_inputs(ports):
    text = media_item(ports, "z-notes.txt", b"Frozen note evidence", mime_type="text/plain")
    image = media_item(ports, "a-wrong.txt", png_bytes(), mime_type="text/plain")
    turn = receive(ports, event(message_type="mixed", attachments=[text, image]))
    assert settled(ports, turn)["status"] == "completed"
    accepted = ports.facts.admissions[0]
    payload = accepted["attachments"]
    assert [item["name"] for item in payload] == ["z-notes.txt", "a-wrong.txt"]
    assert [item["media_type"] for item in payload] == [None, "image/png"]
    assert base64.b64decode(payload[0]["data_base64"]) == b"Frozen note evidence"
    assert base64.b64decode(payload[1]["data_base64"]) == png_bytes()
    assert "Frozen note evidence" in accepted["input"]["attachment_content"]
    assert [item["path"] for item in accepted["input"]["channel"]["attachments"]] == [
        "/in/channel/z-notes.txt", "/in/channel/a-wrong.txt",
    ]
    stored = json.loads(ports.turns.get(turn["session"], turn["id"])["channel_input"])
    assert "data_base64" not in json.dumps(stored)
    assert stored["attachment_snapshot"]["files"][0]["sha256"] == hashlib.sha256(b"Frozen note evidence").hexdigest()
    ports.facts.records[assistant.run_id(turn)]["attachments"].reverse()
    assert assistant.execute(ports, ports.turns.get(turn["session"], turn["id"]))[1] == 409


def test_pure_image_is_admitted_and_repeated_event_does_not_read_deleted_download(ports, monkeypatch):
    item = media_item(ports, "photo.png", png_bytes(), kind="image")
    value = event(text="", message_type="image", attachments=[item])
    turn = receive(ports, value)
    assert settled(ports, turn)["status"] == "completed"
    Path(item["path"]).unlink()

    def no_read(*_args):
        raise AssertionError("a repeated event cannot reread media")

    monkeypatch.setattr("anchor.channel.media._read_file", no_read)
    assert receive(ports, value)["id"] == turn["id"]
    assert len(ports.facts.admissions) == 1
    changed = {**value, "attachments": [{**item, "name": "changed.png"}]}
    assert assistant.receive(ports, changed, wait=False)[1] == 409


def test_invalid_image_does_not_supersede_the_running_turn(ports):
    ports.facts.auto_complete = False
    current = receive(ports)
    assert ports.facts.admitted.wait(2)
    item = media_item(ports, "broken.png", b"not an image", kind="image", mime_type="image/png")
    body, status = assistant.receive(ports, event(identifier="bad-image", message_type="image", attachments=[item]),
                                     wait=False)
    assert status == 400 and "image" in json.loads(body)["error"]
    assert ports.turns.get(current["session"], current["id"])["status"] == "running"
    assert len(ports.turns.list(current["session"])) == 1 and not ports.facts.controls
    ports.facts.complete(assistant.run_id(current), "Original work finishes")
    settled(ports, current)


def test_accepted_attachment_run_reopens_without_download_or_ingress_snapshot(ports, monkeypatch):
    item = media_item(ports, "notes.txt", b"retained bytes")
    turn = receive(ports, event(attachments=[item]))
    settled(ports, turn)
    current = ports.turns.get(turn["session"], turn["id"])
    stored = json.loads(current["channel_input"])
    Path(item["path"]).unlink()
    Path(stored["attachment_snapshot"]["files"][0]["path"]).unlink()

    def no_read(*_args):
        raise AssertionError("accepted input must come from public Rust facts")

    monkeypatch.setattr("anchor.channel.media._read_file", no_read)
    assert assistant.execute(ports, current)[1] == 200
    assert len(ports.facts.admissions) == 1
    ports.facts.records[assistant.run_id(current)]["attachments"][0]["sha256"] = "0" * 64
    assert assistant.execute(ports, current)[1] == 409


def test_ingress_snapshot_survives_source_deletion_before_run_admission(ports, monkeypatch):
    item = media_item(ports, "frozen.txt", b"Before admission")
    create_turn = ports.create_turn

    def deleted_before_admission(*args, **kwargs):
        Path(item["path"]).unlink()
        return create_turn(*args, **kwargs)

    monkeypatch.setattr(ports, "create_turn", deleted_before_admission)
    turn = receive(ports, event(attachments=[item]))
    assert settled(ports, turn)["status"] == "completed"
    assert base64.b64decode(ports.facts.admissions[0]["attachments"][0]["data_base64"]) == b"Before admission"


def test_legacy_raw_attachment_turn_reuses_owned_public_run_without_source_reads(ports, monkeypatch):
    item = media_item(ports, "retained.txt", b"public retained record")
    turn = receive(ports, event(attachments=[item]))
    settled(ports, turn)
    with ports.turns.connect() as db:
        db.execute("UPDATE turns SET channel_input=? WHERE id=?",
                   (json.dumps({"attachments": [item]}), turn["id"]))
    current = ports.turns.get(turn["session"], turn["id"])
    Path(item["path"]).unlink()
    monkeypatch.setattr("anchor.channel.media._read_file", lambda *_args: pytest.fail("no source reread"))
    assert assistant.execute(ports, current)[1] == 200
    assert len(ports.facts.admissions) == 1
    ports.facts.records[assistant.run_id(current)]["state"]["trigger"]["session"] = "foreign"
    assert assistant.execute(ports, current)[1] == 409


def test_two_users_attachment_runs_are_bound_to_their_own_session(ports):
    ports.facts.auto_complete = False
    first = receive(ports, event(sender="alice", attachments=[media_item(ports, "alice.txt", b"ALICE-ONLY")]))
    second = receive(ports, event(identifier="bob-file", sender="bob",
                                  attachments=[media_item(ports, "bob.txt", b"BOB-ONLY")]))
    wait_until(lambda: len(ports.facts.admissions) == 2)
    for turn in [first, second]:
        snapshot = ports.facts.records[assistant.run_id(turn)]
        assert snapshot["state"]["trigger"]["session"] == turn["session"]
        assert snapshot["state"]["trigger"]["previous_run"] is None
        sender = snapshot["state"]["input"]["channel"]["sender_id"]
        assert ("ALICE-ONLY" if sender == "alice" else "BOB-ONLY") in snapshot["state"]["input"]["attachment_content"]
        assert ("BOB-ONLY" if sender == "alice" else "ALICE-ONLY") not in snapshot["state"]["input"]["attachment_content"]
        ports.facts.complete(assistant.run_id(turn), "Own reply")
        settled(ports, turn)


def test_incoming_snapshot_and_unauthorized_sender_do_not_read_files(ports, monkeypatch):
    item = media_item(ports, "private.txt", b"safe bytes")
    monkeypatch.setattr("anchor.channel.media._read_file", lambda *_args: pytest.fail("no unauthorized read"))
    assert assistant.receive(ports, event(attachments=[item], attachment_snapshot={}), wait=False)[1] == 400
    assert assistant.receive(ports, event(sender="mallory", attachments=[item]), wait=False)[1] == 403
    assert not ports.sessions.list() and not ports.facts.admissions


def test_graph_metadata_checks_reply_and_real_library_plugins_without_python_parser(ports):
    ports.graph_response = ({"graph": "assistant-graph", "node_plugins": {"different": ["wecom"]}}, 200)
    with pytest.raises(RuntimeHTTPError, match="REPLY_NODE"):
        graph_plugins(ports)
    ports.graph_response = ({"graph": "assistant-graph", "node_plugins": {ports.wecom_reply_node: ["missing"]}}, 200)
    body, status = assistant.receive(ports, event(), wait=False)
    assert status == 503 and "missing Plugin" in json.loads(body)["error"]
    assert not ports.sessions.list()


def test_reopen_finds_orphan_admission_before_session_association_and_stops_it(ports, tmp_path):
    ports.sessions.create("existing", graph=ports.wecom_graph, reply_node=ports.wecom_reply_node,
                          channel={"source": "wecom", "sender_id": "alice", "conversation_id": "alice-private"})
    old, _ = ports.turns.create("existing", "old", "Unfinished instruction")
    ports.facts.auto_complete = False
    assert ports.facts.admit(ports.wecom_graph, assistant.run_id(old), "existing", ports.wecom_reply_node,
                             {"message": "Unfinished instruction"}, None)[1] == 202
    assert not ports.sessions.get("existing").run_ids
    reopened = PublicPorts(tmp_path, ports.facts)
    reopened.facts.auto_complete = True
    try:
        body, status = reopened.create_turn("existing", "new", "Followup", {"attachments": []})
        assert status == 202
        new = json.loads(body)["turn"]
        assert settled(reopened, new)["status"] == "completed"
        assert reopened.facts.admissions[-1]["previous_run"] == assistant.run_id(old)
        assert reopened.facts.records[assistant.run_id(old)]["active"] is False
        assert reopened.facts.admissions[-1]["input"]["interrupted_messages"] == ["Unfinished instruction"]
    finally:
        reopened.close()


def test_foreign_session_run_cannot_be_used_as_predecessor(ports):
    old = receive(ports)
    settled(ports, old)
    foreign = "foreign-run"
    ports.facts.records[foreign] = copy.deepcopy(ports.facts.records[assistant.run_id(old)])
    ports.facts.records[foreign]["state"]["trigger"]["session"] = "someone-else"
    ports.sessions.attach_run(old["session"], foreign)
    new = receive(ports, event(identifier="event-2"))
    assert settled(ports, new)["status"] == "failed"
    assert len(ports.facts.admissions) == 1 and not ports.facts.controls


def test_legacy_history_is_readonly_and_cannot_be_inherited_by_rust(ports):
    old = receive(ports)
    settled(ports, old)
    identifier = assistant.run_id(old)
    record = ports.facts.records[identifier]
    record["backend"] = "legacy"
    record["read_only"] = True
    record["state"]["status"] = "finished"
    record["state"]["trigger"].pop("reply_node")
    before = copy.deepcopy(record)
    assert assistant.history(ports, old["session"])[-1]["text"] == "Reply: Hello"
    new = receive(ports, event(identifier="event-2"))
    assert settled(ports, new)["status"] == "failed"
    assert ports.facts.records[identifier] == before and not ports.facts.controls
    assert len(ports.facts.admissions) == 1


def test_completed_remote_reply_survives_interrupted_turn_and_outage_is_not_empty_history(ports):
    turn = receive(ports)
    settled(ports, turn)
    with ports.turns.connect() as db:
        db.execute("UPDATE turns SET status='interrupted',error='lost settlement' WHERE id=?", (turn["id"],))
    assert assistant.result(ports, turn)[1] == 200
    ports.facts.unavailable = True
    assert assistant.result(ports, turn)[1] == 503
    with pytest.raises(RuntimeHTTPError, match="unavailable"):
        assistant.history(ports, turn["session"])


class Process:
    def __init__(self):
        self.terminated = False
        self.returncode = None
        self.pid = 123

    def poll(self):
        return self.returncode

    def terminate(self):
        self.terminated = True
        self.returncode = -15

    def wait(self, timeout):
        return self.returncode

    def kill(self):
        self.returncode = -9


def test_supervisor_scans_compiled_plugins_without_local_graph_and_keeps_gateway_on_outage(tmp_path, monkeypatch):
    def no_local_graphs():
        raise AssertionError("local Graph discovery is forbidden")

    def no_parser(*_args):
        raise AssertionError("Python authoring parser is forbidden")

    monkeypatch.setattr("anchor.channel.supervisor.graph_module.load", no_parser)
    projected = [{"module/assistant": ["wecom"], "other": []}]
    failed = False

    def projection():
        if failed:
            raise RuntimeHTTPError("fixture Runtime unavailable", 503)
        return projected

    supervisor = ChannelSupervisor(tmp_path, Library(tmp_path / "library"), no_local_graphs,
                                   callback_url="http://localhost/events", api_key="fixture",
                                   node_plugins=projection)
    started = []
    monkeypatch.setattr(supervisor, "_start", lambda platform, spec: started.append((platform, spec)))
    supervisor._reconcile()
    assert len(started) == 1 and started[0][0] == "wecom"
    assert started[0][1]["entrypoint_path"] == str(tmp_path / "library" / "plugins" / "wecom" / "ws_gateway.py")
    process = Process()
    supervisor.processes["wecom"] = process
    failed = True
    supervisor._reconcile()
    assert supervisor.processes["wecom"] is process and not process.terminated
    failed = False
    projected.clear()
    supervisor._reconcile()
    assert process.terminated and not supervisor.processes


@pytest.mark.parametrize("existing", [False, True], ids=["first-start", "replace-existing"])
def test_supervisor_publishes_private_descriptor_atomically_and_cleans_it_after_polling(tmp_path, monkeypatch, existing):
    descriptor = tmp_path / "state/channels/wecom/control.json"
    previous = b'{"socket":"old.sock","token":"old-token"}'
    if existing:
        descriptor.parent.mkdir(parents=True)
        descriptor.write_bytes(previous)
        descriptor.chmod(0o600)
    monkeypatch.setenv("WECOM_CHANNEL_STATE", str(tmp_path / "other-profile"))
    monkeypatch.setenv("ANCHOR_WECOM_SEND_USERS", "")
    monkeypatch.setenv("ANCHOR_WECOM_USERS", "alice")
    process = Process()
    launches = []

    def popen(command, **kwargs):
        launches.append((command, kwargs))
        return process

    monkeypatch.setattr("anchor.channel.supervisor.subprocess.Popen", popen)
    monkeypatch.setattr("anchor.channel.supervisor.secrets.token_urlsafe", lambda _size: "new-gateway-token")
    import os

    replace = os.replace
    published = []

    def checked_replace(source, destination):
        source, destination = Path(source), Path(destination)
        assert destination == descriptor
        assert source.stat().st_mode & 0o777 == 0o600
        assert descriptor.exists() is existing
        if existing:
            assert descriptor.read_bytes() == previous
        published.append(json.loads(source.read_text()))
        replace(source, destination)

    monkeypatch.setattr("anchor.channel.supervisor.os.replace", checked_replace)
    supervisor = ChannelSupervisor(tmp_path, Library(tmp_path / "library"), lambda: [],
                                   callback_url="http://localhost/events", api_key="fixture",
                                   node_plugins=lambda: [{"assistant": ["wecom"]}])
    try:
        supervisor._reconcile()
        expected = {"socket": str(descriptor.with_name("control.sock")), "token": "new-gateway-token"}
        assert published == [expected] and json.loads(descriptor.read_text()) == expected
        assert descriptor.stat().st_mode & 0o777 == 0o600
        assert not (tmp_path / "other-profile").exists()
        assert not list(descriptor.parent.glob(".control-*"))
        env = launches[0][1]["env"]
        assert env["WECOM_CHANNEL_STATE"] == str(descriptor.parent)
        assert env["ANCHOR_CHANNEL_CONTROL_TOKEN"] == expected["token"]
        assert env["ANCHOR_WECOM_SEND_USERS"] == "alice"
        supervisor._reconcile()
        assert len(launches) == 1 and descriptor.exists()
    finally:
        supervisor.stop()
    assert process.terminated and not supervisor.processes and not supervisor.controls
    assert not descriptor.exists()


def test_supervisor_rotates_descriptor_after_gateway_exit_and_removes_it_when_unmounted(tmp_path, monkeypatch):
    launches = []

    def popen(_command, **kwargs):
        process = Process()
        launches.append((process, kwargs["env"]))
        return process

    monkeypatch.setattr("anchor.channel.supervisor.subprocess.Popen", popen)
    tokens = iter(["gateway-one", "gateway-two"])
    monkeypatch.setattr("anchor.channel.supervisor.secrets.token_urlsafe", lambda _size: next(tokens))
    projected = [{"assistant": ["wecom"]}]
    supervisor = ChannelSupervisor(tmp_path, Library(tmp_path / "library"), lambda: [],
                                   callback_url="http://localhost/events", api_key="fixture",
                                   node_plugins=lambda: projected)
    descriptor = tmp_path / "state/channels/wecom/control.json"
    try:
        supervisor._reconcile()
        assert json.loads(descriptor.read_text())["token"] == "gateway-one"
        launches[0][0].returncode = 1
        supervisor._reconcile()
        assert len(launches) == 2
        assert json.loads(descriptor.read_text())["token"] == "gateway-two"
        assert descriptor.stat().st_mode & 0o777 == 0o600
        assert supervisor.controls["wecom"][1] == launches[1][1]["ANCHOR_CHANNEL_CONTROL_TOKEN"] == "gateway-two"
        projected.clear()
        supervisor._reconcile()
        assert launches[1][0].terminated
        assert not descriptor.exists() and not supervisor.processes and not supervisor.controls
    finally:
        supervisor.stop()


def test_previous_recovers_background_binding_before_session_association(tmp_path):
    from anchor.channel.runtime import _previous

    ports = PublicPorts(tmp_path)
    channel = {"source": "wecom", "sender_id": "alice"}
    session = ports.sessions.create("alice-session", graph="assistant-graph",
                                    reply_node="module/assistant", channel=channel)
    turn, _ = ports.turns.create(session.id, "next", "continue")
    ports.facts.records["background"] = {
        "run": "background", "graph": session.graph, "backend": "rust", "active": False,
        "control_requested": None,
        "session_call": {"context": {"session": session.id, "channel": channel}, "status": "pending"},
        "state": {"status": "stopped", "trigger": {"source": "graph_call", "session": session.id,
                    "reply_node": session.reply_node, "previous_run": None}},
    }
    assert session.run_ids == []
    assert _previous(ports, session, turn) == "background"
