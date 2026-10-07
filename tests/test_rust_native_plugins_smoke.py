"""Native smoke checks use synthetic evidence and loopback HTTP, never a model."""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
import sys
from types import SimpleNamespace
from urllib.error import HTTPError
from urllib.parse import urlencode
from urllib.request import ProxyHandler, Request, build_opener

import pytest

sys.path.insert(0, str(Path(__file__).parents[1] / "scripts"))
import rust_native_plugins_smoke as smoke
from rust_low_cost_regression import invocation_digest


def test_driver_reuses_existing_runtime_wait_port():
    from rust_low_cost_regression import wait_run

    assert smoke.wait_run is wait_run


@pytest.mark.parametrize("timeout", [5, 30])
def test_fixture_client_preserves_default_and_supports_bounded_admission_timeout(monkeypatch, timeout):
    from io import BytesIO

    client = smoke.Api(12345) if timeout == 5 else smoke.Api(12345, timeout=timeout)
    observed = []

    def opened(_request, *, timeout):
        observed.append(timeout)
        response = BytesIO(b'{}')
        response.status = 200
        return response

    monkeypatch.setattr(client.opener, "open", opened)
    assert client.raw("GET", "/health") == (200, b'{}')
    assert observed == [timeout]


def save(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value), encoding="utf-8")


@pytest.fixture
def fixture(tmp_path):
    server = smoke.Fixture(smoke.Evidence(tmp_path, ("private-model-key",)))
    try:
        yield server
    finally:
        server.stop()


def http(fixture, method, path, data=None, headers=None):
    request = Request(fixture.endpoint + path, data=data, headers=headers or {}, method=method)
    try:
        response = build_opener(ProxyHandler({})).open(request, timeout=5)
    except HTTPError as error:
        response = error
    with response:
        raw = response.read()
        return response.code, json.loads(raw) if response.headers.get_content_type() == "application/json" else raw


def multipart(fixture, image=smoke.IMAGE):
    return (f'--test-boundary\r\nContent-Disposition: form-data; name="pageId"\r\n\r\n{fixture.page}\r\n'
            '--test-boundary\r\nContent-Disposition: form-data; name="file"; filename="panel.png"\r\n'
            'Content-Type: image/png\r\n\r\n').encode() + image + b'\r\n--test-boundary--\r\n'


def test_loopback_fixture_accepts_only_exact_native_requests_and_records_http_errors(fixture):
    assert http(fixture, "GET", "/cgi-bin/gettoken?" + urlencode({"corpid": "fixture-corp", "corpsecret": "fixture-secret"}))[0] == 200
    status, member = http(fixture, "GET", "/cgi-bin/user/get?" + urlencode({"access_token": "fixture-token", "userid": fixture.userid}))
    assert status == 200 and member == fixture.expected["wecom"]
    headers = {"Authorization": "Bearer fixture-docmost-key", "Content-Type": "multipart/form-data; boundary=test-boundary"}
    status, upload = http(fixture, "POST", "/api/files/upload", multipart(fixture), headers)
    assert status == 200 and upload["id"] == fixture.expected["docmost"]["attachmentId"]
    assert http(fixture, "POST", "/api/files/upload", multipart(fixture, b"changed"), headers)[0] == 400
    assert http(fixture, "GET", "/cgi-bin/user/get?access_token=wrong&userid=wrong")[0] == 400
    assert http(fixture, "POST", "/cgi-bin/message/send", b"{}")[0] == 404
    assert http(fixture, "DELETE", "/api/files/upload")[0] == 501
    assert [call["status"] for call in fixture.calls] == [200, 200, 200, 400, 400, 404, 501]
    audit = smoke.read_json(fixture.evidence.root / "business-calls.json")
    assert audit == fixture.calls
    assert "access_token" not in json.dumps(audit)


