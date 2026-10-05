"""Service exit must stop the detached channel gateway and release its signal handler."""

from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock

import pytest

import anchor.serve as service


HOST = """
import json
import signal
import sys
import threading
from pathlib import Path

import anchor.serve as service

root = Path(sys.argv[1])
previous_sigterm = signal.getsignal(signal.SIGTERM)

class ObservedServer(service.ThreadingHTTPServer):
    def serve_forever(self, *args, **kwargs):
        self.serving_thread = threading.get_ident()
        marker = root / 'http-ready.tmp'
        marker.write_text(json.dumps({'thread': self.serving_thread}))
        marker.replace(root / 'http-ready.json')
        super().serve_forever(*args, **kwargs)

    def shutdown(self):
        (root / 'http-shutdown.json').write_text(json.dumps({
            'serving_thread': self.serving_thread,
            'shutdown_thread': threading.get_ident(),
        }))
        super().shutdown()

service.ThreadingHTTPServer = ObservedServer
service.serve(root, root / 'runtime.json', port=0)
assert signal.getsignal(signal.SIGTERM) == previous_sigterm
(root / 'host-stopped').write_text('restored')
"""

GATEWAY = """
import json
import os
import signal
import time
from pathlib import Path

state = Path(os.environ['WECOM_CHANNEL_STATE'])
state.mkdir(parents=True, exist_ok=True)

def stop(signum, frame):
    (state / 'gateway-stopped').write_text(str(signum))
    raise SystemExit(0)

signal.signal(signal.SIGTERM, stop)
marker = state / 'gateway-ready.tmp'
marker.write_text(json.dumps({
    'pid': os.getpid(), 'parent': os.getppid(), 'session': os.getsid(0),
}))
marker.replace(state / 'gateway-ready.json')
while True:
    time.sleep(0.1)
"""


def _wait_for(path: Path, process: subprocess.Popen, log: Path) -> None:
    deadline = time.monotonic() + 8
    while not path.exists() and time.monotonic() < deadline:
        assert process.poll() is None, log.read_text()
        time.sleep(0.02)
    assert path.exists(), log.read_text()


def test_sigterm_stops_host_and_detached_gateway(tmp_path):
    plugin = tmp_path / "library/plugins/local-gateway"
    plugin.mkdir(parents=True)
    (plugin / "plugin.json").write_text(json.dumps({"name": "Local gateway", "description": "test"}))
    (plugin / "channel.json").write_text(json.dumps({
        "platform": "wecom", "transport": "websocket", "entrypoint": "gateway.py",
        "required_environment": [],
    }))
    (plugin / "gateway.py").write_text(GATEWAY)
    workspace = tmp_path / "workspaces/assistant"
    workspace.mkdir(parents=True)
    (workspace / "graph.json").write_text(json.dumps({
        "objective": "local shutdown fixture", "agents": {"assistant": {"model": "models.default"}},
        "nodes": [{"id": "assistant", "agent": "assistant", "plugins": ["local-gateway"]}],
        "edges": [],
    }))
    (tmp_path / "runtime.json").write_text('{"models": []}')
    key = "local-shutdown-fixture-" + "k" * 32
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "PYTHONPATH": str(Path(__file__).resolve().parents[1] / "src"),
        "ANCHOR_API_KEYS": json.dumps([key]),
        "ANCHOR_API_KEY": key,
    }
    state = tmp_path / "state/channels/wecom"
    gateway_ready = state / "gateway-ready.json"
    log = tmp_path / "host.log"
    with log.open("w") as output:
        host = subprocess.Popen([sys.executable, "-c", HOST, str(tmp_path)], env=env,
                                stdout=output, stderr=subprocess.STDOUT, start_new_session=True)
    try:
        _wait_for(tmp_path / "http-ready.json", host, log)
        _wait_for(gateway_ready, host, log)
        gateway = json.loads(gateway_ready.read_text())
        assert gateway["parent"] == host.pid
        assert gateway["session"] == gateway["pid"]
        assert gateway["session"] != os.getsid(host.pid)
        assert (state / "control.json").exists()

        host.send_signal(signal.SIGTERM)
        assert host.wait(timeout=8) == 0, log.read_text()
        assert (state / "gateway-stopped").read_text() == str(signal.SIGTERM)
        with pytest.raises(ProcessLookupError):
            os.kill(gateway["pid"], 0)
        assert not (state / "control.json").exists()
        assert (tmp_path / "host-stopped").read_text() == "restored"
        shutdown = json.loads((tmp_path / "http-shutdown.json").read_text())
        assert shutdown["shutdown_thread"] != shutdown["serving_thread"]
    finally:
        if host.poll() is None:
            host.kill()
        host.wait(timeout=8)
        if gateway_ready.exists():
            try:
                os.kill(json.loads(gateway_ready.read_text())["pid"], signal.SIGKILL)
            except ProcessLookupError:
                pass


