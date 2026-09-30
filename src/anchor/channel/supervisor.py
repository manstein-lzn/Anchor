"""Service-level supervision for Plugin-declared long-lived channels."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import threading
from pathlib import Path
from typing import Any, Callable

from anchor.library import Library
from anchor.simple import graph as graph_module


class ChannelSupervisor:
    """Keep one daemon per platform/profile, outside every AgentNode sandbox."""

    def __init__(self, root: Path, library: Library, workspaces: Callable[[], list[Path]],
                 *, callback_url: str, api_key: str):
        self.root = Path(root).resolve()
        self.library = library
        self.workspaces = workspaces
        self.callback_url = callback_url
        self.api_key = api_key
        self.processes: dict[str, subprocess.Popen] = {}
        self.specs: dict[str, dict[str, Any]] = {}
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

    def _loop(self) -> None:
        while not self._stop.wait(1):
            self._reconcile()

    def _desired(self) -> dict[str, dict[str, Any]]:
        found: dict[str, dict[str, Any]] = {}
        conflicts: set[str] = set()
        for workspace in self.workspaces():
            try:
                graph = graph_module.load(workspace / "graph.json")
                for node in graph.nodes.values():
                    for plugin_id in node.plugins:
                        for channel in self.library.channels(plugin_id):
                            platform = channel["platform"]
                            plugin_dir = self.library.root / "plugins" / plugin_id
                            spec = {**channel, "graph": workspace.name,
                                    "entrypoint_path": str(plugin_dir / channel["entrypoint"])}
                            prior = found.get(platform)
                            if prior and (prior["plugin"] != plugin_id or prior["graph"] != workspace.name):
                                conflicts.add(platform)
                            else:
                                found[platform] = spec
            except (OSError, ValueError) as exc:
                print(json.dumps({"channel_config_error": str(exc)}, ensure_ascii=False), flush=True)
        for platform in conflicts:
            found.pop(platform, None)
            if platform not in self._errors:
                print(json.dumps({"channel_conflict": platform,
                                  "error": "one platform channel may be mounted by only one Graph"}), flush=True)
                self._errors.add(platform)
        return found

    def _reconcile(self) -> None:
        desired = self._desired()
        for platform, process in tuple(self.processes.items()):
            if platform not in desired or process.poll() is not None:
                if process.poll() is None:
                    process.terminate()
                self.processes.pop(platform, None)
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
        print(json.dumps({"channel_started": platform, "plugin": spec["plugin"],
                          "graph": spec["graph"], "pid": process.pid}), flush=True)
