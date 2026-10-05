"""Real-provider call.session acceptance with local gateway ACKs only.

Uses the unchanged WeCom Graph/Plugin and the production Scheduler, Handler,
background tick, Rust Host and ControlServer/EventLedger. Disposable gates
hold dispatch or delivery across two-service restarts; no external recipient,
production gateway, deployment or production data is involved.
"""
from __future__ import annotations

import argparse
import hashlib
from http.server import ThreadingHTTPServer
import json
import os
from pathlib import Path
import secrets
import shlex
import signal
import sqlite3
import tempfile
import threading
import time
from urllib.error import HTTPError
from urllib.request import Request
from uuid import uuid4

from anchor.channel.control import request
from anchor.runtime.secrets import load_dotenv
from rust_channel_smoke import Deployment, GRAPH, ROOT
from rust_channel_tools_smoke import Gateway
from rust_platform_plugin_smoke import (
    Api, Evidence, MODEL_KEYS, Service, require, service_envs, tree_hashes,
    unused_ports, wait_until,
)


def platform_fixture(root: Path, port: int) -> None:
    """Run the actual platform ports with a local, operator-owned send adapter."""
    from anchor.channel.runtime_background import tick
    from anchor.serve import Handler, Scheduler

    descriptor = json.loads((root / "control.json").read_text())

    class LocalSupervisor:
        def send(self, source, payload):
            require(source == "wecom", "Unexpected gateway source")
            # This fixture gate acts before the existing gateway claims a send.
            while not (root / "delivery.enabled").exists():
                time.sleep(0.05)
            return request(Path(descriptor["socket"]), descriptor["token"], payload)

        def stop(self):
            pass

    scheduler = Scheduler(root / "platform", root / "runtime.json")
    scheduler.channel_supervisor = LocalSupervisor()

    class BoundHandler(Handler):
        pass

    BoundHandler.scheduler = scheduler
    server = ThreadingHTTPServer(("127.0.0.1", port), BoundHandler)
    stopping = threading.Event()

    def shutdown(_signum, _frame):
        stopping.set()
        threading.Thread(target=server.shutdown, daemon=True).start()

    def scan():
        while not stopping.is_set():
            scheduler.tick_schedules()
            if (root / "background.enabled").exists():
                tick(scheduler)
            stopping.wait(0.1)

    previous = signal.signal(signal.SIGTERM, shutdown)
    try:
        (scheduler.root / "workspaces").mkdir(parents=True, exist_ok=True)
        scheduler.resume_all()
        threading.Thread(target=scan, daemon=True).start()
        server.serve_forever(poll_interval=0.1)
    finally:
        stopping.set()
        scheduler.stop_channels()
        server.server_close()
        signal.signal(signal.SIGTERM, previous)


