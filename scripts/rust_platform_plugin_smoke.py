"""Opt-in real-provider acceptance through the original Python HTTP platform.

Start the Python platform with its Rust HTTP backend, create the unchanged
academic Graph through POST /graphs, and execute the existing scholarly tool.
Disposable service roots and evidence remain under .local/rust-platform-plugin-*.
This does not publish research or send business messages.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import sqlite3
import subprocess
import tempfile
import threading
import time
import unicodedata
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import ProxyHandler, Request, build_opener


ROOT = Path(__file__).resolve().parents[1]
MODEL_KEYS = ("ANCHOR_MODEL_API_KEY", "ANCHOR_MODEL_URL", "ANCHOR_MODEL_NAME")
SKILL = "skills/academic-research/SKILL.md"
GRAPH_NAME = "plugin-research"
TASK = (
    "本轮仅验收已有学术 Plugin 的真实检索接线，不做完整调研。"
    "先用 anchor_run 读取 /plugins/academic-research/skills/academic-research/SKILL.md。"
    "然后用 anchor_run 执行 sh -c '/tools/scholarly/run search "
    '--query "retrieval augmented generation" --source crossref --limit 1 > /workspace/sources.json' "'"
    "，必须保留工具原始 JSON 输出，不要编造或改写 sources.json。"
    "读取 sources.json，写简短中文 research.md，包含实际标题、DOI 和读取范围，"
    "明确这里只查询了元数据/摘要，没有阅读论文全文。随后完成。"
)


class SmokeFailure(RuntimeError):
    """A failed acceptance check whose message contains no provider details."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SmokeFailure(message)


def read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


class Evidence:
    def __init__(self, root: Path, secrets: tuple[str, ...]):
        self.root = root
        self.secrets = tuple(sorted({part for value in secrets if value for part in (
            value, json.dumps(value, ensure_ascii=False)[1:-1],
        )}, key=len, reverse=True))

    def redact(self, text: str) -> str:
        for value in self.secrets:
            text = text.replace(value, "[redacted]")
        return text

    def save(self, name: str, value) -> None:
        (self.root / name).write_text(self.redact(json.dumps(
            value, indent=2, ensure_ascii=False,
        )) + "\n", encoding="utf-8")

    def drain(self, stream, name: str) -> None:
        with stream, (self.root / name).open("w", encoding="utf-8") as log:
            for line in iter(stream.readline, b""):
                log.write(self.redact(line.decode("utf-8", errors="replace")))
                log.flush()


class Service:
    def __init__(self, label: str, command: list[str], env: dict[str, str], evidence: Evidence):
        self.label = label
        self.process = subprocess.Popen(
            command, env=env, cwd=evidence.root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        )
        self.log = threading.Thread(
            target=evidence.drain, args=(self.process.stdout, f"{label}.log"), daemon=True,
        )
        self.log.start()

    def check(self) -> None:
        require(self.process.poll() is None, f"{self.label} service exited; inspect its retained log")

    def stop(self) -> None:
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        self.log.join(timeout=5)


class Api:
    def __init__(self, port: int, timeout: float = 5):
        self.base = f"http://127.0.0.1:{port}"
        self.opener = build_opener(ProxyHandler({}))
        self.timeout = timeout

    def raw(self, method: str, path: str, body=None) -> tuple[int, bytes]:
        request = Request(
            self.base + path, method=method,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
        )
        try:
            with self.opener.open(request, timeout=self.timeout) as response:
                return response.status, response.read()
        except HTTPError as error:
            with error:
                return error.code, error.read()

    def expect(self, method: str, path: str, body=None, status: int = 200):
        actual, raw = self.raw(method, path, body)
        require(actual == status, f"{method} {path}: expected HTTP {status}, got {actual}")
        return json.loads(raw)

    def ready(self, service: Service, path: str) -> bool:
        service.check()
        try:
            return self.raw("GET", path)[0] == 200
        except (URLError, ConnectionError, TimeoutError):
            return False