def package_binaries(tmp_path, monkeypatch, mismatch=False):
    binaries = {}
    for name in ("host", "wecom", "docmost"):
        binaries[name] = tmp_path / f"source-{name}"
        binaries[name].write_bytes(f"fixed-{name}-binary".encode())
        binaries[name].chmod(0o755)

    def package(command, *, env, capture_output, timeout, check):
        assert command[1] == "package-plugin" and env == {} and capture_output and timeout == 30 and not check
        name = next(name for name in ("wecom", "docmost") if str(binaries[name]) == command[0] or f"anchor-{name}" in command[0])
        plugin = Path(command[2])
        binary = plugin / f"bin/anchor-{name}-tools"
        binary.parent.mkdir(parents=True)
        binary.write_bytes(b"mismatch" if mismatch else binaries[name].read_bytes())
        binary.chmod(0o755)
        skill = plugin / f"skills/{name}/SKILL.md"
        skill.parent.mkdir(parents=True)
        skill.write_text(f"# {name} fixture Skill\nOnly use approved fixture data.\n")
        servers = {"wecom": {"command": "bin/anchor-wecom-tools", "args": [], "cwd": ".", "optional_env_vars": ["WECOM_CORP_ID", "WECOM_AGENT_ID", "WECOM_SECRET"]}} if name == "wecom" else {
            "docmost": {"type": "http", "url": "https://never-contact.invalid/mcp", "bearer_token_env_var": "DOCMOST_API_KEY"},
            "attachments": {"command": "bin/anchor-docmost-tools", "args": [], "cwd": ".", "env_vars": ["DOCMOST_API_KEY"]}}
        save(plugin / "plugin.json", {"name": name, "skills": "skills/", "mcpServers": servers})
        return SimpleNamespace(returncode=0, stdout=b"", stderr=b"")

    monkeypatch.setattr(smoke.subprocess, "run", package)
    return binaries


def test_prepare_freezes_native_package_bindings_and_only_fake_endpoints(tmp_path, monkeypatch, fixture):
    binaries = package_binaries(tmp_path, monkeypatch)
    graph = smoke.prepare(tmp_path, binaries, fixture, fixture.evidence)
    manifest = smoke.read_json(tmp_path / "bundle/manifest.json")
    for name, binding in zip(smoke.SKILLS, manifest["plugins"], strict=True):
        plugin = tmp_path / "bundle/plugins" / name
        digest = hashlib.sha256()
        for resource in binding["resources"]:
            digest.update(resource.encode())
            digest.update(hashlib.sha256((plugin / resource).read_bytes()).digest())
        assert binding["digest"] == digest.hexdigest()
        assert binding["mcp_servers"] == (["wecom"] if name == "wecom" else ["attachments"])
    upload = smoke.read_json(tmp_path / "bundle/plugins/docmost/plugin.json")["mcpServers"]
    assert list(upload) == ["attachments"]
    assert upload["attachments"]["args"] == ["--endpoint", fixture.endpoint + "/api/files/upload"]
    assert upload["attachments"]["env"] == {"DOCMOST_API_KEY": "fixture-docmost-key"}
    assert "env_vars" not in upload["attachments"]
    wecom = smoke.read_json(tmp_path / "bundle/plugins/wecom/plugin.json")["mcpServers"]["wecom"]
    assert "optional_env_vars" not in wecom
    assert wecom["env"]["WECOM_SECRET"] == "fixture-secret"
    assert graph["agents"]["worker"]["wall_time_limit_seconds"] == 60
    assert graph["agents"]["worker"]["network"] is True
    assert fixture.expected["wecom"]["name"] not in json.dumps(graph)
    assert fixture.expected["docmost"]["attachmentId"] not in json.dumps(graph)
    assert "fixture-image" in graph["ops"]["publish"]["run"]
    (tmp_path / "bundle/plugins/wecom/bad").symlink_to(binaries["host"])
    with pytest.raises(smoke.SmokeFailure, match="symlinks"):
        smoke.binding(tmp_path / "bundle/plugins/wecom")


def test_mismatched_package_binary_is_not_accepted(tmp_path, monkeypatch, fixture):
    binaries = package_binaries(tmp_path, monkeypatch, mismatch=True)
    with pytest.raises(smoke.SmokeFailure, match="Packaged binary differs"):
        smoke.prepare(tmp_path, binaries, fixture, fixture.evidence)


