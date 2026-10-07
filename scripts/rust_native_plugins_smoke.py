"""Opt-in, bounded real-model acceptance of two native Plugins against loopback APIs."""
from __future__ import annotations

import argparse
from collections import Counter
from email import policy
from email.parser import BytesParser
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import math
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import time
from urllib.parse import parse_qs, urlsplit
import uuid

from rust_low_cost_regression import (
    MODEL_OPTIONS, check_file, check_histories, collect_usage, file_sha256, host_environment, wait_run,
)
from rust_platform_plugin_smoke import (
    Api, Evidence, MODEL_KEYS, Service, SmokeFailure, read_json, require, unused_ports, wait_until,
)

CASE = "native-plugins"
IMAGE = b"fixture-image"
SKILLS = {name: f"/plugins/{name}/skills/{name}/SKILL.md" for name in ("wecom", "docmost")}
TOOLS = {"wecom": "wecom-wecom_wecom_get_user", "docmost": "docmost-attachments_upload_page_image"}
READ = "set -eu\ncat " + " ".join(SKILLS.values()) + "\nprintf fixture-image | cmp - /in/publish/assets/panel.png\n" + (
    "if (: >> /in/publish/assets/panel.png) 2>/dev/null; then exit 1; fi\nprintf 'input-readonly\\n'"
)


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def send_error(self, code, message=None, explain=None):
        self.server.record({"kind": "rejected", "status": code, "method": self.command})
        super().send_error(code, "fixture rejected request")

    def do_GET(self):
        self.connection.settimeout(5)
        route = urlsplit(self.path)
        query = parse_qs(route.query, keep_blank_values=True)
        fixture = self.server
        kind, status = "rejected", 400
        try:
            require(self.client_address[0] == "127.0.0.1", "non-loopback client")
            if self.command == "GET" and route.path == "/cgi-bin/gettoken":
                require(query == {"corpid": ["fixture-corp"], "corpsecret": ["fixture-secret"]}, "token parameters")
                kind, result = "token", {"errcode": 0, "access_token": "fixture-token", "expires_in": 7200}
            elif self.command == "GET" and route.path == "/cgi-bin/user/get":
                require(query == {"access_token": ["fixture-token"], "userid": [fixture.userid]}, "member parameters")
                kind, result = "member", fixture.expected["wecom"]
            elif self.command == "POST" and route.path == "/api/files/upload":
                require(not query and self.headers.get("Authorization") == "Bearer fixture-docmost-key", "upload auth")
                size = int(self.headers.get("Content-Length", "0"))
                require(0 < size <= 65536, "upload size")
                body = self.rfile.read(size)
                require(len(body) == size, "truncated upload")
                message = BytesParser(policy=policy.default).parsebytes(
                    b"Content-Type: " + self.headers.get("Content-Type", "").encode() + b"\r\n\r\n" + body)
                parts = list(message.iter_parts())
                fields = {part.get_param("name", header="content-disposition"): part for part in parts}
                require(message.get_content_type() == "multipart/form-data" and len(parts) == 2
                        and set(fields) == {"pageId", "file"}, "multipart fields")
                require(fields["pageId"].get_payload(decode=True) == fixture.page.encode(), "pageId")
                require(fields["file"].get_filename() == "panel.png"
                        and fields["file"].get_content_type() == "image/png"
                        and fields["file"].get_payload(decode=True) == IMAGE, "image bytes or MIME")
                kind = "upload"
                result = {"id": fixture.expected["docmost"]["attachmentId"], "fileName": "panel.png",
                          "mimeType": "image/png", "pageId": fixture.page}
            else:
                status = 404
                raise SmokeFailure("route not permitted")
            status = 200
        except (SmokeFailure, ValueError, OSError, KeyError) as error:
            result = {"error": str(error)}
        fixture.record({"kind": kind, "status": status, "method": self.command, "path": route.path})
        raw = json.dumps(result).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    do_POST = do_GET