def wait_until(check, timeout: float, what: str):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.2)
    raise SmokeFailure(f"Timed out waiting for {what}")


def unused_ports() -> tuple[int, int]:
    with socket.socket() as rust, socket.socket() as python:
        rust.bind(("127.0.0.1", 0))
        python.bind(("127.0.0.1", 0))
        return rust.getsockname()[1], python.getsockname()[1]


def tree_hashes(root: Path) -> dict[str, str]:
    return {path.relative_to(root).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in sorted(root.rglob("*")) if path.is_file()}


def prepare(proof: Path, python: Path, source_root: Path) -> dict[str, Path]:
    paths = {name: proof / name for name in ("platform", "bootstrap", "catalog", "state", "work")}
    library = paths["platform"] / "library"
    shutil.copytree(ROOT / "plugins/academic-research", library / "plugins/academic-research")
    tool = library / "tools/scholarly"
    tool.mkdir(parents=True)
    (tool / "tool.json").write_text(json.dumps({
        "entrypoint": str(python.parent / "anchor-scholarly"),
        "environment": str(python.parent.parent), "imports": [str(source_root)],
    }) + "\n", encoding="utf-8")
    paths["bootstrap"].mkdir()
    paths["catalog"].mkdir()
    (paths["bootstrap"] / "graph.json").write_text(json.dumps({
        "entry": "start", "objective": "Untriggered service bootstrap", "agents": {},
        "ops": {"start": {"run": "true"}}, "nodes": [{"id": "start", "op": "start"}], "edges": [],
    }) + "\n", encoding="utf-8")
    (paths["bootstrap"] / "manifest.json").write_text(json.dumps({
        "format": 1, "graph": "graph.json", "plugins": [],
    }) + "\n", encoding="utf-8")
    (proof / "runtime.json").write_text("{}\n", encoding="utf-8")
    paths["library"] = library
    return paths


