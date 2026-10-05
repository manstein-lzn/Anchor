"""Controlled provider acceptance for channel supersession and partial files.

Only the model is a fixture. Both HTTP services, native Session, cancellation,
sandbox tools, files and Turn/Run persistence are real. No production gateway.
"""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import tempfile
import threading
from uuid import uuid4

from rust_channel_smoke import Deployment, GRAPH, ROOT
from rust_platform_plugin_smoke import Evidence, SmokeFailure, require, wait_until


class Provider(BaseHTTPRequestHandler):
    entered = threading.Event()
    release = threading.Event()
    calls: list[dict] = []
    failures: list[str] = []
    crash_mode = False
    expect_unknown = False

    def log_message(self, *_args):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        try:
            messages = request["messages"]
            # Native Session seeds historical operator messages after the
            # bounded Goal block. Read the current Goal, never an old prompt.
            mode = None
            for message in messages:
                if message.get("role") != "user":
                    continue
                content = message.get("content")
                if isinstance(content, list):
                    content = "\n".join(item.get("text", "") for item in content)
                if not isinstance(content, str) or not content.startswith("Goal:") or "Input:\n" not in content:
                    continue
                try:
                    value, _ = json.JSONDecoder().raw_decode(content.rsplit("Input:\n", 1)[1].lstrip())
                    mode = value["input"]["message"]
                except (KeyError, ValueError):
                    continue
            require(mode in {"fixture-A", "fixture-B", "fixture-C", "fixture-D"}, "Unknown fixture prompt")
            results = [message for message in messages if message.get("role") == "tool"]
            tools = [call["function"]["name"] for message in messages
                     for call in message.get("tool_calls", [])]
            if mode == "fixture-A" and not results:
                command = "printf unfinished-A > draft.txt" + ("; sleep 90" if self.crash_mode else "")
                name, arguments = "anchor_run", {"command": ["sh", "-c", command]}
            elif mode == "fixture-A":
                self.entered.set()
                require(self.release.wait(45), "Supersession gate was not released")
                name, arguments = "final_result", {"summary": "old reply must be suppressed", "route": None}
            elif mode == "fixture-B":
                raise AssertionError("Superseded queued message reached the provider")
            elif mode == "fixture-C" and "anchor_conversation_history" not in tools:
                name, arguments = "anchor_conversation_history", {}
            elif mode == "fixture-C" and "anchor_run" not in tools:
                observed = json.dumps(results)
                require("anchor_run" in observed and
                        (("unknown" in observed.lower()) if self.expect_unknown else "draft.txt" in observed),
                        "Prior tool facts are absent")
                name, arguments = "anchor_run", {"command": ["sh", "-c",
                    "set -eu; test \"$(cat /previous/draft.txt)\" = unfinished-A; "
                    "if printf forbidden >> /previous/draft.txt 2>/dev/null; then exit 88; fi; "
                    "cp /previous/draft.txt recovered.txt; printf '\ncontinued-C\n' >> recovered.txt"]}
            else:
                if mode == "fixture-C":
                    observed = json.dumps(results)
                    require('exit_code' in observed and ('\\\"exit_code\\\":0' in observed or
                            '\\\"exit_code\\\": 0' in observed or '"exit_code": 0' in observed),
                            "Read-only handoff command did not succeed")
                name, arguments = "final_result", {"summary": f"{mode} completed", "route": None}
            self.calls.append({"mode": mode, "tool": name})
            payload = json.dumps({"id": f"chatcmpl-{uuid4()}", "object": "chat.completion", "created": 1,
                "model": request["model"], "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {
                    "role": "assistant", "content": None, "tool_calls": [{"id": f"call-{uuid4()}",
                        "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}]}}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}).encode()
            status = 200
        except (AssertionError, KeyError, ValueError, SmokeFailure) as error:
            self.failures.append(str(error))
            payload = json.dumps({"error": {"message": str(error), "type": "fixture_error"}}).encode()
            status = 400
        self.send_response(status)
        if status == 200 and request.get("stream"):
            response = json.loads(payload)
            choice = response["choices"][0]
            call = choice["message"]["tool_calls"][0]
            delta = {"role": "assistant", "tool_calls": [{"index": 0, **call}]}
            chunks = [{"id": response["id"], "object": "chat.completion.chunk", "created": 1,
                       "model": request["model"], "choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                      {"id": response["id"], "object": "chat.completion.chunk", "created": 1,
                       "model": request["model"], "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}],
                       "usage": response["usage"]}]
            payload = ("".join(f"data: {json.dumps(chunk)}\n\n" for chunk in chunks) + "data: [DONE]\n\n").encode()
            content_type = "text/event-stream"
        else:
            content_type = "application/json"
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        try:
            self.wfile.write(payload)
        except (BrokenPipeError, ConnectionResetError):
            pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "rust/target/debug/anchor-runner-host")
    args = parser.parse_args()
    root = Path(tempfile.mkdtemp(prefix="rust-channel-control-", dir=ROOT / ".local"))
    evidence = Evidence(root, ())
    provider = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    thread = threading.Thread(target=provider.serve_forever, daemon=True)
    thread.start()
    os.environ.update({"ANCHOR_MODEL_API_KEY": "fixture-not-a-secret", "ANCHOR_MODEL_NAME": "fixture-channel",
        "ANCHOR_MODEL_URL": f"http://127.0.0.1:{provider.server_port}/v1", "ANCHOR_MODEL_WIRE_API": "chat",
        "ANCHOR_MODEL_ALIASES": "{}"})
    deployment = Deployment(evidence, args.binary.resolve())
    report = {"status": "failed", "provider": "controlled fixture; not live model"}
    pool = ThreadPoolExecutor(max_workers=2)
    try:
        deployment.start()
        graph = json.loads((ROOT / "examples/graphs/wecom-assistant.json").read_text())
        deployment.api.expect("POST", "/graphs", {"name": GRAPH, "definition": graph}, status=201)
        first = pool.submit(deployment.event, "A", "alice", "fixture-A", 90)
        require(Provider.entered.wait(30), "First message did not leave partial work")
        # A is still in a real provider call. Another user can finish concurrently.
        _, independent = deployment.event("D", "bob", "fixture-D", 30)
        deployment.check_run(independent)
        sessions = deployment.api.expect("GET", "/channel-sessions")["sessions"]
        session = next(item["id"] for item in sessions
                       if deployment.api.expect("GET", f"/sessions/{item['id']}")["session"]
                       .get("channel", {}).get("sender_id") == "alice")
        path = f"/sessions/{session}/turns"
        middle = deployment.api.expect("POST", path, {"request_id": "queued-B", "message": "fixture-B"}, status=202)["turn"]
        latest = deployment.api.expect("POST", path, {"request_id": "latest-C", "message": "fixture-C"}, status=202)["turn"]
        _, old = first.result(timeout=10)
        require(old.get("superseded") and old["text"] == "", "Old reply escaped suppression")
        Provider.release.set()

        def settled():
            turns = deployment.api.expect("GET", path)["turns"]
            current = next(item for item in turns if item["id"] == latest["id"])
            return current if current["status"] != "running" else None

        final = wait_until(settled, 60, "latest Turn completion")
        evidence.save("turns.json", deployment.api.expect("GET", path))
        require(final["status"] == "completed", "Latest message did not complete")
        identifier = f"channel-{latest['id']}"
        runs = deployment.rust.expect("GET", "/runs")["runs"]
        require(f"channel-{middle['id']}" not in {item["run"] for item in runs}, "Queued superseded Turn created a Run")
        require(len(runs) == 3, "Unexpected Run count")
        status, data = deployment.api.raw("GET", f"/runs/{identifier}/files/assistant/recovered.txt?download=1")
        require(status == 200 and data.decode() == "unfinished-A\ncontinued-C\n", "Partial file handoff failed")
        previous = deployment.rust.expect("GET", f"/runs/{old['run']}")
        require(previous["state"]["status"] == "stopped" and not previous["active"], "Predecessor did not stop")
        require(not Provider.failures, "Provider fixture rejected an execution contract")
        # A second interruption kills the actual writing host during a real
        # tool, preserving the native unknown attempt without editing facts.
        Provider.crash_mode = True
        prior_runs = {item["run"] for item in runs}
        crashed = pool.submit(deployment.event, "crashed-A", "alice", "fixture-A", 90)

        def partial_tool():
            current = deployment.rust.expect("GET", "/runs")["runs"]
            for item in current:
                if item["run"] not in prior_runs:
                    if list((deployment.paths["work"] / item["run"]).glob("*/draft.txt")):
                        return item["run"]
            return None

        interrupted = wait_until(partial_tool, 30, "tool to persist partial work before host kill")
        before_calls = sum(item["mode"] == "fixture-A" and item["tool"] == "anchor_run" for item in Provider.calls)
        deployment.services[0].process.kill()
        deployment.services[0].process.wait(timeout=10)
        try:
            crashed.result(timeout=15)
        except (OSError, SmokeFailure):
            pass  # The normalized event records the truthful interrupted error.
        deployment.stop()
        Provider.crash_mode = False
        Provider.expect_unknown = True
        deployment.start()
        _, recovered = deployment.event("after-crash-C", "alice", "fixture-C", 60)
        deployment.check_run(recovered)
        status, data = deployment.api.raw("GET", f"/runs/{recovered['run']}/files/assistant/recovered.txt?download=1")
        require(status == 200 and data.decode() == "unfinished-A\ncontinued-C\n", "Crash lost partial files")
        require(sum(item["mode"] == "fixture-A" and item["tool"] == "anchor_run" for item in Provider.calls)
                == before_calls, "Crash replayed the old tool")
        interrupted_detail = deployment.rust.expect("GET", f"/runs/{interrupted}")
        require(interrupted_detail["state"]["status"] == "stopped" and not interrupted_detail["active"],
                "Crash predecessor did not settle")
        evidence.save("crash-predecessor.json", interrupted_detail)
        require(not Provider.failures, "Provider fixture rejected crash recovery")
        final_runs = deployment.rust.expect("GET", "/runs")["runs"]
        require(len(final_runs) == 5, "Unexpected Run count after crash recovery")
        report.update(status="passed", runs=len(final_runs), runs_before_crash=len(runs), superseded_queued_skipped=True,
            other_user_concurrent=True, unfinished_previous_readonly=True, prior_tool_facts_readable=True,
            actual_host_killed_during_tool=True, crash_tool_not_replayed=True)
    finally:
        Provider.release.set()
        deployment.stop()
        provider.shutdown()
        provider.server_close()
        pool.shutdown(wait=True)
        evidence.save("provider.json", {"calls": Provider.calls, "failures": Provider.failures})
        evidence.save("evidence.json", report)
        print(root / "evidence.json", flush=True)


if __name__ == "__main__":
    main()
