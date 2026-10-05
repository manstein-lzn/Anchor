"""Isolated real-provider text channel acceptance; never sends WeCom messages.

Uses the unchanged WeCom Graph and Plugin, normalized trusted channel events,
the platform Turn API and a Rust service. Credentials go only to the Rust
provider adapter. Retains the same service roots across restart.
"""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
from http.client import HTTPConnection
import hashlib
import json
import os
from pathlib import Path
import shutil
import tempfile
from urllib.parse import urlsplit
from uuid import uuid4

from rust_platform_plugin_smoke import (
    Api, Evidence, MODEL_KEYS, Service, require, service_envs, unused_ports, wait_until,
)

ROOT = Path(__file__).resolve().parents[1]
GRAPH = "wecom-assistant"


class Deployment:
    def __init__(self, evidence: Evidence, binary: Path, *, rust_env: dict | None = None):
        self.evidence, self.binary = evidence, binary
        self.rust_env = rust_env or {}
        self.paths = {name: evidence.root / name for name in ("platform", "bootstrap", "catalog", "state", "work")}
        self.paths["library"] = self.paths["platform"] / "library"
        shutil.copytree(ROOT / "plugins/wecom", self.paths["library"] / "plugins/wecom")
        self.paths["bootstrap"].mkdir()
        self.paths["catalog"].mkdir()
        (self.paths["bootstrap"] / "graph.json").write_text(json.dumps({
            "entry": "start", "agents": {}, "ops": {"start": {"run": "true"}},
            "nodes": [{"id": "start", "op": "start"}], "edges": [],
        }))
        (self.paths["bootstrap"] / "manifest.json").write_text('{"format":1,"graph":"graph.json","plugins":[]}')
        (evidence.root / "runtime.json").write_text("{}")
        self.services: list[Service] = []
        self.generation = 0

    def start(self):
        self.generation += 1
        rust_port, python_port = unused_ports()
        rust, platform = service_envs(self.paths, ROOT / "src", rust_port)
        # Neither process inherits bot/application credentials, so no external
        # gateway or application message can be sent by this acceptance.
        rust["ANCHOR_RUNNER_ALLOWED_COMMANDS"] = "sh,cat,printf,cp,sleep,true,python3"
        rust.update(self.rust_env)
        platform.update({"ANCHOR_WECOM_GRAPH": GRAPH, "ANCHOR_WECOM_REPLY_NODE": "assistant",
                         "ANCHOR_WECOM_USERS": "alice,bob"})
        self.rust, self.api = Api(rust_port), Api(python_port)
        self.services.append(Service(f"rust-{self.generation}", [str(self.binary), "serve"], rust, self.evidence))
        wait_until(lambda: self.rust.ready(self.services[0], "/health"), 30, "Rust readiness")
        self.services.append(Service(f"platform-{self.generation}", [str(ROOT / ".venv/bin/python"), "-m", "anchor",
            "--root", str(self.paths["platform"]), "--config", str(self.evidence.root / "runtime.json"),
            "--host", "127.0.0.1", "--port", str(python_port)], platform, self.evidence))
        wait_until(lambda: self.api.ready(self.services[1], "/graphs"), 30, "platform readiness")

    def stop(self):
        for service in reversed(self.services):
            service.stop()
        self.services.clear()

    def event(self, label: str, user: str, prompt: str, timeout: float, event=None):
        event = event or {"source": "wecom", "event_id": f"{label}-{uuid4()}", "sender_id": user,
                          "conversation_id": user, "message_type": "text", "text": prompt,
                          "metadata": {"chat_type": "single"}}
        endpoint = urlsplit(self.api.base)
        client = HTTPConnection(endpoint.hostname, endpoint.port, timeout=timeout)
        wire = []
        try:
            client.request("POST", "/v1/channels/wecom/events", json.dumps({"event": event}),
                           {"Content-Type": "application/json", "Accept": "text/event-stream"})
            response = client.getresponse()
            require(response.status == 200, f"Channel admission failed: {label} HTTP {response.status}")
            for line in response:
                wire.append(line.decode())
                if line.startswith(b"data: "):
                    value = json.loads(line[6:])
                    if value.get("run") or value.get("error"):
                        self.evidence.save(f"reply-{label}.json", {"event": event, "reply": value, "wire": wire})
                        require(not value.get("error"), f"Channel execution failed: {label}; inspect reply evidence")
                        return event, value
            raise RuntimeError(f"Channel stream ended without final reply: {label}")
        finally:
            client.close()

    def check_run(self, reply: dict):
        detail = self.api.expect("GET", f"/runs/{reply['run']}")
        require(detail.get("backend") == "rust" and detail["state"]["status"] == "completed"
                and not detail["active"], "Expected a completed Rust-owned Run")
        self.evidence.save(f"run-{reply['run']}.json", detail)
        return detail

    def check_followup_trace(self, reply: dict, nonce: str, forbidden: str):
        detail = self.check_run(reply)
        commands = [command for messages in detail["traces"].values() for message in messages
                    for command in message.get("commands", [])]
        require(any(command.startswith("anchor_conversation_history ") for command in commands),
                "Follow-up did not inspect actual prior tool records")
        require(any("/previous/note.txt" in command for command in commands), "Follow-up did not read previous files")
        record = json.loads((self.paths["state"] / "runs" / f"{reply['run']}.json").read_text())
        key = record["results"]["assistant"][0]["key"]
        durable = ":".join(str(key[name]) for name in ("run_id", "graph_digest", "node_id", "invocation"))
        stem = "np1-" + hashlib.sha256(durable.encode()).hexdigest()
        recording = self.paths["state"] / "io-harness/store" / f"{stem}.recordings"
        first_request = (recording / "00000000000000000001/rig-request.json").read_text()
        require(nonce in first_request and forbidden not in first_request,
                "Native Session did not seed isolated prior history before any follow-up tool")
        require(bool(list(recording.glob("*/rig-response.json"))), "Model responses were not retained")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "rust/target/release/anchor-runner-host")
    parser.add_argument("--timeout", type=float, default=180)
    args = parser.parse_args()
    require(all(os.environ.get(key) for key in MODEL_KEYS), "Real provider environment is required")
    root = Path(tempfile.mkdtemp(prefix="rust-channel-", dir=ROOT / ".local"))
    evidence = Evidence(root, tuple(os.environ[key] for key in MODEL_KEYS))
    deployment = Deployment(evidence, args.binary.resolve())
    report = {"status": "failed", "scope": "normalized text channel; no external delivery", "runs": []}
    try:
        deployment.start()
        graph = json.loads((ROOT / "examples/graphs/wecom-assistant.json").read_text())
        deployment.api.expect("POST", "/graphs", {"name": GRAPH, "definition": graph}, status=201)
        require(deployment.api.expect("GET", f"/graphs/{GRAPH}")["definition"] == graph,
                "Original Graph was changed")
        nonce_a, nonce_b = f"ALICE-{uuid4().hex}", f"BOB-{uuid4().hex}"
        prompts = {
            "alice": f"本轮验收：请记住我的口令 {nonce_a}。通过 anchor_run 写入 /workspace/note.txt，内容就是该口令。最终回复包含口令。不要发送外部消息。",
            "bob": f"本轮验收：请记住我的口令 {nonce_b}。通过 anchor_run 写入 /workspace/note.txt，内容就是该口令。最终回复包含口令。不要发送外部消息。",
        }
        with ThreadPoolExecutor(max_workers=2) as executor:
            pending = {user: executor.submit(deployment.event, f"{user}-first", user, prompt, args.timeout)
                       for user, prompt in prompts.items()}
            first = {user: task.result() for user, task in pending.items()}
        for user, nonce, forbidden in (("alice", nonce_a, nonce_b), ("bob", nonce_b, nonce_a)):
            _, reply = first[user]
            require(nonce in reply["text"] and forbidden not in reply["text"], "Conversation identity leaked")
            deployment.check_run(reply)
            status, data = deployment.api.raw("GET", f"/runs/{reply['run']}/files/assistant/note.txt?download=1")
            require(status == 200 and data.decode().strip() == nonce, "First-turn artifact differs from the user's nonce")
            report["runs"].append(reply["run"])
        event, reply = first["alice"]
        _, duplicate = deployment.event("alice-duplicate", "alice", "", args.timeout, event=event)
        require(duplicate == reply, "Duplicate channel event changed its result")
        require(len(deployment.rust.expect("GET", "/runs")["runs"]) == 2, "Duplicate event created a Run")
        deployment.stop()
        deployment.start()
        for user, nonce, forbidden in (("alice", nonce_a, nonce_b), ("bob", nonce_b, nonce_a)):
            _, reply = deployment.event(f"{user}-after-restart", user,
                "继续上一轮：先根据会话记忆说出我的口令，再用 anchor_run 读取 /previous/note.txt 核对。"
                "用 anchor_conversation_history 查看上轮真实工具记录。复制 note.txt 到本轮 /workspace 并另写包含口令与核对结果的 verified.txt，"
                "最后回复原口令和核对结果。不要调用企业微信外部发送。", args.timeout)
            require(nonce in reply["text"] and forbidden not in reply["text"], "Restart lost or mixed conversation history")
            deployment.check_followup_trace(reply, nonce, forbidden)
            for filename in ("note.txt", "verified.txt"):
                status, data = deployment.api.raw("GET", f"/runs/{reply['run']}/files/assistant/{filename}?download=1")
                require(status == 200 and nonce in data.decode() and forbidden not in data.decode(),
                        "Follow-up artifact was not inherited from the correct previous Run")
            report["runs"].append(reply["run"])
        require(not list((deployment.paths["platform"] / "workspaces").glob("*/runs/*/run.json")),
                "Python created a second Graph Run")
        native = list((deployment.paths["state"] / "io-harness").rglob("*.sqlite3"))
        require(bool(native), "Native framework history is missing")
        report.update({"status": "passed", "restart": True, "duplicate_no_new_run": True,
                       "two_users": True, "graph_unchanged": True, "native_databases": len(native)})
    finally:
        deployment.stop()
        evidence.save("evidence.json", report)
        print(root / "evidence.json", flush=True)


if __name__ == "__main__":
    main()