class SessionDeployment(Deployment):
    def __init__(self, evidence: Evidence, binary: Path, token: str):
        super().__init__(evidence, binary)
        self.token = token

    def start(self):
        self.generation += 1
        rust_port, python_port = unused_ports()
        rust, platform = service_envs(self.paths, ROOT / "src", rust_port)
        keys = json.dumps([self.token])
        rust.update({
            "ANCHOR_RUNNER_ALLOWED_COMMANDS": "sh,cat,printf,cp,sleep,true,python3",
            "ANCHOR_API_KEYS": keys,
            "ANCHOR_SESSION_HOST_URL": f"http://127.0.0.1:{python_port}",
            "ANCHOR_SESSION_HOST_TOKEN": self.token,
        })
        platform.update({
            "ANCHOR_API_KEYS": keys, "ANCHOR_RUNTIME_API_KEY": self.token,
            "ANCHOR_WECOM_GRAPH": GRAPH, "ANCHOR_WECOM_REPLY_NODE": "assistant",
            "ANCHOR_WECOM_USERS": "alice,bob", "ANCHOR_WECOM_SEND_USERS": "alice,bob",
        })
        self.rust, self.api = Api(rust_port), Api(python_port)
        for api in (self.rust, self.api):
            api.opener.addheaders = [("Authorization", f"Bearer {self.token}")]
        rust_service = Service(f"rust-{self.generation}", [str(self.binary), "serve"], rust, self.evidence)
        self.services.append(rust_service)
        wait_until(lambda: self.rust.ready(rust_service, "/health"), 30, "Rust readiness")
        platform_service = Service(f"platform-{self.generation}", [str(ROOT / ".venv/bin/python"),
            str(Path(__file__).resolve()), "--platform-fixture", "--fixture-root", str(self.evidence.root),
            "--port", str(python_port)], platform, self.evidence)
        self.services.append(platform_service)
        wait_until(lambda: self.api.ready(platform_service, "/graphs"), 30, "platform readiness")

    def event(self, label: str, user: str, prompt: str, timeout: float, event=None):
        event = event or {"source": "wecom", "event_id": f"{label}-{uuid4()}", "sender_id": user,
            "conversation_id": user, "message_type": "text", "text": prompt,
            "metadata": {"chat_type": "single"}}
        wire = Request(self.api.base + "/v1/channels/wecom/events", method="POST",
            data=json.dumps({"event": event}).encode(), headers={"Content-Type": "application/json"})
        try:
            with self.api.opener.open(wire, timeout=timeout) as response:
                reply = json.loads(response.read())
        except HTTPError as error:
            raise RuntimeError(f"Channel event {label} returned HTTP {error.code}; inspect service logs") from None
        self.evidence.save(f"reply-{label}.json", {"event": event, "reply": reply})
        require(not reply.get("error"), f"Channel event {label} failed; inspect reply evidence")
        return event, reply

    def detail(self, run: str) -> dict:
        for service in self.services:
            service.check()
        value = self.api.expect("GET", f"/runs/{run}")
        self.evidence.save(f"run-{run}.json", value)
        require(value.get("backend") == "rust", "Expected a Rust-owned Run")
        return value

    def completed(self, run: str, timeout: float) -> dict:
        def settled():
            detail = self.detail(run)
            require(detail["state"]["status"] not in {"failed", "aborted", "budget_stopped"},
                    "Run failed; inspect its retained detail")
            return detail if detail["state"]["status"] == "completed" and not detail["active"] else None
        return wait_until(settled, timeout, "Run completion")

    def children(self, parent: str) -> list[str]:
        return sorted(item["run"] for item in self.rust.expect("GET", "/runs")["runs"]
                      if item.get("trigger", {}).get("source") == "graph_call"
                      and item["trigger"].get("run") == parent)

    def identifiers(self) -> set[str]:
        return {item["run"] for item in self.rust.expect("GET", "/runs")["runs"]}


def parent_graph(session: str, mode: str, marker: str, private: str) -> dict:
    command = lambda value: shlex.join(["sh", "-c", value])
    call = {"graph": GRAPH, "mode": mode, "session": session,
        "input_map": {"message": "/message", "mapped_value": "/mapped_value",
                      "session": "/forged_session", "channel": "/forged_channel"},
        "files": [{"node": "publish", "path": "request.txt", "as": "request.txt"}]}
    ops = {"publish": {"run": command(
        f"printf %s {shlex.quote(marker)} > request.txt\n"
        f"printf %s {shlex.quote(private)} > private.txt")}, "invoke": {"call": call}}
    nodes = [{"id": "publish", "op": "publish"}, {"id": "invoke", "op": "invoke"}]
    edges = [{"from": "publish", "to": "invoke"}]
    if mode == "wait":
        call["result"] = {"node": "assistant", "files": ["request-copy.txt"]}
        ops["verify"] = {"run": command(
            f'test "$(cat /in/invoke/result/request-copy.txt)" = {shlex.quote(marker)} '
            "&& cp /in/invoke/result/request-copy.txt verified.txt")}
        nodes.append({"id": "verify", "op": "verify"})
        edges.append({"from": "invoke", "to": "verify"})
    return {"entry": "publish", "agents": {}, "ops": ops, "nodes": nodes, "edges": edges}