def acceptance_evidence(tmp_path, monkeypatch, fixture):
    binaries = package_binaries(tmp_path, monkeypatch)
    graph = smoke.prepare(tmp_path, binaries, fixture, fixture.evidence)
    original_input = {"acceptance": "unchanged-input"}
    record = {"status": "completed", "run_id": "run", "input": original_input.copy(), "invocations": {"publish": 1, "worker": 1}, "results": {}}
    for node, files in {"publish": {"assets/panel.png": smoke.IMAGE}, "worker": {"report.json": json.dumps(fixture.expected).encode(), "effects.txt": b"once\n"}}.items():
        result = {"key": {"run_id": "run", "graph_digest": "digest", "node_id": node, "invocation": 1},
                  "commit": {"id": node}, "completion": {"submission": "native Plugins verified", "route": None}}
        record["results"][node] = [result]
        artifact = tmp_path / "state/artifacts" / node
        save(artifact / "manifest.json", {"key": result["key"], "completion": result["completion"], "files": {
            name: {"bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest()} for name, raw in files.items()}})
        for name, raw in files.items():
            for parent in (artifact / "files", tmp_path / "work/run" / invocation_digest(result)):
                (parent / name).parent.mkdir(parents=True, exist_ok=True)
                (parent / name).write_bytes(raw)
    attempt = tmp_path / "state/io-harness/store" / f"np1-{invocation_digest(record['results']['worker'][0])}.recordings/01"
    save(attempt / "outcome.json", {"status": "succeeded"})
    save(attempt / "recording.json", {"exchanges": [{"response": {"usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5},
         "tool_calls": [{"name": "final_result", "arguments": {"summary": "native Plugins verified"}}]}}]})
    skill_bytes = "".join((tmp_path / "bundle" / path.removeprefix("/")).read_text() for path in smoke.SKILLS.values())
    history = [{"role": "assistant", "commands": ["anchor_run " + json.dumps({"command": ["sh", "-c", smoke.READ]})]},
               {"role": "tool", "text": "\n[anchor_run]\n" + json.dumps({"exit_code": 0, "stdout": skill_bytes + "input-readonly\n"})}]
    for name, tool in smoke.TOOLS.items():
        arguments = {"userid": fixture.userid} if name == "wecom" else {"path": "/in/publish/assets/panel.png", "pageId": fixture.page}
        history.extend([{"role": "assistant", "commands": [tool + " " + json.dumps(arguments)]},
                        {"role": "tool", "text": f"\n[{tool}]\n" + json.dumps(fixture.expected[name])}])
    detail = {"active": False, "state": {"input": original_input.copy()}, "traces": {'["worker",1]': history}}
    for kind in ("token", "member", "upload"):
        fixture.record({"kind": kind, "status": 200})
    return graph, record, detail, original_input


@pytest.mark.parametrize("bad", [None, "report", "hash", "workspace", "producer", "input", "skill_read", "missing_call", "missing_result", "duplicate", "http_error"])
def test_verifier_rejects_false_success_or_changed_facts(tmp_path, monkeypatch, fixture, bad):
    graph, record, detail, original_input = acceptance_evidence(tmp_path, monkeypatch, fixture)
    history = detail["traces"]['["worker",1]']
    if bad == "report":
        save(tmp_path / "state/artifacts/worker/files/report.json", {"wecom": "invented"})
    elif bad == "hash":
        manifest = smoke.read_json(tmp_path / "state/artifacts/worker/manifest.json")
        manifest["files"]["report.json"]["sha256"] = "0" * 64
        save(tmp_path / "state/artifacts/worker/manifest.json", manifest)
    elif bad == "workspace":
        (tmp_path / "work/run" / invocation_digest(record["results"]["worker"][0]) / "effects.txt").write_bytes(b"twice\n")
    elif bad == "producer":
        (tmp_path / "state/artifacts/publish/files/assets/panel.png").write_bytes(b"changed")
    elif bad == "input":
        record["input"]["acceptance"] = "changed"
    elif bad == "skill_read":
        history[1]["text"] = '[anchor_run]\n{"exit_code":0,"stdout":"input-readonly\\n"}'
    elif bad == "missing_call":
        history[2]["commands"] = []
    elif bad == "missing_result":
        history[3] = {"role": "assistant", "text": json.dumps(fixture.expected["wecom"])}
    elif bad == "duplicate":
        history.append(history[2])
    elif bad == "http_error":
        fixture.record({"kind": "rejected", "status": 404})
    if bad is None:
        checked = smoke.verify(tmp_path, graph, detail, record, fixture, original_input)
        assert len(checked["files"]) == 3 and len(checked["histories"]) == 1
    else:
        with pytest.raises(smoke.SmokeFailure):
            smoke.verify(tmp_path, graph, detail, record, fixture, original_input)


def test_failed_run_retains_binary_hashes_logs_and_missing_usage_without_starting_a_model(tmp_path, monkeypatch):
    binaries = package_binaries(tmp_path, monkeypatch)
    for key in smoke.MODEL_KEYS:
        monkeypatch.setenv(key, "private-model-key")
    monkeypatch.setenv("WECOM_SECRET", "production-must-not-inherit")
    monkeypatch.setenv("DOCMOST_API_KEY", "production-must-not-inherit")
    def blocked_service(_label, _command, env, _evidence):
        assert "WECOM_SECRET" not in env and "DOCMOST_API_KEY" not in env
        raise smoke.SmokeFailure("deliberate failure private-model-key")
    monkeypatch.setattr(smoke, "Service", blocked_service)
    args = SimpleNamespace(binary=binaries["host"], wecom_binary=binaries["wecom"], docmost_binary=binaries["docmost"], timeout=120)
    report = smoke.run(args, smoke.Evidence(tmp_path, ("private-model-key",)))
    assert report["status"] == "failed" and "[redacted]" in report["failure"]
    assert len(report["binary_hashes"]) == 3 and (tmp_path / "frozen-hashes.json").exists()
    assert report["provider_attempts"] == 0 and report["reported_tokens"]["total_tokens"] is None
    assert smoke.read_json(tmp_path / "evidence.json") == report
    assert "private-model-key" not in (tmp_path / "evidence.json").read_text()
    for name in ("wecom", "docmost"):
        assert (tmp_path / f"package-{name}.json").exists()


@pytest.mark.parametrize("timed_out", [False, True])
def test_failed_packaging_retains_partial_output_and_never_starts_host(tmp_path, monkeypatch, timed_out):
    binaries = package_binaries(tmp_path, monkeypatch)
    def failed_package(command, **_kwargs):
        if timed_out:
            raise smoke.subprocess.TimeoutExpired(command, 30, output=b"partial stdout", stderr=b"private-model-key")
        return SimpleNamespace(returncode=1, stdout=b"partial stdout", stderr=b"private-model-key")
    def forbidden(*_args, **_kwargs):
        pytest.fail("Failed packaging must not start Host or model")
    monkeypatch.setattr(smoke.subprocess, "run", failed_package)
    monkeypatch.setattr(smoke, "Service", forbidden)
    args = SimpleNamespace(binary=binaries["host"], wecom_binary=binaries["wecom"], docmost_binary=binaries["docmost"], timeout=120)
    report = smoke.run(args, smoke.Evidence(tmp_path, ("private-model-key",)))
    assert report["status"] == "failed" and report["provider_attempts"] == 0
    output = smoke.read_json(tmp_path / "package-wecom.json")
    assert output["stdout"] == "partial stdout" and output["stderr"] == "[redacted]"
    assert smoke.read_json(tmp_path / "evidence.json") == report


@pytest.mark.parametrize("timeout", ["0", "nan", "inf", "181", None])
def test_cli_fails_before_any_process_when_unbounded_or_model_configuration_missing(monkeypatch, tmp_path, timeout):
    for name in smoke.MODEL_KEYS:
        monkeypatch.delenv(name, raising=False)
    def forbidden(*_args, **_kwargs):
        pytest.fail("No process, binary copy or model may be started")
    monkeypatch.setattr(smoke, "run", forbidden)
    monkeypatch.setattr(smoke.subprocess, "run", forbidden)
    argv = ["smoke", "--binary", "missing-host", "--wecom-binary", "missing-wecom", "--docmost-binary", "missing-docmost", "--evidence-root", str(tmp_path)]
    monkeypatch.setattr(sys, "argv", argv + (["--timeout", timeout] if timeout else []))
    with pytest.raises(SystemExit) as error:
        smoke.main()
    assert error.value.code == 2 and not list(tmp_path.iterdir())
