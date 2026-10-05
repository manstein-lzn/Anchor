"""Service-level supervision for Plugin-declared long-lived channels."""

from __future__ import annotations

import json
import os
import secrets
import subprocess
import sys
import threading
import tempfile
from pathlib import Path
from typing import Any, Callable

from anchor.library import Library
from anchor.simple import graph as graph_module
from anchor.runtime_http import RuntimeHTTPError


class ChannelSupervisor:
    """Keep one daemon per platform/profile, outside every AgentNode sandbox."""

    def __init__(self, root: Path, library: Library, workspaces: Callable[[], list[Path]],
                 *, callback_url: str, api_key: str,
                 node_plugins: Callable[[], list[dict[str, list[str]]]] | None = None):
        self.root = Path(root).resolve()
        self.library = library
        self.workspaces = workspaces
        self.node_plugins = node_plugins
        self.callback_url = callback_url
        self.api_key = api_key
        self.processes: dict[str, subprocess.Popen] = {}
        self.specs: dict[str, dict[str, Any]] = {}
        self.controls: dict[str, tuple[Path, str]] = {}
        self._errors: set[str] = set()
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None

    def start(self) -> None:
        if self._thread is not None:
            return
        self._reconcile()
        self._thread = threading.Thread(target=self._loop, name="anchor-channel-supervisor", daemon=True)
        self._thread.start()

    def stop(self) -> None:
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout=5)
        for process in tuple(self.processes.values()):
            if process.poll() is None:
                process.terminate()
        for process in tuple(self.processes.values()):
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
        self.processes.clear()
        self.controls.clear()
        for platform in self.specs:
            (self.root / "state/channels" / platform / "control.json").unlink(missing_ok=True)

    def send(self, platform: str, payload: dict) -> dict:
        from anchor.channel.control import request
        control = self.controls.get(platform)
        process = self.processes.get(platform)
        if control is None or process is None or process.poll() is not None:
            raise RuntimeError("channel gateway is not running")
        return request(*control, payload)

    def _loop(self) -> None:
        while not self._stop.wait(1):
            self._reconcile()

    def _plugin_records(self) -> list[dict[str, list[str]]]:
        if self.node_plugins is not None:
            records = self.node_plugins()
            if (not isinstance(records, list) or any(not isinstance(record, dict) or
                    any(not isinstance(ids, list) or any(not isinstance(plugin, str) for plugin in ids)
                        for ids in record.values()) for record in records)):
                raise ValueError("invalid compiled channel Plugin projection")
            return records
        records = []
        for workspace in self.workspaces():
            try:
                graph = graph_module.load(workspace / "graph.json")
                records.append({node: list(value.plugins) for node, value in graph.nodes.items()})
            except (OSError, ValueError) as exc:
                print(json.dumps({"channel_config_error": str(exc)}, ensure_ascii=False), flush=True)
        return records

    def _desired(self) -> dict[str, dict[str, Any]]:
        found: dict[str, dict[str, Any]] = {}
        conflicts: set[str] = set()
        for record in self._plugin_records():
            try:
                for plugins in record.values():
                    for plugin_id in plugins:
                        for channel in self.library.channels(plugin_id):
                            platform = channel["platform"]
                            plugin_dir = self.library.root / "plugins" / plugin_id
                            spec = {**channel,
                                    "entrypoint_path": str(plugin_dir / channel["entrypoint"])}
                            prior = found.get(platform)
                            if prior and prior != spec:
                                conflicts.add(platform)
                            else:
                                found[platform] = spec
            except (OSError, ValueError) as exc:
                print(json.dumps({"channel_config_error": str(exc)}, ensure_ascii=False), flush=True)
        for platform in conflicts:
            found.pop(platform, None)
            if platform not in self._errors:
                print(json.dumps({"channel_conflict": platform,
                                  "error": "one platform must use the same channel Plugin and configuration"}), flush=True)
                self._errors.add(platform)
        return found

    def _reconcile(self) -> None:
        try:
            desired = self._desired()
        except (RuntimeHTTPError, OSError, ValueError) as exc:
            print(json.dumps({"channel_config_error": str(exc)}, ensure_ascii=False), flush=True)
            return
        for platform, process in tuple(self.processes.items()):
            if platform not in desired or process.poll() is not None:
                if process.poll() is None:
                    process.terminate()
                self.processes.pop(platform, None)
                self.controls.pop(platform, None)
                (self.root / "state/channels" / platform / "control.json").unlink(missing_ok=True)

        for platform, spec in desired.items():
            self.specs[platform] = spec
            if platform in self.processes:
                continue
            self._start(platform, spec)

    def _start(self, platform: str, spec: dict[str, Any]) -> None:
        required = tuple(spec.get("required_environment", ()))
        missing = [name for name in required if not os.environ.get(name, "").strip()]
        if missing:
            if platform not in self._errors:
                print(json.dumps({"channel_not_started": platform,
                                  "missing_environment": missing}), flush=True)
                self._errors.add(platform)
            return
        # Give the adapter only its declared platform credentials and Anchor callback material;
        # model keys, MCP secrets and unrelated process environment stay in the service process.
        env = {"PATH": os.environ.get("PATH", "/usr/local/bin:/usr/bin:/bin")}
        for name in required:
            env[name] = os.environ[name]
        env["ANCHOR_CHANNEL_WEBHOOK_URL"] = self.callback_url
        env["ANCHOR_API_KEY"] = self.api_key
        env["WECOM_CHANNEL_STATE"] = str(self.root / "state" / "channels" / platform)
        control_path = self.root / "state" / "channels" / platform / "control.sock"
        token = secrets.token_urlsafe(32)
        env["ANCHOR_CHANNEL_CONTROL_SOCKET"] = str(control_path)
        env["ANCHOR_CHANNEL_CONTROL_TOKEN"] = token
        env["ANCHOR_WECOM_SEND_USERS"] = (os.environ.get("ANCHOR_WECOM_SEND_USERS") or
                                          os.environ.get("ANCHOR_WECOM_USERS", ""))
        env["PYTHONPATH"] = os.pathsep.join(dict.fromkeys(
            [str(Path(__file__).resolve().parents[3] / "src"), str(Path.cwd()),
             *([env["PYTHONPATH"]] if env.get("PYTHONPATH") else [])]))
        try:
            process = subprocess.Popen([sys.executable, spec["entrypoint_path"]], cwd=str(Path.cwd()),
                                       env=env, start_new_session=True)
        except OSError as exc:
            print(json.dumps({"channel_start_error": platform, "error": str(exc)}), flush=True)
            return
        self.processes[platform] = process
        self.controls[platform] = (control_path, token)
        # Rust hosts run in a separate process and cannot inherit the gateway's
        # per-start token. Publish the same private control endpoint through a
        # mode-0600 descriptor; the Rust adapter rereads it for every send so
        # gateway restarts rotate credentials without restarting the host.
        descriptor = control_path.with_name("control.json")
        descriptor.parent.mkdir(parents=True, exist_ok=True)
        fd, temporary = tempfile.mkstemp(prefix=".control-", dir=descriptor.parent)
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as stream:
                json.dump({"socket": str(control_path), "token": token}, stream)
            os.replace(temporary, descriptor)
        finally:
            Path(temporary).unlink(missing_ok=True)
        print(json.dumps({"channel_started": platform, "plugin": spec["plugin"],
                          "pid": process.pid}), flush=True)