def _stub_service(tmp_path, monkeypatch):
    scheduler = SimpleNamespace(
        root=tmp_path, runtime=None, api_keys=(), workspaces=lambda: [],
        start_channels=Mock(), resume_all=Mock(), tick_schedules=Mock(), stop_channels=Mock(),
    )
    server = SimpleNamespace(server_port=1234, serve_forever=Mock(), shutdown=Mock(), server_close=Mock())
    monkeypatch.setattr(service, "Scheduler", lambda *_args: scheduler)
    monkeypatch.setattr(service, "ThreadingHTTPServer", lambda *_args: server)
    return scheduler, server


@pytest.mark.parametrize("failure", [None, "startup", "stop", "close"])
def test_restores_sigterm_handler_and_closes_server_after_failure(tmp_path, monkeypatch, failure):
    scheduler, server = _stub_service(tmp_path, monkeypatch)
    previous = signal.getsignal(signal.SIGTERM)

    def custom_sigterm(_signum, _frame):
        pass

    def start_channels(_url):
        assert signal.getsignal(signal.SIGTERM) is not custom_sigterm
        if failure == "startup":
            raise RuntimeError("startup")

    scheduler.start_channels.side_effect = start_channels
    if failure == "stop":
        scheduler.stop_channels.side_effect = RuntimeError("stop")
    if failure == "close":
        server.server_close.side_effect = RuntimeError("close")
    signal.signal(signal.SIGTERM, custom_sigterm)
    try:
        if failure is None:
            service.serve(tmp_path, tmp_path / "runtime.json", port=0)
        else:
            with pytest.raises(RuntimeError, match=failure):
                service.serve(tmp_path, tmp_path / "runtime.json", port=0)
        assert signal.getsignal(signal.SIGTERM) is custom_sigterm
        scheduler.stop_channels.assert_called_once()
        server.server_close.assert_called_once()
    finally:
        signal.signal(signal.SIGTERM, previous)


def test_serve_in_worker_thread_does_not_modify_signal_handlers(tmp_path, monkeypatch):
    scheduler, server = _stub_service(tmp_path, monkeypatch)
    get_signal = Mock(side_effect=AssertionError("worker must not inspect process signal handlers"))
    set_signal = Mock(side_effect=AssertionError("worker must not replace process signal handlers"))
    monkeypatch.setattr(service.signal, "getsignal", get_signal)
    monkeypatch.setattr(service.signal, "signal", set_signal)
    errors = []

    def run():
        try:
            service.serve(tmp_path, tmp_path / "runtime.json", port=0)
        except Exception as exc:  # noqa: BLE001 - propagate a worker failure to the test thread
            errors.append(exc)

    worker = threading.Thread(target=run)
    worker.start()
    worker.join(timeout=3)
    assert not worker.is_alive()
    assert errors == []
    get_signal.assert_not_called()
    set_signal.assert_not_called()
    scheduler.stop_channels.assert_called_once()
    server.server_close.assert_called_once()
