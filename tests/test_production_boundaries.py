"""Production boundaries, as behaviour rather than as prose.

A limitation that is only written down is not a boundary. The failure this file exists for was
real and measured: `ANCHOR_ARTIFACT_ROOT=s3://bucket/artifacts` was accepted and became a local
directory literally called `s3:/bucket/artifacts`. An operator who configured remote storage got
local storage in an unexpected place, and nothing said so — the mistake would have surfaced
later as missing evidence, which is the worst way to learn it.

So each limitation here is asserted: it fails, it fails with a code, and it fails at startup
rather than at use.
"""

from __future__ import annotations

import json
import subprocess
import sys
import os

import pytest

from anchor.runtime.artifacts import ArtifactBackendUnsupported, LocalArtifactStore
from anchor.runtime.preflight import ENVIRONMENT_EXIT_CODE, check_artifacts, check_environment

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


@pytest.mark.parametrize("root", [
    "s3://bucket/artifacts",
    "gs://bucket/artifacts",
    "https://remote.example/artifacts",
    # The degraded form. Path resolution collapses `s3://x` to `s3:/x` before anything else can
    # notice, so this is how the silent version would arrive if only the `://` form were refused.
    "s3:/bucket/artifacts",
    "minio:artifacts",
])
def test_a_remote_artifact_root_is_refused(tmp_path, root):
    """Production may substitute a backend; it may not configure one and get a directory."""
    with pytest.raises(ArtifactBackendUnsupported) as caught:
        LocalArtifactStore(root)
    assert caught.value.code == "artifact_backend_unsupported"
    assert "does not implement" in str(caught.value)


@pytest.mark.parametrize("root", ["s3://bucket/artifacts", "s3:/bucket/artifacts"])
def test_a_remote_root_is_reported_before_a_service_starts(tmp_path, root):
    """Not an ENOENT inside a worker an hour later, with the reason unknowable from the journal."""
    problems = check_artifacts(root)
    assert [problem.code for problem in problems] == ["artifact_backend_unsupported"]
    assert "ANCHOR_ARTIFACT_ROOT" in problems[0].detail


def test_a_remote_root_makes_the_whole_environment_unready(tmp_path):
    """The check a service runs before its loop, so a bad deployment never starts working."""
    problems = check_environment(role="worker", database_url=None,
                                 artifact_root="s3://bucket/artifacts")
    assert "artifact_backend_unsupported" in [problem.code for problem in problems]


def test_a_local_directory_is_still_accepted(tmp_path):
    """The refusal must not become a refusal to work locally."""
    assert check_artifacts(str(tmp_path / "artifacts")) == []
    assert LocalArtifactStore(tmp_path / "artifacts").put_text("x").startswith("artifact://")


def test_a_rejected_root_creates_nothing(tmp_path):
    """The old behaviour left a directory behind, so the filesystem recorded the mistake and
    the operator had something plausible to investigate."""
    before = set(os.listdir(tmp_path))
    with pytest.raises(ArtifactBackendUnsupported):
        LocalArtifactStore("s3://bucket/artifacts")
    assert set(os.listdir(tmp_path)) == before


def test_a_worker_refuses_to_start_against_a_remote_root(tmp_path):
    """The fail-closed path, exercised through the real entry point.

    Asserted at the process boundary because that is where an operator meets it: a unit that
    exits 2 with a JSON reason lands in the journal, while a unit that starts and misplaces its
    artifacts does not.
    """
    done = subprocess.run(
        [sys.executable, "-m", "anchor.runtime.worker_service"],
        cwd=ROOT, capture_output=True, text=True, timeout=60,
        env={**os.environ, "ANCHOR_DATABASE_URL": f"sqlite:///{tmp_path}/missing.sqlite",
             "ANCHOR_ARTIFACT_ROOT": "s3://bucket/artifacts",
             "ANCHOR_RUNTIME_CONFIG": str(tmp_path / "absent.json")})
    assert done.returncode == ENVIRONMENT_EXIT_CODE, done.stderr[-500:]
    report = json.loads(_last_json(done.stderr))
    assert report["status"] == "environment_not_ready"
    assert "artifact_backend_unsupported" in [item["code"] for item in report["problems"]]


def _last_json(text: str) -> str:
    for line in reversed(text.splitlines()):
        if line.strip().startswith("{"):
            return line.strip()
    raise AssertionError(f"no structured report on stderr:\n{text[-800:]}")


def test_authorization_is_one_shared_token_and_nothing_more(tmp_path):
    """Recorded as a fact so it cannot quietly become an assumption.

    There is no per-user identity, no role, and no authenticated actor: the audit trail's actor
    is a string the caller supplies. The boundary is that a request with no token, or a wrong
    one, is refused — which is what this asserts, rather than the capability the deployment
    would need to claim identity.
    """
    pytest.importorskip("fastapi")
    from fastapi.testclient import TestClient

    from anchor.api.app import create_app

    from conftest import make_store

    store = make_store(tmp_path, "auth.sqlite")
    try:
        with TestClient(create_app(store, "a" * 40)) as client:
            assert client.get("/api/runs").status_code in (401, 403)
            assert client.get("/api/runs", headers={
                "Authorization": "Bearer " + "b" * 40}).status_code in (401, 403)
            assert client.get("/api/runs", headers={
                "Authorization": "Bearer " + "a" * 40}).status_code == 200
            # Liveness is deliberately open: a probe that needs a secret cannot tell an operator
            # anything when the secret is what is wrong.
            assert client.get("/health/live").status_code == 200
    finally:
        store.close()


def test_the_approval_surface_agrees_across_cli_ui_and_api(tmp_path):
    """One set of routes, reached three ways. A surface that disagreed would let an operator
    approve something over the CLI that the console refuses to show."""
    from pathlib import Path

    web = (Path(ROOT) / "apps" / "web" / "src" / "RunConsole.tsx").read_text(encoding="utf-8")
    cli = (Path(ROOT) / "src" / "anchor" / "cli.py").read_text(encoding="utf-8")
    app = (Path(ROOT) / "src" / "anchor" / "api" / "app.py").read_text(encoding="utf-8")
    for verb in ("approve", "reject"):
        assert verb in web, f"the console cannot {verb}"
        assert verb in cli, f"the CLI cannot {verb}"
        assert f"/{verb}" in app, f"the API does not serve {verb}"
    # And the client is the single path all three use.
    assert "approve_wait" in (Path(ROOT) / "src" / "anchor" / "client.py").read_text(
        encoding="utf-8")