class Fixture(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, evidence):
        super().__init__(("127.0.0.1", 0), Handler)
        self.evidence, self.calls, self.lock = evidence, [], threading.Lock()
        self.endpoint = f"http://127.0.0.1:{self.server_port}"
        self.userid, self.page, attachment = "member-" + uuid.uuid4().hex, str(uuid.uuid4()), str(uuid.uuid4())
        self.expected = {"wecom": {"errcode": 0, "userid": self.userid, "name": "Native-" + uuid.uuid4().hex},
                         "docmost": {"attachmentId": attachment, "fileName": "panel.png",
                                     "url": f"/api/files/{attachment}/panel.png", "mimeType": "image/png", "pageId": self.page}}
        self.thread = threading.Thread(target=self.serve_forever, daemon=True)
        self.thread.start()

    def record(self, call):
        with self.lock:
            self.calls.append(call)
            self.evidence.save("business-calls.json", self.calls)

    def stop(self):
        self.shutdown()
        self.server_close()
        self.thread.join(timeout=5)


def binding(plugin: Path) -> dict:
    require(not any(path.is_symlink() for path in plugin.rglob("*")), "Plugin contains symlinks")
    resources = sorted(path.relative_to(plugin).as_posix() for path in plugin.rglob("*") if path.is_file())
    digest = hashlib.sha256()
    for resource in resources:
        digest.update(resource.encode())
        digest.update(bytes.fromhex(file_sha256(plugin / resource)))
    return {"id": plugin.name, "digest": digest.hexdigest(), "resources": resources,
            "mcp_servers": sorted(read_json(plugin / "plugin.json")["mcpServers"])}


def prepare(root: Path, binaries: dict, fixture: Fixture, evidence: Evidence) -> dict:
    plugins = root / "bundle/plugins"
    plugins.mkdir(parents=True)
    for name in SKILLS:
        try:
            output = subprocess.run([str(binaries[name]), "package-plugin", str(plugins / name)], env={},
                                    capture_output=True, timeout=30, check=False)
        except subprocess.TimeoutExpired as error:
            evidence.save(f"package-{name}.json", {"timed_out": True,
                          "stdout": (error.stdout or b"").decode(errors="replace"),
                          "stderr": (error.stderr or b"").decode(errors="replace")})
            raise
        evidence.save(f"package-{name}.json", {"exit_code": output.returncode,
                      "stdout": output.stdout.decode(errors="replace"), "stderr": output.stderr.decode(errors="replace")})
        require(output.returncode == 0, f"{name} native packaging failed")
        plugin = plugins / name
        require(set(binding(plugin)["resources"]) == {"plugin.json", f"skills/{name}/SKILL.md", f"bin/anchor-{name}-tools"},
                f"{name} package contains non-native payload")
        require(file_sha256(plugin / f"bin/anchor-{name}-tools") == file_sha256(binaries[name]), "Packaged binary differs")
        manifest = read_json(plugin / "plugin.json")
        if name == "wecom":
            manifest["mcpServers"]["wecom"].pop("optional_env_vars", None)
            manifest["mcpServers"]["wecom"]["env"] = {
                "WECOM_CORP_ID": "fixture-corp", "WECOM_AGENT_ID": "1", "WECOM_SECRET": "fixture-secret",
                "WECOM_API_BASE_URL": fixture.endpoint}
        else:
            server = manifest["mcpServers"]["attachments"]
            server.pop("env_vars", None)
            server.update(args=["--endpoint", fixture.endpoint + "/api/files/upload"], env={"DOCMOST_API_KEY": "fixture-docmost-key"})
            manifest["mcpServers"] = {"attachments": server}
        (plugin / "plugin.json").write_text(json.dumps(manifest), encoding="utf-8")
    instructions = (
        f"This is a local-only native Plugin acceptance. First anchor_run command=['sh','-c',{READ!r}]. "
        f"Then call {TOOLS['wecom']} exactly once with userid={fixture.userid!r}, and {TOOLS['docmost']} exactly once "
        f"with path='/in/publish/assets/panel.png', pageId={fixture.page!r}. Do not supply attachmentId. "
        "No other MCP calls, HTTP, messages or publishing; never retry a business call. "
        "Write report.json as exactly {'wecom': <entire returned member JSON>, 'docmost': <entire returned upload JSON>}, "
        "using actual tool outputs, not guesses, with JSON double quotes. Using anchor_run, first test that report.json "
        "and effects.txt do not exist, write them once, effects.txt bytes exactly 'once\\n', and cat both. "
        "Then submit final_result alone with summary='native Plugins verified', without a route.")
    graph = {"objective": "Verify native Plugins with loopback business fixtures", "entry": "publish",
             "agents": {"worker": {"model": "models.regression", "network": True, "wall_time_limit_seconds": 60,
                                   "instructions": instructions}},
             "ops": {"publish": {"run": "sh -c 'set -eu; mkdir -p assets; printf fixture-image > assets/panel.png'"}},
             "nodes": [{"id": "publish", "op": "publish"}, {"id": "worker", "agent": "worker", "plugins": list(SKILLS)}],
             "edges": [{"from": "publish", "to": "worker"}]}
    (root / "bundle/graph.json").write_text(json.dumps(graph), encoding="utf-8")
    (root / "bundle/manifest.json").write_text(json.dumps({"format": 1, "graph": "graph.json",
                 "plugins": [binding(plugins / name) for name in SKILLS]}), encoding="utf-8")
    return graph