def service_envs(paths: dict[str, Path], source_root: Path, rust_port: int) -> tuple[dict, dict]:
    base = {"PATH": "/usr/bin:/bin", "PYTHONDONTWRITEBYTECODE": "1", "PYTHONUNBUFFERED": "1"}
    rust = {**base, **{key: os.environ[key] for key in MODEL_KEYS}}
    for name in ("ANCHOR_MODEL_WIRE_API", "ANCHOR_MODEL_ALIASES", "ANCHOR_MODEL_CONTEXT_WINDOW", "ANCHOR_MODEL_IMAGE_MODELS",
                 "SSL_CERT_FILE", "SSL_CERT_DIR", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"):
        if os.environ.get(name):
            rust[name] = os.environ[name]
    rust.update({
        "ANCHOR_MODEL_WIRE_API": rust.get("ANCHOR_MODEL_WIRE_API", "responses"),
        "ANCHOR_RUNNER_BUNDLE_ROOT": str(paths["bootstrap"]),
        "ANCHOR_RUNNER_STATE_ROOT": str(paths["state"]),
        "ANCHOR_RUNNER_WORKSPACE_ROOT": str(paths["work"]),
        "ANCHOR_RUNNER_CATALOG_ROOT": str(paths["catalog"]),
        "ANCHOR_RUNNER_LIBRARY_ROOT": str(paths["library"]),
        "ANCHOR_RUNNER_GRAPH_NAME": "bootstrap", "ANCHOR_RUNNER_LISTEN": f"127.0.0.1:{rust_port}",
        "ANCHOR_RUNNER_ALLOWED_COMMANDS": "sh,cat,find,printf",
    })
    platform = {**base, "PYTHONPATH": str(source_root), "ANCHOR_RUNTIME_BACKEND": "rust",
                "ANCHOR_API_KEYS": "", "ANCHOR_API_KEY": "",
                "ANCHOR_MODEL_URL": "", "ANCHOR_MODEL_API_KEY": "", "ANCHOR_MODEL_NAME": "",
                "ANCHOR_RUNTIME_URL": f"http://127.0.0.1:{rust_port}"}
    return rust, platform


def wait_run(api: Api, run_id: str, services: list[Service], evidence: Evidence, timeout: float) -> dict:
    def completed():
        for service in services:
            service.check()
        detail = api.expect("GET", f"/runs/{quote(run_id, safe='')}")
        evidence.save("platform-run.json", detail)
        require(detail.get("backend") == "rust", "The platform did not expose a Rust-owned Run")
        status = detail["state"]["status"]
        require(status in {"ready", "running", "completed"}, f"Run did not complete: {status}")
        return detail if status == "completed" and not detail["active"] else None

    return wait_until(completed, timeout, "the academic Plugin Run to complete")


def verify_artifacts(api: Api, paths: dict[str, Path], run_id: str, detail: dict, evidence: Evidence) -> tuple:
    record = read_json(paths["state"] / "runs" / f"{run_id}.json")
    require(record["status"] == "completed", "Rust did not persist a completed Run")
    metadata = read_json(paths["state"] / "run-metadata" / f"{run_id}.json")
    require(metadata["graph"] == GRAPH_NAME, "Rust Run metadata has the wrong Graph identity")
    require(metadata["trigger_source"] == "manual", "Unexpected trigger source")
    results = record["results"]["research"]
    require(len(results) == 1, "Expected one research invocation")
    result = results[0]
    artifacts = paths["state"] / "artifacts" / result["commit"]["id"] / "files"
    files = api.expect("GET", f"/runs/{run_id}/files/research")
    evidence.save("platform-files.json", files)
    require({"sources.json", "research.md"} <= {item["path"] for item in files["files"]},
            "The platform is missing committed research artifacts")
    for name in ("sources.json", "research.md"):
        status, raw = api.raw("GET", f"/runs/{run_id}/files/research/{name}?download=1")
        require(status == 200 and raw == (artifacts / name).read_bytes(),
                f"The platform download differs from the authoritative Rust artifact: {name}")
    raw_sources = (artifacts / "sources.json").read_text(encoding="utf-8")
    sources = json.loads(raw_sources)
    require(sources.get("source") == "crossref" and bool(sources.get("papers")),
            "Missing real Crossref results")
    paper = sources["papers"][0]
    doi = paper.get("doi")
    research = (artifacts / "research.md").read_text(encoding="utf-8")
    require(isinstance(doi, str) and bool(doi) and doi in research, "research.md lacks the actual DOI")
    # Markdown may use an ASCII hyphen for the source's typographic hyphen.
    # Preserve all words and still compare the raw scholarly JSON separately.
    def title_text(text: str) -> str:
        return unicodedata.normalize("NFKC", text).replace("\u2010", "-").replace("\u2011", "-")

    require(isinstance(paper.get("title"), str) and title_text(paper["title"]) in title_text(research),
            "research.md lacks the actual paper title")
    require(detail["state"]["input"] == {"task": TASK}, "The platform changed the original task input")
    return result, artifacts, raw_sources, doi


def verify_harness(state: Path, result: dict, raw_sources: str, skill: str) -> tuple[dict, int]:
    key = result["key"]
    durable_key = ":".join(str(key[name]) for name in ("run_id", "graph_digest", "node_id", "invocation"))
    stem = "np1-" + hashlib.sha256(durable_key.encode()).hexdigest()
    store_root = state / "io-harness/store"
    stores = sorted(store_root.glob("*.sqlite3"))
    require(stores == [store_root / f"{stem}.sqlite3"], "Unexpected Harness invocation stores")
    with sqlite3.connect(f"file:{stores[0]}?mode=ro", uri=True) as store:
        calls = [(step, call) for step, serialized in store.execute("SELECT step, calls FROM step_turns ORDER BY step")
                 for call in json.loads(serialized) if call["name"] == "anchor_run"]
        providers = store.execute("SELECT model FROM provider_calls ORDER BY id").fetchall()
        final = store.execute("SELECT text FROM step_turns ORDER BY step DESC LIMIT 1").fetchone()
        observations = [(step, json.loads(text.split("[anchor_run]", 1)[1])) for step, text in store.execute(
            "SELECT step, text FROM ledger_observations WHERE target='anchor_run' ORDER BY id",
        )]
    require(bool(providers) and final is not None, "Missing persisted provider and completion evidence")
    completion = json.loads(final[0])
    submitted = completion["_anchor_completion"]
    require(submitted["status"] == "submitted" and len(submitted["calls"]) == 1,
            "The native completion was not submitted alone")
    final_call = submitted["calls"][0]
    require(final_call["name"] == "final_result" and final_call["arguments"]["summary"] == completion["summary"],
            "Missing canonical native final_result arguments")
    require(result["completion"]["submission"] == completion["summary"], "Graph completion differs from Harness")
    commands = [(step, " ".join(call["arguments"]["command"])) for step, call in calls]
    skill_steps = {step for step, command in commands if f"/plugins/academic-research/{SKILL}" in command}
    require(any(step in skill_steps and obs.get("exit_code") == 0 and obs.get("status") == "completed"
                and skill in obs.get("stdout", "") for step, obs in observations),
            "The original Plugin Skill was not successfully read")
    search_steps = {step for step, command in commands if "/tools/scholarly/run search " in command
                    and "--source crossref" in command and "--limit 1" in command}
    require(any(step in search_steps and obs.get("exit_code") == 0 and obs.get("status") == "completed"
                for step, obs in observations), "The existing scholarly Crossref search did not succeed")
    require(any(obs.get("status") == "completed" and obs.get("exit_code") == 0
                and raw_sources in obs.get("stdout", "") for _, obs in observations),
            "The original scholarly JSON output was not read back unchanged")
    return {"provider_model_observed": sorted({row[0] for row in providers if row[0]}),
            "provider_requests": len(providers), "anchor_run_calls": len(calls),
            "completion_protocol": "native final_result tool", "completion_arguments": final_call["arguments"]}, len(providers)


def verify_recordings(state: Path, result: dict, request_count: int, doi: str) -> dict:
    key = result["key"]
    durable_key = ":".join(str(key[name]) for name in ("run_id", "graph_digest", "node_id", "invocation"))
    root = state / "io-harness/store" / ("np1-" + hashlib.sha256(durable_key.encode()).hexdigest() + ".recordings")
    roots = sorted((state / "io-harness/store").glob("*.recordings"))
    require(roots == [root], "Missing or unexpected per-invocation recording roots")
    attempts = sorted(root.iterdir())
    require(len(attempts) == request_count and bool(attempts), "Recording count differs from persisted provider calls")
    responses = []
    requests = []
    for sequence, attempt in enumerate(attempts, 1):
        require(attempt.name == f"{sequence:020}", "Provider recording sequence is incomplete")
        require(read_json(attempt / "outcome.json")["status"] == "succeeded", "Provider attempt was not recorded as succeeded")
        recording = read_json(attempt / "recording.json")
        require(recording["harness"] == "0.86.0" and len(recording["exchanges"]) == 1,
                "Invalid native io-harness recording")
        exchange = recording["exchanges"][0]
        require(exchange["request"] == read_json(attempt / "request.json"), "The recorded native request changed")
        rig_request = read_json(attempt / "rig-request.json")
        require(any(tool["name"] == "final_result" for tool in rig_request["tools"]),
                "Rig did not send the native final_result tool definition")
        rig_response = read_json(attempt / "rig-response.json")
        require("raw" not in rig_response, "A raw provider document leaked into the typed recording")
        require(all(path.stat().st_mode & 0o777 == 0o600 for path in attempt.iterdir() if path.is_file()),
                "Provider recording files are not private")
        responses.append(exchange["response"])
        requests.append(exchange["request"])
    final_calls = responses[-1]["tool_calls"]
    require(len(final_calls) == 1 and final_calls[0]["name"] == "final_result",
            "The provider did not emit final_result alone in its native response")
    require(final_calls[0]["arguments"]["summary"] == result["completion"]["submission"],
            "Native recording and Graph completion differ")
    require(any(item.get("function", {}).get("name") == "final_result"
                for item in rig_response["choice"]), "The typed Rig response lacks native final_result")
    require(any(doi in json.dumps(request, ensure_ascii=False) for request in requests),
            "No recorded provider request contains the inspected DOI")
    return {"provider_recordings": len(attempts), "recording_harness": "0.86.0",
            "recording_directory": root.as_posix(), "native_completion_recorded": True}


def verify_ownership(paths: dict[str, Path], original_graph: dict, original_plugin: dict[str, str]) -> None:
    bundle = paths["catalog"] / GRAPH_NAME
    require(read_json(bundle / "graph.json") == original_graph, "Rust Graph definition changed")
    require(tree_hashes(bundle / "plugins/academic-research") == original_plugin, "The frozen Plugin changed")
    require(tree_hashes(paths["library"] / "plugins/academic-research") == original_plugin,
            "The platform Library Plugin changed")
    platform = paths["platform"]
    require(not list((platform / "workspaces").iterdir()), "The platform created a Python-owned Graph workspace")
    require(not list(platform.rglob("graph.json")) and not list(platform.rglob("run.json")),
            "The platform created Python authoritative Graph/Run copies")
    require(all(not (platform / name).exists() for name in ("runs", "artifacts", "io-harness", "run-metadata")),
            "Rust execution facts were duplicated into the platform root")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "rust/target/debug/anchor-runner-host")
    parser.add_argument("--python", type=Path, default=ROOT / ".venv/bin/python")
    parser.add_argument("--source-root", type=Path, default=ROOT / "src")
    parser.add_argument("--timeout", type=float, default=300, help="Run completion timeout in seconds")
    args = parser.parse_args()
    source_root = args.source_root.expanduser().resolve()
    # Keep the venv path: resolving its interpreter symlink loses the tool environment.
    python = args.python.expanduser().absolute()
    from anchor.runtime.secrets import load_dotenv

    load_dotenv(ROOT / ".env")
    if any(not os.environ.get(name) for name in MODEL_KEYS):
        parser.exit(2, "Missing ANCHOR_MODEL_API_KEY/URL/NAME; no model request made\n")
    if not args.binary.is_file() or not python.is_file() or not (python.parent / "anchor-scholarly").is_file():
        parser.exit(2, "Provide a built --binary and a --python venv with anchor-scholarly; no model request made\n")
    if not (source_root / "anchor/serve.py").is_file() or args.timeout <= 0:
        parser.exit(2, "Provide a valid --source-root and positive --timeout; no model request made\n")
    (ROOT / ".local").mkdir(exist_ok=True)
    proof = Path(tempfile.mkdtemp(prefix="rust-platform-plugin-", dir=ROOT / ".local"))
    print(f"Evidence directory: {proof}", flush=True)
    rust_port, platform_port = unused_ports()
    evidence = Evidence(proof, tuple(os.environ.get(name, "") for name in (
        "ANCHOR_MODEL_API_KEY", "ANCHOR_MODEL_URL", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY",
    )))
    services: list[Service] = []
    stage = "prepare"
    try:
        original_bytes = (ROOT / "examples/graphs/plugin-research.json").read_bytes()
        original_graph = json.loads(original_bytes)
        original_plugin = tree_hashes(ROOT / "plugins/academic-research")
        paths = prepare(proof, python, source_root)
        rust_env, platform_env = service_envs(paths, source_root, rust_port)
        rust = Api(rust_port)
        platform = Api(platform_port)
        stage = "start-services"
        services.append(Service("rust", [str(args.binary.resolve()), "serve"], rust_env, evidence))
        wait_until(lambda: rust.ready(services[0], "/health"), 30, "Rust HTTP readiness")
        services.append(Service("python", [str(python), "-m", "anchor", "--root", str(paths["platform"]),
                                             "--config", str(proof / "runtime.json"), "--host", "127.0.0.1",
                                             "--port", str(platform_port)], platform_env, evidence))
        wait_until(lambda: platform.ready(services[1], "/graphs"), 30, "Python platform readiness")
        require(platform.expect("GET", "/runs")["runs"] == [], "The platform roots were not fresh")
        plugin = platform.expect("GET", "/plugins/academic-research")
        evidence.save("platform-plugin.json", plugin)
        require(plugin.get("available") is True and SKILL in plugin["skills"], "The original Plugin is unavailable")
        status, skill = platform.raw("GET", f"/plugins/academic-research/files/{SKILL}")
        require(status == 200 and skill == (ROOT / "plugins/academic-research" / SKILL).read_bytes(),
                "The platform did not expose the original Skill")
        stage = "create-graph-through-platform"
        create_body = {"name": GRAPH_NAME, "definition": original_graph}
        evidence.save("create-request.json", create_body)
        evidence.save("create-response.json", platform.expect("POST", "/graphs", create_body, status=201))
        deployed = platform.expect("GET", f"/graphs/{GRAPH_NAME}")
        evidence.save("platform-graph.json", deployed)
        require(deployed["definition"] == original_graph, "The platform changed the original Graph")
        stage = "trigger-through-platform"
        trigger_body = {"graph": GRAPH_NAME, "input": {"task": TASK}}
        evidence.save("trigger-request.json", trigger_body)
        triggered = platform.expect("POST", "/trigger", trigger_body, status=202)
        evidence.save("trigger-response.json", triggered)
        run_id = triggered["run"]
        stage = "execute-academic-plugin"
        detail = wait_run(platform, run_id, services, evidence, args.timeout)
        evidence.save("rust-run.json", rust.expect("GET", f"/runs/{run_id}"))
        stage = "verify-artifacts-and-harness"
        result, artifacts, raw_sources, doi = verify_artifacts(platform, paths, run_id, detail, evidence)
        harness, count = verify_harness(paths["state"], result, raw_sources, skill.decode("utf-8"))
        recordings = verify_recordings(paths["state"], result, count, doi)
        stage = "verify-authority-and-original-resources"
        verify_ownership(paths, original_graph, original_plugin)
        require((ROOT / "examples/graphs/plugin-research.json").read_bytes() == original_bytes,
                "The original Graph source changed")
        require(tree_hashes(ROOT / "plugins/academic-research") == original_plugin, "The original Plugin source changed")
        result_evidence = {
            "status": "passed", "run": run_id, "graph": GRAPH_NAME,
            "route": "Python HTTP platform -> Rust HTTP host -> shared Rust Runtime",
            "graph_unchanged": True, "plugin_unchanged": True, "python_authoritative_graph_run_copies": 0,
            "library": str(paths["library"]), "runtime": "Rust io-harness",
            "tool": "existing Python anchor-scholarly", "provider": "real configured model",
            "source": "crossref", "doi": doi, "original_graph_sha256": hashlib.sha256(original_bytes).hexdigest(),
            "original_plugin_files": original_plugin,
            "research_artifact": str((artifacts / "research.md").relative_to(proof)),
            "scope": "Original Plugin Skill and live Crossref metadata search; not a full academic review",
            **harness, **recordings,
        }
        evidence.save("evidence.json", result_evidence)
        print(evidence.redact(json.dumps({
            "status": "passed", "run": run_id, "doi": doi, "provider_requests": count,
            "provider_recordings": recordings["provider_recordings"], "evidence": str(proof / "evidence.json"),
        }, indent=2, ensure_ascii=False)))
        return 0
    except (SmokeFailure, OSError, ValueError, KeyError, IndexError, TypeError, sqlite3.Error) as error:
        failure = {"status": "failed", "stage": stage, "error_kind": type(error).__name__,
                   "check": str(error) if isinstance(error, SmokeFailure) else "Inspect the retained service and runtime evidence"}
        evidence.save("evidence.json", failure)
        print(evidence.redact(json.dumps({**failure, "evidence": str(proof / "evidence.json")}, indent=2)))
        return 1
    finally:
        for service in reversed(services):
            service.stop()


if __name__ == "__main__":
    raise SystemExit(main())
