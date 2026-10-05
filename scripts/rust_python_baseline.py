"""Measure a provider-free Op Graph with the Python and packaged Rust hosts.

This is a local decision aid, not a cross-machine performance claim. It keeps
the Graph shape and shell work equivalent, then records cold process time and
maximum resident set size for each host.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import statistics
import struct
import subprocess
import sys
import tarfile
import tempfile
import time


ROOT = Path(__file__).resolve().parents[1]
PACKAGER = ROOT / "scripts/package_rust_runtime.py"


def graph_definition() -> dict:
    return {
        "objective": "Run the same two Op transformations for a local baseline.",
        "entry": "first",
        "ops": {
            "first": {"run": "sh -c 'printf benchmark > first.txt'"},
            "second": {"run": "sh -c 'cat /in/first/first.txt > second.txt'"},
        },
        "nodes": [{"id": "first", "op": "first"}, {"id": "second", "op": "second"}],
        "edges": [{"from": "first", "to": "second"}],
    }


def measured(command, env, cwd, output, input_data=None):
    started = time.perf_counter()
    process = subprocess.Popen(command, stdin=subprocess.PIPE if input_data is not None else None,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, cwd=cwd)
    if input_data is not None:
        process.stdin.write(input_data)
        process.stdin.close()
        process.stdin = None
    peak_rss = 0
    while process.poll() is None:
        try:
            status = Path(f"/proc/{process.pid}/status").read_text(encoding="utf-8")
            resident = next(line for line in status.splitlines() if line.startswith("VmRSS:"))
            peak_rss = max(peak_rss, int(resident.split()[1]))
        except (FileNotFoundError, StopIteration):
            pass
        time.sleep(0.005)
    stdout, stderr = process.communicate()
    elapsed = time.perf_counter() - started
    if process.returncode:
        raise RuntimeError(f"benchmark command failed ({process.returncode}): {stderr.decode(errors='replace')}")
    output.write_bytes(stdout)
    return {"seconds": elapsed, "max_rss_kib": peak_rss, "stdout": stdout}


def timed(command, env, cwd, output):
    result = measured(command, env, cwd, output)
    result.pop("stdout")
    return result


def timed_input(command, env, cwd, data, output):
    result = measured(command, env, cwd, output, data)
    stdout = result.pop("stdout")
    if len(stdout) < 4:
        raise RuntimeError("Rust benchmark returned no framed response")
    size = struct.unpack(">I", stdout[:4])[0]
    result["response"] = json.loads(stdout[4 : 4 + size])
    return result


def package_runtime(proof, bundle):
    archive = proof / "anchor-runtime.tar.gz"
    subprocess.run(
        [sys.executable, str(PACKAGER), "--binary", str(ROOT / "rust/target/release/anchor-runner-host"),
         "--bundle", str(bundle), "--output", str(archive)],
        check=True,
        capture_output=True,
        text=True,
    )
    deployment = proof / "deployment"
    deployment.mkdir()
    with tarfile.open(archive, "r:gz") as package:
        package.extractall(deployment)
    return deployment / "anchor-runtime", archive


def framed_command(binary, env, cwd, run_id, output):
    request = {"op": "start_bundle", "version": 1, "request_id": run_id, "run_id": run_id, "input": {}}
    payload = json.dumps(request).encode()
    command = [str(binary)]
    result = timed_input(command, env, cwd, struct.pack(">I", len(payload)) + payload, output)
    if result["response"].get("status") != "completed":
        raise RuntimeError(f"Rust baseline Run did not complete: {result['response']}")
    return {key: result[key] for key in ("seconds", "max_rss_kib")}


def tree_disk_bytes(root: Path) -> int:
    result = subprocess.run(["du", "-sk", str(root)], check=True, capture_output=True, text=True)
    return int(result.stdout.split()[0]) * 1024


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--iterations", type=int, default=5)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.iterations < 1:
        raise SystemExit("--iterations must be positive")
    proof = Path(tempfile.mkdtemp(prefix="rust-python-baseline-", dir=ROOT / ".local"))
    definition = graph_definition()
    bundle = proof / "bundle"
    bundle.mkdir()
    (bundle / "graph.json").write_text(json.dumps(definition), encoding="utf-8")
    (bundle / "manifest.json").write_text(json.dumps({"format": 1, "graph": "graph.json", "plugins": []}), encoding="utf-8")
    runtime_root, archive = package_runtime(proof, bundle)
    rust_state = runtime_root / "state"
    rust_work = runtime_root / "workspaces"
    rust_state.mkdir()
    rust_work.mkdir()
    rust_env = {
        "PATH": "/usr/bin:/bin",
        "ANCHOR_RUNNER_BUNDLE_ROOT": str(runtime_root / "bundle"),
        "ANCHOR_RUNNER_STATE_ROOT": str(rust_state),
        "ANCHOR_RUNNER_WORKSPACE_ROOT": str(rust_work),
        "ANCHOR_RUNNER_ALLOWED_COMMANDS": "sh,cat,printf",
        "ANCHOR_BWRAP": "/usr/bin/bwrap",
    }
    python_config = proof / "runtime.json"
    python_config.write_text('{"models": []}\n', encoding="utf-8")
    python_env = {
        "PATH": "/usr/bin:/bin",
        "HOME": str(proof / "python-home"),
        "PYTHONPATH": str(ROOT / "src"),
        "ANCHOR_MODEL_API_KEY": "",
        "ANCHOR_MODEL_URL": "",
        "ANCHOR_MODEL_NAME": "",
    }
    (proof / "python-home").mkdir()
    python_samples = []
    rust_samples = []
    for index in range(args.iterations):
        python_workspace = proof / f"python-{index}"
        python_workspace.mkdir()
        (python_workspace / "graph.json").write_text(json.dumps(definition), encoding="utf-8")
        python_samples.append(timed(
            [sys.executable, "-m", "anchor.simple", str(python_workspace), "--config", str(python_config)],
            python_env,
            python_workspace,
            proof / f"python-{index}.json",
        ))
        rust_samples.append(framed_command(
            runtime_root / "bin/anchor-runner-host",
            rust_env,
            runtime_root,
            f"rust-baseline-{index}",
            proof / f"rust-{index}.bin",
        ))
    evidence = {
        "status": "passed",
        "proof": str(proof),
        "iterations": args.iterations,
        "graph": "two provider-free Op nodes with an immutable input edge",
        "rust_archive_bytes": archive.stat().st_size,
        "python_environment_disk_bytes": tree_disk_bytes(Path(sys.executable).parent.parent),
        "python": python_samples,
        "rust": rust_samples,
        "summary": {
            "python_seconds_median": statistics.median(item["seconds"] for item in python_samples),
            "rust_seconds_median": statistics.median(item["seconds"] for item in rust_samples),
            "python_rss_kib_median": statistics.median(item["max_rss_kib"] for item in python_samples),
            "rust_rss_kib_median": statistics.median(item["max_rss_kib"] for item in rust_samples),
        },
    }
    (proof / "evidence.json").write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(evidence, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
