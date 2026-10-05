"""Verify pause, packaged-host restart, and resume for a Rust Graph Run."""

from __future__ import annotations

import json
from pathlib import Path
import socket
import subprocess
import sys
import tarfile
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen
import uuid


ROOT = Path(__file__).resolve().parents[1]
PACKAGER = ROOT / "scripts/package_rust_runtime.py"


def until(check, seconds=30):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = check()
        if value is not None:
            return value
        time.sleep(0.1)
    raise TimeoutError("recovery observation deadline exceeded")


def terminate(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def request(base, method, path, body=None):
    request_data = None if body is None else json.dumps(body).encode()
    request_obj = Request(
        base + path,
        method=method,
        data=request_data,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urlopen(request_obj, timeout=15) as response:
            return response.status, json.loads(response.read())
    except HTTPError as error:
        return error.code, json.loads(error.read())


def package_runtime(proof, bundle):
    archive = proof / "anchor-runtime.tar.gz"
    subprocess.run(
        [
            sys.executable,
            str(PACKAGER),
            "--binary",
            str(ROOT / "rust/target/release/anchor-runner-host"),
            "--bundle",
            str(bundle),
            "--output",
            str(archive),
        ],
        check=True,
    )
    deployment = proof / "deployment"
    deployment.mkdir()
    with tarfile.open(archive, "r:gz") as package:
        package.extractall(deployment)
    return deployment / "anchor-runtime"


def start_host(runtime_root, env, proof):
    log = (proof / "host.log").open("ab")
    return subprocess.Popen(
        [str(runtime_root / "bin/anchor-runner-host"), "serve"],
        env=env,
        cwd=runtime_root,
        stdout=log,
        stderr=subprocess.STDOUT,
    )


def wait_ready(process, base):
    def ready():
        if process.poll() is not None:
            raise RuntimeError("packaged host exited; inspect host.log")
        try:
            return request(base, "GET", "/health")[0] == 200
        except (URLError, ConnectionError):
            return None

    return until(ready)


def wait_run(base, run_id, wanted):
    terminal = {"completed", "failed", "stopped"}

    def observe():
        status, value = request(base, "GET", f"/runs/{run_id}")
        if status != 200:
            raise RuntimeError(f"run lookup failed: {status} {value}")
        current = value["state"]["status"]
        if current == wanted:
            return value
        if current in terminal:
            raise RuntimeError(f"Run settled before {wanted}: {current}")
        return None

    return until(observe)


def main():
    output_root = ROOT / ".local"
    output_root.mkdir(exist_ok=True)
    proof = Path(tempfile.mkdtemp(prefix="rust-runtime-recovery-", dir=output_root))
    bundle = proof / "bundle"
    bundle.mkdir()
    graph = {
        "objective": "Pause and resume a packaged Run after host restart.",
        "entry": "first",
        "ops": {
            "first": {"run": "sh -c 'sleep 2; printf first > first.txt'"},
            "second": {"run": "sh -c 'cat /in/first/first.txt > second.txt'"},
        },
        "nodes": [{"id": "first", "op": "first"}, {"id": "second", "op": "second"}],
        "edges": [{"from": "first", "to": "second"}],
    }
    (bundle / "graph.json").write_text(json.dumps(graph), encoding="utf-8")
    (bundle / "manifest.json").write_text(
        json.dumps({"format": 1, "graph": "graph.json", "plugins": []}),
        encoding="utf-8",
    )
    runtime_root = package_runtime(proof, bundle)
    state_root = runtime_root / "state"
    workspace_root = runtime_root / "workspaces"
    state_root.mkdir()
    workspace_root.mkdir()
    with socket.socket() as reserved:
        reserved.bind(("127.0.0.1", 0))
        port = reserved.getsockname()[1]
    env = {
        "PATH": "/usr/bin:/bin",
        "ANCHOR_RUNNER_BUNDLE_ROOT": str(runtime_root / "bundle"),
        "ANCHOR_RUNNER_STATE_ROOT": str(state_root),
        "ANCHOR_RUNNER_WORKSPACE_ROOT": str(workspace_root),
        "ANCHOR_RUNNER_ALLOWED_COMMANDS": "sh,cat,printf,sleep",
        "ANCHOR_RUNNER_LISTEN": f"127.0.0.1:{port}",
        "ANCHOR_BWRAP": "/usr/bin/bwrap",
    }
    base = f"http://127.0.0.1:{port}"
    run_id = f"recovery-{uuid.uuid4().hex}"
    process = None

    try:
        process = start_host(runtime_root, env, proof)
        wait_ready(process, base)
        status, trigger = request(base, "POST", "/trigger", {"graph": "bundle"})
        if status != 202:
            raise RuntimeError(f"trigger failed: {status} {trigger}")
        run_id = trigger["run"]
        status, pause = request(base, "POST", f"/runs/{run_id}/pause")
        if status != 202:
            raise RuntimeError(f"pause failed: {status} {pause}")

        paused_record = wait_run(base, run_id, "paused")
        terminate(process)
        process = start_host(runtime_root, env, proof)
        wait_ready(process, base)
        status, resumed = request(base, "POST", f"/runs/{run_id}/resume")
        if status != 202:
            raise RuntimeError(f"resume failed: {status} {resumed}")

        completed_record = wait_run(base, run_id, "completed")
        artifacts = [path for path in (state_root / "artifacts").rglob("second.txt") if path.is_file()]
        if len(artifacts) != 1 or artifacts[0].read_text() != "first":
            raise RuntimeError(f"resumed Artifact is missing or incorrect: {artifacts}")
        evidence = {
            "status": "passed",
            "proof": str(proof),
            "run": run_id,
            "paused_status": paused_record["state"]["status"],
            "completed_status": completed_record["state"]["status"],
            "artifact": str(artifacts[0].relative_to(runtime_root)),
            "packaged_runtime": True,
        }
        (proof / "evidence.json").write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8")
        print(json.dumps(evidence, ensure_ascii=False, indent=2))
    finally:
        if process is not None:
            terminate(process)


if __name__ == "__main__":
    main()