def tool_results(history, name):
    prefix = f"[{name}]\n"
    return [json.loads(message["text"].strip()[len(prefix):]) for message in history
            if message["role"] == "tool" and message.get("text", "").strip().startswith(prefix)]


def verify(root, graph, detail, record, fixture, original_input):
    require(record["status"] == "completed" and not detail["active"], "Graph incomplete")
    require(record["invocations"] == {"publish": 1, "worker": 1}, "Invocation counts differ")
    require(record["input"] == original_input == detail["state"]["input"], "Original input changed")
    artifacts = root / "state/artifacts" / record["results"]["worker"][0]["commit"]["id"] / "files"
    require(read_json(artifacts / "report.json") == fixture.expected, "Report differs from actual fixture responses")
    files = [check_file(root, record, "publish", "assets/panel.png", IMAGE),
             check_file(root, record, "worker", "report.json", (artifacts / "report.json").read_bytes()),
             check_file(root, record, "worker", "effects.txt", b"once\n")]
    histories = check_histories(root, graph, record, detail)
    history = detail["traces"]['["worker",1]']
    commands = [command for message in history for command in message.get("commands", [])]
    require(any(command.startswith("anchor_run ") and all(path in command for path in SKILLS.values())
                for command in commands), "Missing combined actual Skill read")
    skills = [(root / "bundle" / path.removeprefix("/")).read_text() for path in SKILLS.values()]
    require(any(result.get("exit_code") == 0 and "input-readonly\n" in result.get("stdout", "")
                and all(skill in result.get("stdout", "") for skill in skills)
                for result in tool_results(history, "anchor_run")), "Missing Skill bytes or read-only input evidence")
    for name, tool in TOOLS.items():
        arguments = {"userid": fixture.userid} if name == "wecom" else {"path": "/in/publish/assets/panel.png", "pageId": fixture.page}
        calls = [json.loads(command[len(tool) + 1:]) for command in commands if command.startswith(tool + " ")]
        require(calls == [arguments], f"{tool} was not called exactly once with approved arguments")
        require(tool_results(history, tool) == [fixture.expected[name]], f"Missing actual {tool} result")
    require(all(call["status"] == 200 for call in fixture.calls), "Fixture HTTP errors; inspect business-calls.json")
    require(Counter(call["kind"] for call in fixture.calls) == {"token": 1, "member": 1, "upload": 1}, "Business calls differ")
    return {"files": files, "histories": histories, "invocations": record["invocations"]}