def first_provider_request(deployment: SessionDeployment, run: str) -> str:
    record = json.loads((deployment.paths["state"] / "runs" / f"{run}.json").read_text())
    key = record["results"]["assistant"][0]["key"]
    durable = ":".join(str(key[name]) for name in ("run_id", "graph_digest", "node_id", "invocation"))
    stem = "np1-" + hashlib.sha256(durable.encode()).hexdigest()
    recording = deployment.paths["state"] / "io-harness/store" / f"{stem}.recordings"
    require(bool(list(recording.glob("*/rig-response.json"))), "Missing real provider response recording")
    return (recording / "00000000000000000001/rig-request.json").read_text()


def delivery_receipt(gateway: Gateway, child: str, detail: dict) -> dict:
    identifier = hashlib.sha256(f"graph-call-reply:{child}".encode()).hexdigest()
    text = detail["state"]["nodes"]["assistant"]["submission"]
    with sqlite3.connect(gateway.server.ledger.path) as database:
        row = database.execute("SELECT status, reply FROM channel_events WHERE source=? AND event_id=?",
                               ("wecom-outbound", identifier)).fetchone()
    digest = hashlib.sha256(json.dumps(["alice", text], ensure_ascii=False).encode()).hexdigest()
    require(row == ("completed", digest), "Gateway did not persist the matching confirmed ACK")
    duplicate = request(gateway.path, gateway.token, {"operation": "send", "request_id": identifier,
        "userid": "alice", "content": text})
    require(duplicate.get("accepted") and duplicate.get("duplicate"), "Gateway receipt did not deduplicate")
    return {"run": child, "request_id": identifier, "status": row[0], "content_sha256": digest}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "rust/target/release/anchor-runner-host")
    parser.add_argument("--timeout", type=float, default=180)
    parser.add_argument("--platform-fixture", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--fixture-root", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("--port", type=int, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.platform_fixture:
        require(args.fixture_root is not None and args.port is not None, "Missing platform fixture arguments")
        platform_fixture(args.fixture_root, args.port)
        return
    load_dotenv(ROOT / ".env")
    require(all(os.environ.get(key) for key in MODEL_KEYS), "Real provider configuration required")
    require(args.binary.is_file(), "Build the Rust Host binary before running acceptance")
    (ROOT / ".local").mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="rust-session-call-", dir=ROOT / ".local"))
    token = secrets.token_hex(32)
    gateway = Gateway(root)
    evidence = Evidence(root, (*[os.environ[key] for key in MODEL_KEYS], token, gateway.token))
    descriptor = root / "control.json"
    descriptor.write_text(json.dumps({"socket": str(gateway.path), "token": gateway.token}))
    descriptor.chmod(0o600)
    deployment = SessionDeployment(evidence, args.binary.resolve(), token)
    previous_users = os.environ.get("ANCHOR_WECOM_SEND_USERS")
    os.environ["ANCHOR_WECOM_SEND_USERS"] = "alice,bob"
    plugin_hashes = tree_hashes(ROOT / "plugins/wecom")
    report = {"status": "failed", "scope": "real Rust provider; local ControlServer/EventLedger ACK; no external delivery",
        "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(), "runs": [], "receipts": []}
    gateway_started = False
    try:
        gateway.start()
        gateway_started = True
        (root / "background.enabled").touch()
        deployment.start()
        graph = json.loads((ROOT / "examples/graphs/wecom-assistant.json").read_text())
        deployment.api.expect("POST", "/graphs", {"name": GRAPH, "definition": graph}, status=201)
        nonce, other = f"ALICE-{uuid4().hex}", f"BOB-{uuid4().hex}"
        _, seed = deployment.event("seed-alice", "alice", f"Remember my private phrase {nonce}. "
            "Use anchor_run to write only this phrase to /workspace/note.txt and include it in your final reply. "
            "Do not use proactive send tools.", args.timeout)
        deployment.check_run(seed)
        status, contents = deployment.api.raw("GET", f"/runs/{seed['run']}/files/assistant/note.txt?download=1")
        require(status == 200 and contents.decode().strip() == nonce and nonce in seed["text"],
                "The first foreground turn did not preserve its private phrase")
        _, bob = deployment.event("seed-bob", "bob", f"Remember my private phrase {other}. "
            "Reply with this phrase only. Do not use proactive send tools.", args.timeout)
        deployment.check_run(bob)
        report["runs"].extend([seed["run"], bob["run"]])
        rejected = deployment.api.expect("POST", "/v1/runtime/session-calls/resolve", {
            "parent_run": seed["run"], "session": bob["session"], "graph": GRAPH}, status=400)
        require("conversation" in rejected.get("error", ""), "Unexpected conversation caller rejection")
        evidence.save("cross-session-rejection.json", rejected)

        marker, mapped, private = (f"FILE-{uuid4().hex}", f"MAPPED-{uuid4().hex}", f"UNSELECTED-{uuid4().hex}")
        message = ("This is an authorized background task. Use anchor_run to read /in/call/request.txt and "
            "/previous/note.txt, copy both files to /workspace/request-copy.txt and /workspace/note.txt, "
            "and write /workspace/background.txt containing the file marker, my remembered phrase and "
            "input.mapped_value. Inspect the actual earlier tool records with anchor_conversation_history. "
            "Include all three values in the final summary. Do not call proactive send tools: the host delivers the summary.")
        parent_input = {"message": message, "mapped_value": mapped, "unselected": private,
                        "forged_session": "forged", "forged_channel": {"sender_id": "bob"}}
        deployment.api.expect("POST", "/graphs", {"name": "session-wait",
            "definition": parent_graph(seed["session"], "wait", marker, private)}, status=201)
        parent = deployment.api.expect("POST", "/trigger", {"graph": "session-wait", "input": parent_input}, status=202)["run"]
        child = wait_until(lambda: deployment.children(parent), 30, "wait child admission")
        require(len(child) == 1, "Wait admission created multiple children")
        child = child[0]
        waiting = deployment.completed(child, args.timeout)
        require(waiting["session_call"]["status"] == "pending" and not gateway.sent,
                "Delivery gate did not hold the completed background reply")
        summary = waiting["state"]["nodes"]["assistant"]["submission"]
        require(all(value in summary for value in (nonce, marker, mapped)) and other not in summary,
                "The background reply did not verify the mapped input and its own conversation")
        for filename in ("note.txt", "background.txt"):
            status, contents = deployment.api.raw("GET", f"/runs/{child}/files/assistant/{filename}?download=1")
            expected = (nonce,) if filename == "note.txt" else (nonce, marker, mapped)
            require(status == 200 and all(value in contents.decode() for value in expected)
                    and other not in contents.decode(), "Background files did not preserve the verified source values")
        wait_until(lambda: deployment.detail(parent)["state"]["status"] == "waiting_call", 10, "wait parent parking")
        child_input = waiting["state"]["input"]
        require(child_input["session"] == seed["session"] and child_input["channel"]["sender_id"] == "alice"
                and child_input["message"] == message and child_input["mapped_value"] == mapped
                and private not in json.dumps(child_input), "Source mapping bypassed trusted identity or copied unselected input")
        selected = deployment.paths["state"] / "artifacts/call-inputs" / child
        require((selected / "request.txt").read_text() == marker and not (selected / "private.txt").exists(),
                "The call input snapshot did not contain exactly the selected source file")
        evidence.save("source-mapping.json", {"parent": parent, "child": child, "input": child_input,
            "selected_file": "request.txt", "file_sha256": hashlib.sha256(marker.encode()).hexdigest(),
            "unselected_file_absent": True})
        first = first_provider_request(deployment, child)
        require(nonce in first and other not in first and private not in first,
                "Background child did not receive only the bound user's history and mapped input")
        identifiers = deployment.identifiers()
        deployment.stop()
        (root / "delivery.enabled").touch()
        deployment.start()
        parent_detail = deployment.completed(parent, args.timeout)
        delivered = deployment.detail(child)
        require(delivered["session_call"]["status"] == "delivered" and len(gateway.sent) == 1,
                "Wait parent completed without one confirmed delivery")
        require(deployment.identifiers() == identifiers and deployment.children(parent) == [child],
                "Pending-delivery restart replaced or duplicated the child")
        status, contents = deployment.api.raw("GET", f"/runs/{parent}/files/verify/verified.txt?download=1")
        require(status == 200 and contents.decode() == marker, "Wait result file did not return to the source Graph")
        require(parent_detail["state"]["nodes"]["verify"]["submitted"], "Wait parent did not execute its final verification")
        report["receipts"].append(delivery_receipt(gateway, child, delivered))
        require(len(gateway.sent) == 1, "Rechecking the confirmed receipt sent a second message")
        report["runs"].extend([parent, child])

        _, followup = deployment.event("foreground-followup", "alice",
            "Continue our conversation. State my remembered private phrase and the background task's file marker and mapped value. "
            "Use anchor_conversation_history to inspect actual earlier tools, and anchor_run to read /previous/note.txt "
            "and /previous/background.txt. Copy note.txt to /workspace/note.txt. Do not use proactive send tools.", args.timeout)
        require(all(value in followup["text"] for value in (nonce, marker, mapped)) and other not in followup["text"],
                "Foreground continuation lost the background history")
        deployment.check_followup_trace(followup, nonce, other)
        first = first_provider_request(deployment, followup["run"])
        require(marker in first and mapped in first and private not in first,
                "Foreground follow-up did not seed native background history before its tools")
        report["runs"].append(followup["run"])

        (root / "background.enabled").unlink()
        deployment.api.expect("POST", "/graphs", {"name": "session-detach",
            "definition": parent_graph(seed["session"], "detach", marker, private)}, status=201)
        detached_parent = deployment.api.expect("POST", "/trigger", {"graph": "session-detach", "input": parent_input}, status=202)["run"]
        deployment.completed(detached_parent, args.timeout)
        detached = deployment.children(detached_parent)
        require(len(detached) == 1, "Detach admission created multiple children")
        detached = detached[0]
        pending = deployment.detail(detached)
        require(pending["state"]["status"] == "ready" and pending["session_call"]["status"] == "pending"
                and not pending["active"] and len(gateway.sent) == 1, "Detach did not return before background dispatch")
        identifiers = deployment.identifiers()
        deployment.stop()
        deployment.start()
        require(deployment.identifiers() == identifiers and deployment.children(detached_parent) == [detached]
                and len(gateway.sent) == 1, "Restart duplicated an admitted child or a completed send")
        (root / "background.enabled").touch()
        def detached_delivered():
            detail = deployment.detail(detached)
            require(detail["session_call"]["status"] != "failed", "Detached background delivery failed")
            return detail if detail["session_call"]["status"] == "delivered" else None
        done = wait_until(detached_delivered, args.timeout, "detached execution and delivery")
        report["receipts"].append(delivery_receipt(gateway, detached, done))
        require(len(gateway.sent) == 2 and all(item["user"] == "alice" for item in gateway.sent),
                "Unexpected delivery recipient or duplicate send")
        require(deployment.identifiers() == identifiers, "Background dispatch created a replacement Run")
        require(not list((deployment.paths["platform"] / "workspaces").glob("*/runs/*/run.json")),
                "Python created a second Graph Run")
        require(deployment.api.expect("GET", f"/graphs/{GRAPH}")["definition"] == graph
                and tree_hashes(deployment.paths["library"] / "plugins/wecom") == plugin_hashes,
                "Original WeCom Graph or Plugin changed")
        report["runs"].extend([detached_parent, detached])
        report.update(status="passed", wait_parent_after_ack=True, detach_before_dispatch=True,
            pending_delivery_restart=True, pending_dispatch_restart=True, no_duplicate_child_or_send=True,
            mapped_input_and_files=True, foreground_sees_background_history=True,
            conversation_caller_rejected=True, graph_and_plugin_unchanged=True, local_ack_count=len(gateway.sent))
        evidence.save("deliveries.json", gateway.sent)
    finally:
        deployment.stop()
        if gateway_started:
            gateway.stop()
        if previous_users is None:
            os.environ.pop("ANCHOR_WECOM_SEND_USERS", None)
        else:
            os.environ["ANCHOR_WECOM_SEND_USERS"] = previous_users
        evidence.save("evidence.json", report)
        print(json.dumps({"status": report["status"], "evidence": str(root / "evidence.json")}), flush=True)


if __name__ == "__main__":
    main()
