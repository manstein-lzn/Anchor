"""Run a packaged Rust Graph Runtime in a source-free deployment directory.

The Python process only builds and observes the package. Graph execution is
performed by the extracted Rust binary with a minimal deployment environment.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import struct
import subprocess
import sys
import tarfile
import tempfile


ROOT = Path(__file__).resolve().parents[1]
PACKAGER = ROOT / "scripts/package_rust_runtime.py"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True, help="release anchor-runner-host binary")
    parser.add_argument("--bundle", type=Path, required=True, help="format-1 Graph bundle directory")
    parser.add_argument("--input-json", default="{}", help="JSON object passed as Graph input")
    parser.add_argument("--run-id", default="package-smoke", help="durable Run id")
    parser.add_argument("--timeout", type=float, default=120.0, help="execution timeout in seconds")
    return parser.parse_args()


def safe_extract(archive_path: Path, destination: Path) -> None:
    with tarfile.open(archive_path, "r:gz") as archive:
        members = archive.getmembers()
        for member in members:
            name = Path(member.name)
            if name.is_absolute() or ".." in name.parts or member.issym() or member.islnk():
                raise ValueError(f"unsafe package member: {member.name}")
        archive.extractall(destination)


def package(binary: Path, bundle: Path, output: Path) -> None:
    result = subprocess.run(
        [sys.executable, str(PACKAGER), "--binary", str(binary), "--bundle", str(bundle), "--output", str(output)],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode:
        raise RuntimeError(f"runtime packaging failed: {result.stderr.strip()}")


def framed_start(
    binary: Path,
    deployment_root: Path,
    env: dict[str, str],
    run_id: str,
    graph_input: object,
    timeout: float,
) -> dict:
    request = {"op": "start_bundle", "version": 1, "request_id": "package-smoke", "run_id": run_id, "input": graph_input}
    payload = json.dumps(request).encode()
    result = subprocess.run(
        [str(binary)],
        input=struct.pack(">I", len(payload)) + payload,
        capture_output=True,
        env=env,
        cwd=deployment_root,
        timeout=timeout,
        check=False,
    )
    if result.returncode:
        raise RuntimeError(f"runtime exited {result.returncode}: {result.stderr.decode(errors='replace')}")
    if len(result.stdout) < 4:
        raise RuntimeError("runtime returned no framed response")
    size = struct.unpack(">I", result.stdout[:4])[0]
    try:
        return json.loads(result.stdout[4 : 4 + size])
    except json.JSONDecodeError as exc:
        raise RuntimeError("runtime returned invalid JSON") from exc


def assert_source_free(archive_path: Path) -> None:
    with tarfile.open(archive_path, "r:gz") as archive:
        names = archive.getnames()
    forbidden = (".py", ".git/", "/src/", "pyproject.toml")
    if any(any(token in name for token in forbidden) for name in names):
        raise AssertionError(f"package contains source or Python metadata: {names}")


def main() -> int:
    args = parse_args()
    graph_input = json.loads(args.input_json)
    if not isinstance(graph_input, dict):
        raise SystemExit("--input-json must be a JSON object")
    (ROOT / ".local").mkdir(exist_ok=True)
    proof = Path(tempfile.mkdtemp(prefix="rust-runtime-package-", dir=ROOT / ".local"))
    archive = proof / "anchor-runtime.tar.gz"
    package(args.binary.resolve(), args.bundle.resolve(), archive)
    assert_source_free(archive)
    deployment = proof / "deployment"
    deployment.mkdir()
    safe_extract(archive, deployment)
    runtime_root = deployment / "anchor-runtime"
    state_root = runtime_root / "state"
    workspace_root = runtime_root / "workspaces"
    state_root.mkdir()
    workspace_root.mkdir()
    env = {
        "PATH": "/usr/bin:/bin",
        "ANCHOR_RUNNER_BUNDLE_ROOT": str(runtime_root / "bundle"),
        "ANCHOR_RUNNER_STATE_ROOT": str(state_root),
        "ANCHOR_RUNNER_WORKSPACE_ROOT": str(workspace_root),
        "ANCHOR_RUNNER_ALLOWED_COMMANDS": "sh",
        "ANCHOR_BWRAP": "/usr/bin/bwrap",
    }
    response = framed_start(
        runtime_root / "bin/anchor-runner-host",
        runtime_root,
        env,
        args.run_id,
        graph_input,
        args.timeout,
    )
    (proof / "response.json").write_text(json.dumps(response, indent=2) + "\n", encoding="utf-8")
    if response.get("status") != "completed":
        raise RuntimeError(f"packaged Graph did not complete: {response}")
    run_path = state_root / "runs" / f"{args.run_id}.json"
    if not run_path.is_file():
        raise RuntimeError(f"durable Run record is missing: {run_path}")
    artifact_files = [path for path in (state_root / "artifacts").rglob("*") if path.is_file() and path.name != "manifest.json"]
    if not artifact_files:
        raise RuntimeError("completed packaged Graph produced no Artifact files")
    evidence = {
        "status": "passed",
        "proof": str(proof),
        "archive": str(archive),
        "response": response,
        "run": str(run_path.relative_to(runtime_root)),
        "artifacts": [str(path.relative_to(runtime_root)) for path in artifact_files],
        "python_runtime_in_deployment": False,
    }
    (proof / "evidence.json").write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(evidence, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