def run(args, evidence):
    root, report, service, fixture = evidence.root, {"status": "failed", "binary_hashes": {}}, None, None
    started = time.monotonic()
    try:
        binaries = {}
        for name, source in {"host": args.binary, "wecom": args.wecom_binary, "docmost": args.docmost_binary}.items():
            source = source.expanduser().resolve(strict=True)
            require(source.is_file() and os.access(source, os.X_OK), f"Missing executable {name} binary")
            digest = file_sha256(source)
            binaries[name] = root / f"anchor-{name}"
            shutil.copy2(source, binaries[name])
            require(file_sha256(binaries[name]) == digest == file_sha256(source), f"{name} binary changed during copy")
            report["binary_hashes"][name] = digest
        fixture = Fixture(evidence)
        graph = prepare(root, binaries, fixture, evidence)
        frozen = {path.relative_to(root).as_posix(): file_sha256(path) for path in (root / "bundle").rglob("*") if path.is_file()}
        evidence.save("frozen-hashes.json", frozen)
        report.update(graph_sha256=frozen["bundle/graph.json"], manifest_sha256=frozen["bundle/manifest.json"])
        port, _unused = unused_ports()
        env = host_environment(root, CASE, port)
        env["ANCHOR_RUNNER_ALLOWED_COMMANDS"] += ",mkdir"
        service = Service("host", [str(binaries["host"]), "serve"], env, evidence)
        api = Api(port, timeout=30)
        wait_until(lambda: api.ready(service, "/health"), 30, "isolated Rust Host")
        original_input = {"acceptance": uuid.uuid4().hex}
        status, raw = api.raw("POST", "/trigger", {"graph": CASE, "input": original_input})
        evidence.save("trigger-response.json", {"status": status, "body": raw.decode(errors="replace")})
        require(status == 202, f"Graph admission failed: HTTP {status}")
        report["run"] = run_id = json.loads(raw)["run"]
        detail = wait_run(api, service, run_id, evidence, args.timeout)
        record = read_json(root / "state/runs" / f"{run_id}.json")
        evidence.save("run-record.json", record)
        report.update(verify(root, graph, detail, record, fixture, original_input))
        require(frozen == {path.relative_to(root).as_posix(): file_sha256(path) for path in (root / "bundle").rglob("*") if path.is_file()}, "Frozen bundle changed")
        require(all(file_sha256(binaries[name]) == digest for name, digest in report["binary_hashes"].items()), "Fixed binary changed")
        report["status"] = "passed"
    except (SmokeFailure, OSError, ValueError, KeyError, TypeError, IndexError, subprocess.TimeoutExpired) as error:
        report["failure"] = evidence.redact(f"{type(error).__name__}: {error}")
    finally:
        for resource in (service, fixture):
            if resource is not None:
                try:
                    resource.stop()
                except (OSError, SmokeFailure, subprocess.TimeoutExpired) as error:
                    report.update(status="failed", cleanup_failure=evidence.redact(str(error)))
        if fixture is not None:
            report["business_calls"] = fixture.calls
        try:
            report.update(collect_usage(root / "state"))
            if report["status"] == "passed":
                require(report["provider_attempts"] > 0, "Missing native provider recordings")
        except (SmokeFailure, OSError, ValueError, KeyError, TypeError, IndexError) as error:
            report.update(status="failed", usage_failure=evidence.redact(str(error)))
        report["elapsed_seconds"] = round(time.monotonic() - started, 3)
        evidence.save("evidence.json", report)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "wecom-binary", "docmost-binary"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--evidence-root", type=Path)
    parser.add_argument("--timeout", type=float, default=120)
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or not 0 < args.timeout <= 180:
        parser.error("--timeout must be finite, positive and at most 180 seconds")
    if any(not os.environ.get(name, "").strip() for name in MODEL_KEYS):
        parser.exit(2, "Missing ANCHOR_MODEL_API_KEY/URL/NAME; no model request made; NOT passed\n")
    parent = args.evidence_root.expanduser().resolve() if args.evidence_root else None
    if parent:
        parent.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="anchor-native-plugins-", dir=parent))
    root.chmod(0o700)
    secrets = tuple(os.environ.get(name, "") for name in (*MODEL_KEYS[:2], *MODEL_OPTIONS)
                    if name not in {"ANCHOR_MODEL_WIRE_API", "ANCHOR_MODEL_CONTEXT_WINDOW"})
    report = run(args, Evidence(root, secrets))
    print(json.dumps({"status": report["status"], "evidence": str(root / "evidence.json"),
                      "reported_tokens": report.get("reported_tokens"), "failure": report.get("failure", report.get("usage_failure"))}))
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
