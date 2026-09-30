"""File-backed Plugins and shared tools. Nodes carry references, never resource copies."""

from __future__ import annotations

import argparse
import asyncio
import fcntl
import hashlib
import json
import os
import re
import shutil
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from urllib.parse import urlparse


def reference(value: object) -> str:
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", value):
        raise ValueError(f"invalid library reference: {value!r}")
    return value


def references(value: object) -> tuple[str, ...]:
    if not isinstance(value, list):
        raise ValueError("plugins must be a list of library references")
    return tuple(dict.fromkeys(reference(item) for item in value))


def _json(path: Path) -> dict:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise ValueError(f"cannot read {path}: {exc}") from exc
    if not isinstance(value, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return value


def _inside(directory: Path, name: str) -> Path:
    path = (directory / name).resolve()
    if not path.is_relative_to(directory.resolve()) or not path.is_file():
        raise ValueError(f"resource must be a file inside {directory}: {name}")
    return path


def _digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _tree_digest(root: Path, *, contents: bool = True) -> str:
    """Source content; environment installation metadata without rereading gigabytes per pass.

    Environment metadata detects ordinary installs/edits, not hostile timestamp-preserving changes.
    Operators must not modify shared resources during an active node execution.
    """
    digest = hashlib.sha256()
    paths = sorted(root.rglob("*")) if root.is_dir() else [root]
    for path in paths:
        if any(part in {".git", "__pycache__"} for part in path.relative_to(root).parts):
            continue
        if path.is_file():
            stat = path.stat()
            identity = _digest(path) if contents else f"{stat.st_size}:{stat.st_mtime_ns}:{stat.st_ino}"
            digest.update(json.dumps([str(path.relative_to(root)), identity]).encode())
    return digest.hexdigest()


def _mount(path: Path) -> tuple[str, str]:
    # Tools are operator-managed, but broad mounts expose unrelated runs and credentials.
    absolute = Path(os.path.abspath(path))
    if not absolute.exists() or absolute.resolve() in (Path('/'), Path('/root'), Path.home(), Path('/tmp')):
        raise ValueError(f"invalid tool mount: {absolute}")
    reserved = {"workspace", "in", "plugins", "tools", "proc", "dev"}
    if absolute.parts[1] in reserved or absolute.resolve().parts[1] in reserved:
        raise ValueError(f"tool mount overlaps a reserved sandbox path: {absolute}")
    return str(absolute), str(absolute)


def _validate_mcp(server: dict, label: str) -> None:  # noqa: C901 - explicit external config validation
    stdio = "command" in server
    allowed = {"type", "enabled", "startup_timeout_sec", "tool_timeout_sec"} | (
        {"command", "args", "env", "env_vars", "optional_env_vars", "cwd"} if stdio else
        {"url", "headers", "http_headers", "env_http_headers", "bearer_token_env_var", "auth", "oauth_resource"})
    if set(server) - allowed:
        raise ValueError(f"{label}: unsupported MCP fields: {', '.join(sorted(set(server) - allowed))}")
    if not isinstance(server.get("enabled", True), bool):
        raise ValueError(f"{label}: enabled must be boolean")
    if server.get("type", "stdio" if stdio else "http") not in (("stdio",) if stdio else ("http", "sse")):
        raise ValueError(f"{label}: invalid MCP transport type")
    for key in ("command", "cwd", "url", "bearer_token_env_var", "oauth_resource"):
        if key in server and (not isinstance(server[key], str) or not server[key] or "\x00" in server[key]):
            raise ValueError(f"{label}: {key} must be a nonempty string")
    for key in ("args", "env_vars", "optional_env_vars"):
        if key in server and (not isinstance(server[key], list) or
                              not all(isinstance(v, str) and "\x00" not in v for v in server[key])):
            raise ValueError(f"{label}: {key} must be a list of strings")
    for key in ("env", "headers", "http_headers", "env_http_headers"):
        if key in server and (not isinstance(server[key], dict) or not all(
                isinstance(k, str) and isinstance(v, str) and k and "\x00" not in k + v
                for k, v in server[key].items())):
            raise ValueError(f"{label}: {key} must contain string keys and values")
    for key in ("startup_timeout_sec", "tool_timeout_sec"):
        if key in server and (type(server[key]) not in (int, float) or not 0 < server[key] <= 86400):
            raise ValueError(f"{label}: {key} must be positive seconds, at most 86400")
    if not stdio:
        url = urlparse(server.get("url", ""))
        if url.scheme not in ("http", "https") or not url.hostname or url.username or url.password or url.fragment:
            raise ValueError(f"{label}: url must be HTTP(S), without embedded credentials or fragment")
        if "auth" in server and server["auth"] != "oauth":
            raise ValueError(f"{label}: auth supports only oauth; use headers or bearer_token_env_var for tokens")
        if "headers" in server and "http_headers" in server:
            raise ValueError(f"{label}: choose headers or http_headers, not both")


@dataclass(frozen=True)
class Attached:
    records: tuple[dict, ...] = ()
    binds: tuple[tuple[str, str], ...] = ()
    mcp_servers: tuple[tuple[str, dict], ...] = ()

    @property
    def instructions(self) -> str:
        if not self.records:
            return ""
        entries = []
        for item in self.records:
            entry = f"- {item['id']}: {json.dumps(item['name'], ensure_ascii=False)} — " \
                    f"{json.dumps(item['description'], ensure_ascii=False)}. "
            entry += ("Instructions: " + ", ".join(f"/plugins/{item['id']}/{p}" for p in item['skills'])
                      if item['skills'] else "MCP tools are available by their Plugin-prefixed names.")
            entries.append(entry)
        return ("Available Plugins (read-only; read their instructions when needed):\n"
                + "\n".join(entries)
                + "\nAfter context compaction, reread the relevant Plugin instructions as needed. "
                  "Save task notes and results in /workspace, never in the Plugin library.")


class Library:
    def __init__(self, root: Path):
        self.root = Path(root).resolve()

    def install(self, source: str, plugin_id: str | None = None, *, replace_existing: bool = False) -> str:
        """Install one GitHub plugin folder, moving its source manifest to the bundle root."""
        plugins = self.root / "plugins"
        plugins.mkdir(parents=True, exist_ok=True)
        with (plugins / ".install.lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            return self._install(source, plugin_id, replace_existing=replace_existing)

    def _install(self, source: str, plugin_id: str | None, *, replace_existing: bool) -> str:
        if not isinstance(source, str):
            raise ValueError("source must be a GitHub tree URL")
        parsed = urlparse(source)
        parts = [part for part in parsed.path.split("/") if part]
        if (parsed.scheme != "https" or parsed.netloc != "github.com" or len(parts) < 5
                or parts[2] != "tree" or parsed.query or parsed.fragment):
            raise ValueError("source must be a GitHub tree URL: https://github.com/owner/repo/tree/ref/path")
        owner, repo, _, ref, *relative = parts
        if any(not re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9_.-]*", part) or part in (".", "..")
               for part in [owner, repo, ref, *relative]):
            raise ValueError("source URL must identify a plugin directory")
        name = reference(plugin_id or relative[-1])
        plugins = self.root / "plugins"
        plugins.mkdir(parents=True, exist_ok=True)
        destination = plugins / name
        if destination.exists() and not replace_existing:
            raise ValueError(f"Plugin already exists: {name}; pass replace_existing=True to update it")
        with tempfile.TemporaryDirectory(prefix=".anchor-plugin-", dir=plugins) as temporary:
            checkout = Path(temporary) / "source"
            subprocess.run(["git", "clone", "--depth=1", "--filter=blob:none", "--sparse",
                            "--branch", ref, f"https://github.com/{owner}/{repo}.git", str(checkout)],
                           check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=180)
            subprocess.run(["git", "-C", str(checkout), "sparse-checkout", "set", "--", "/".join(relative)],
                           check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60)
            source_dir = checkout.joinpath(*relative)
            if not source_dir.resolve().is_relative_to(checkout.resolve()):
                raise ValueError("source plugin directory escapes the checkout")
            source_manifest = source_dir / "plugin.json"
            if not source_manifest.is_file():
                source_manifest = source_dir / ".codex-plugin" / "plugin.json"
            if not source_manifest.is_file():
                raise ValueError("source plugin has no plugin.json manifest")
            staging = Path(temporary) / "staging"
            if any(path.is_symlink() for path in source_dir.rglob("*")):
                raise ValueError("Plugin internal symlinks are not supported")
            shutil.copytree(source_dir, staging, symlinks=True, ignore=shutil.ignore_patterns(".git"))
            shutil.copy2(source_manifest, staging / "plugin.json")
            if (staging / ".codex-plugin").exists():
                shutil.rmtree(staging / ".codex-plugin")
            # Validate with the same loader the runtime uses before replacing a working installation.
            validator_root = Path(temporary) / "validation"
            (validator_root / "plugins").mkdir(parents=True)
            (validator_root / "plugins" / name).symlink_to(staging, target_is_directory=True)
            self.__class__(validator_root).plugin(name)
            backup = Path(temporary) / "previous"
            try:
                if destination.exists():
                    destination.rename(backup)
                staging.rename(destination)
            except OSError:
                if backup.exists() and not destination.exists():
                    backup.rename(destination)
                raise
        return name

    def tool(self, tool_id: str) -> tuple[dict, tuple[tuple[str, str], ...]]:
        directory = self.root / "tools" / reference(tool_id)
        manifest = _inside(directory, "tool.json")
        spec = _json(manifest)
        raw = spec.get("entrypoint")
        if not isinstance(raw, str) or not raw:
            raise ValueError(f"tool {tool_id}: entrypoint is required")
        entry = Path(raw)
        entry = entry if entry.is_absolute() else _inside(directory, raw)
        if not entry.is_file() or not os.access(entry, os.X_OK):
            raise ValueError(f"tool {tool_id}: entrypoint is not executable: {entry}")
        binds = [(str(directory.resolve()), f"/tools/{tool_id}/files"),
                 (str(entry), f"/tools/{tool_id}/run")]
        environment = spec.get("environment")
        if environment:
            if not isinstance(environment, str) or not Path(environment).is_absolute():
                raise ValueError(f"tool {tool_id}: environment must be an absolute path")
            env = Path(environment)
            if not env.is_dir() or not entry.absolute().is_relative_to(env.absolute()):
                raise ValueError(f"tool {tool_id}: entrypoint must belong to its environment")
            binds.append(_mount(env))
            # A venv's interpreter can link to another installation, which must also be visible.
            python = env / "bin" / "python"
            seen: set[Path] = set()
            while python.is_symlink():
                if python in seen:
                    raise ValueError(f"tool {tool_id}: interpreter symlink cycle")
                seen.add(python)
                target = Path(os.readlink(python))
                python = target if target.is_absolute() else python.parent / target
                prefix = python.parent.parent if python.parent.name == "bin" else python.parent
                binds.append(_mount(prefix))
        imports = spec.get("imports", [])
        if not isinstance(imports, list) or not all(isinstance(p, str) and Path(p).is_absolute()
                                                   for p in imports):
            raise ValueError(f"tool {tool_id}: imports must be absolute paths")
        binds.extend(_mount(Path(p)) for p in imports)
        return ({"id": tool_id, "entrypoint": f"/tools/{tool_id}/run",
                 "digest": _tree_digest(directory), "executable_digest": _digest(entry),
                 "imports_digest": [_tree_digest(Path(p)) for p in imports],
                 "environment_digest": _tree_digest(Path(environment), contents=False) if environment else ""},
                tuple(dict.fromkeys(binds)))

    def plugin(self, plugin_id: str) -> tuple[dict, tuple[tuple[str, str], ...]]:
        directory = (self.root / "plugins" / reference(plugin_id)).resolve()
        manifest = _inside(directory, "plugin.json")
        spec = _json(manifest)
        name = spec.get("name")
        interface = spec.get("interface") if isinstance(spec.get("interface"), dict) else {}
        description = spec.get("description") or interface.get("longDescription") \
            or interface.get("shortDescription", "")
        if not isinstance(name, str) or not name.strip():
            raise ValueError(f"Plugin {plugin_id}: name is required")
        if not isinstance(description, str):
            raise ValueError(f"Plugin {plugin_id}: description must be a string")
        skills = spec.get("skills", "skills/" if (directory / "skills").is_dir() else [])
        skill_roots = [skills] if isinstance(skills, str) else skills
        if not isinstance(skill_roots, list) or not all(isinstance(item, str) for item in skill_roots):
            raise ValueError(f"Plugin {plugin_id}: skills must be a path or list of paths")
        skill_paths = []
        for item in skill_roots:
            path = (directory / item).resolve()
            if not path.is_relative_to(directory) or not path.is_dir():
                raise ValueError(f"Plugin {plugin_id}: skill path is outside the bundle or not a directory: {item}")
            skill_paths.extend(str(skill.relative_to(directory)) for skill in sorted(path.rglob("SKILL.md")))
        legacy_instructions = directory / "instructions.md"
        if legacy_instructions.is_file():
            skill_paths.append("instructions.md")
        # Supplemental documents are mounted too; do not let an internal symlink escape the Plugin.
        files = sorted(directory.rglob("*"))
        if any(p.is_symlink() for p in files):
            raise ValueError(f"Plugin {plugin_id}: internal symlinks are not supported")
        digest = hashlib.sha256()
        for path in files:
            if path.is_file():
                digest.update(str(path.relative_to(directory)).encode())
                digest.update(bytes.fromhex(_digest(path)))
        mcp_servers = self.mcp_servers(plugin_id, resolve_env=False)
        channels = list(self.channels(plugin_id))
        record = {"id": plugin_id, "name": name, "description": description,
                  "skills": list(dict.fromkeys(skill_paths)),
                  "unsupported": [key for key in ("hooks", "commands", "agents", "apps")
                                  if spec.get(key) or (directory / key).exists()
                                  or (key == "apps" and (directory / ".app.json").exists())],
                  "mcpServers": {server_name: {"transport": "stdio" if "command" in server else
                                                server.get("type", "http"),
                                                **({"auth": "oauth"} if server.get("auth") == "oauth"
                                                   or server.get("oauth_resource") else {})}
                                                 for server_name, server in mcp_servers},
                  "channels": channels,
                  "digest": digest.hexdigest()}
        binds = [(str(directory), f"/plugins/{plugin_id}")]
        return record, tuple(dict.fromkeys(binds))

    def channels(self, plugin_id: str) -> tuple[dict, ...]:
        """Return validated long-lived channel declarations bundled by a Plugin.

        A channel declaration is deliberately separate from MCP: MCP is a node capability, while
        this entrypoint is a service-level connection supervised by Anchor.  The declaration is
        metadata only; credentials are resolved from the service environment when the supervisor
        starts the process.
        """
        plugin_dir = (self.root / "plugins" / reference(plugin_id)).resolve()
        manifest = plugin_dir / "channel.json"
        if not manifest.exists():
            return ()
        spec = _json(manifest)
        platform = spec.get("platform")
        transport = spec.get("transport")
        entrypoint = spec.get("entrypoint")
        if not isinstance(platform, str) or not re.fullmatch(r"[A-Za-z][A-Za-z0-9_.-]*", platform):
            raise ValueError(f"Plugin {plugin_id}: channel platform is required")
        if transport != "websocket":
            raise ValueError(f"Plugin {plugin_id}: only websocket channels are supported")
        if not isinstance(entrypoint, str) or not entrypoint:
            raise ValueError(f"Plugin {plugin_id}: channel entrypoint is required")
        _inside(plugin_dir, entrypoint)
        required = spec.get("required_environment", [])
        if not isinstance(required, list) or not all(
                isinstance(item, str) and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", item)
                for item in required):
            raise ValueError(f"Plugin {plugin_id}: required_environment must be a list of names")
        description = spec.get("description", "")
        if not isinstance(description, str):
            raise ValueError(f"Plugin {plugin_id}: channel description must be a string")
        sdk = spec.get("sdk", "")
        if not isinstance(sdk, str):
            raise ValueError(f"Plugin {plugin_id}: channel sdk must be a string")
        return ({"plugin": plugin_id, "platform": platform, "transport": transport,
                 "entrypoint": entrypoint, "required_environment": required,
                 "description": description, "sdk": sdk},)

    def attach(self, ids: tuple[str, ...]) -> Attached:
        records: list[dict] = []
        binds: list[tuple[str, str]] = []
        mcp_servers: dict[str, dict] = {}
        for plugin_id in ids:
            record, mounts = self.plugin(plugin_id)
            records.append(record)
            binds.extend(mounts)
            for name, value in self.mcp_servers(plugin_id):
                key = f"{plugin_id}-{name}"
                if key in mcp_servers:
                    raise ValueError(f"duplicate MCP server: {key}")
                mcp_servers[key] = value
        return Attached(tuple(records), tuple(dict.fromkeys(binds)), tuple(mcp_servers.items()))

    def mcp_servers(self, plugin_id: str, *, resolve_env: bool = True) -> tuple[tuple[str, dict], ...]:
        plugin_dir = (self.root / "plugins" / reference(plugin_id)).resolve()
        config_path = plugin_dir / ".mcp.json"
        servers = _json(config_path).get("mcpServers", {}) if config_path.exists() else {}
        manifest = _json(plugin_dir / "plugin.json")
        declared = manifest.get("mcpServers", {})
        if isinstance(declared, str):
            declared = _json(_inside(plugin_dir, declared)).get("mcpServers", {})
        if not isinstance(servers, dict) or not isinstance(declared, dict):
            raise ValueError(f"Plugin {plugin_id}: mcpServers must be an object or config path")
        result = []
        variable = re.compile(r"\$\{([A-Za-z_][A-Za-z0-9_]*)(?::-([^}]*))?\}")

        def environment(key: str) -> str:
            if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", key):
                raise ValueError(f"Plugin {plugin_id}: invalid MCP environment variable name")
            value = os.environ.get(key)
            if value is None:
                raise ValueError(f"Plugin {plugin_id}: MCP environment variable {key} is not set")
            return value

        def expand(item: object) -> object:
            if isinstance(item, str):
                def replace(match: re.Match[str]) -> str:
                    key = match.group(1)
                    if key in ("CODEX_PLUGIN_ROOT", "CLAUDE_PLUGIN_ROOT", "PLUGIN_ROOT"):
                        return str(plugin_dir)
                    if match.group(2) is not None:
                        return os.environ.get(key) or match.group(2)
                    return environment(key)
                return variable.sub(replace, item)
            if isinstance(item, list):
                return [expand(value) for value in item]
            if isinstance(item, dict):
                return {key: expand(value) for key, value in item.items()}
            return item

        for name, value in {**servers, **declared}.items():
            server = self._mcp_server(plugin_id, name, value, plugin_dir, resolve_env, expand, environment)
            if server is not None:
                result.append((name, server))
        return tuple(result)

    def _mcp_server(self, plugin_id: str, name: str, value: object, plugin_dir: Path, resolve_env: bool,
                    expand: Any, environment: Any) -> dict | None:
        reference(name)
        if not isinstance(value, dict):
            raise ValueError(f"Plugin {plugin_id}: MCP server {name} must be an object")
        _validate_mcp(value, f"Plugin {plugin_id}, server {name}")
        if value.get("enabled") is False:
            return None
        server = expand(value) if resolve_env else dict(value)
        if resolve_env and "command" in server:
            executable = Path(server["command"])
            if executable.is_absolute() and not executable.is_relative_to(plugin_dir) and not any(
                    executable.is_relative_to(Path(path))
                    for path in ("/usr", "/bin", "/lib", "/lib64", "/sbin")):
                raise ValueError(f"Plugin {plugin_id}: MCP executable is outside system paths")
            if executable.is_absolute() and executable.is_relative_to(plugin_dir) and (
                    not executable.is_file() or not os.access(executable, os.X_OK)):
                raise ValueError(f"Plugin {plugin_id}: MCP command is not an executable Plugin file")
        if resolve_env and "command" in server and any(
                not os.environ.get(key, "").strip() for key in server.get("optional_env_vars", [])):
            return None
        if isinstance(server.get("cwd"), str) and not Path(server["cwd"]).is_absolute():
            server["cwd"] = str((plugin_dir / server["cwd"]).resolve())
            if not Path(server["cwd"]).is_relative_to(plugin_dir):
                raise ValueError(f"Plugin {plugin_id}: MCP cwd escapes the Plugin bundle")
        if resolve_env:
            self._resolve_mcp_env(plugin_id, name, server, environment)
            server.update(_anchor_plugin_id=plugin_id, _anchor_plugin_dir=str(plugin_dir),
                          _anchor_server_name=name,
                          _anchor_auth_dir=str(self.root.parent / "state" / "mcp-auth" / plugin_id / name))
        return server

    @staticmethod
    def _resolve_mcp_env(plugin_id: str, name: str, server: dict, environment: Any) -> None:
        if "command" in server:
            names = (*server.get("env_vars", []), *server.get("optional_env_vars", []))
            server["env"] = {**{key: environment(key) for key in names},
                             **server.get("env", {})}
            return
        headers = {**server.get("http_headers", {}), **server.get("headers", {}),
                   **{key: environment(value) for key, value in server.get("env_http_headers", {}).items()}}
        token_var = server.get("bearer_token_env_var")
        if token_var:
            if any(key.lower() == "authorization" for key in headers):
                raise ValueError(f"Plugin {plugin_id}: duplicate MCP authorization configuration")
            headers["Authorization"] = "Bearer " + environment(token_var)
        server["headers"] = headers

    @staticmethod
    def _skill_body(path: Path) -> str:
        text = path.read_text(encoding="utf-8")
        if text.startswith("---\n"):
            _, _, rest = text.partition("\n---\n")
            return rest
        return text

    def catalog(self) -> list[dict]:
        base = self.root / "plugins"
        result = []
        for directory in sorted(base.iterdir()) if base.is_dir() else []:
            if directory.name.startswith('.'):
                continue
            try:
                record, _ = self.plugin(directory.name)
                result.append({**record, "available": True})
            except (ValueError, OSError) as exc:
                result.append({"id": directory.name, "name": directory.name, "description": "",
                               "skills": [], "mcpServers": {}, "available": False, "error": str(exc)})
        return result

    def detail(self, plugin_id: str) -> dict:
        record, _ = self.plugin(plugin_id)
        directory = (self.root / "plugins" / reference(plugin_id)).resolve()
        skill_files = [directory / skill for skill in record["skills"]]
        legacy = directory / "instructions.md"
        content = (legacy.read_text(encoding="utf-8") if legacy.is_file() else
                   "\n\n".join(self._skill_body(path) for path in skill_files if path.is_file()))
        return {**record, "available": True, "instructions": content}

    def file(self, plugin_id: str, name: str) -> Path:
        directory = (self.root / "plugins" / reference(plugin_id)).resolve()
        # `plugin()` is the single manifest/layout authority for detail, runtime, and file access.
        record, _ = self.plugin(plugin_id)
        if name == "instructions.md" and name not in record["skills"]:
            skill_files = [directory / skill for skill in record["skills"]]
            content = "\n\n".join(self._skill_body(path) for path in skill_files if path.is_file())
            if content:
                raise ValueError("Plugin uses skills/<skill>/SKILL.md; read its content from "
                                 "the Plugin detail endpoint")
        return _inside(directory, name)


def for_workspace(workspace: Path, root: Path | None = None) -> Library:
    if root is not None:
        return Library(root)
    workspace = workspace.resolve()
    parent = workspace.parent.parent if workspace.parent.name == "workspaces" else workspace.parent
    return Library(parent / "library")


def record_bindings(run_dir: Path, bindings: dict[str, Attached], *, resume: bool) -> None:
    """Record resolved identities, not copies. Refuse changing a resumed run's resources."""
    path = run_dir / "plugins.json"
    current = {node: list(binding.records) for node, binding in bindings.items() if binding.records}
    if path.exists():
        if _json(path) != current:
            raise ValueError("Plugin resources changed since this run started; start a new run")
    elif current:
        if resume:
            raise ValueError("this run has no Plugin binding record; start a new run")
        path.write_text(json.dumps(current, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser(description="Manage Anchor's file-backed Plugin library")
    parser.add_argument("--root", type=Path, required=True, help="Anchor data root (contains library/)")
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("list")
    check = subparsers.add_parser("check")
    check.add_argument("plugin")
    authorize = subparsers.add_parser("authorize")
    authorize.add_argument("plugin")
    authorize.add_argument("server")
    install = subparsers.add_parser("install")
    install.add_argument("source", help="GitHub plugin folder URL")
    install.add_argument("--id", help="installed Plugin id; defaults to source folder name")
    install.add_argument("--replace", action="store_true", help="atomically replace an existing installation")
    args = parser.parse_args()
    library = Library(args.root / "library")
    try:
        if args.command == "install":
            result: dict | list[dict] | str = library.install(args.source, args.id,
                                                                replace_existing=args.replace)
        elif args.command == "check":
            result = library.detail(args.plugin)
        elif args.command == "authorize":
            from anchor.node.mcp import http_toolset

            async def authorize_server() -> dict:
                server = dict(library.mcp_servers(args.plugin))[args.server]
                toolset = http_toolset(args.server, server, interactive=True)
                async with toolset:
                    return {"authorized": True, "tools": sorted(await toolset.get_tools())}

            result = asyncio.run(authorize_server())
        else:
            result = library.catalog()
    except (ValueError, OSError, subprocess.SubprocessError) as exc:
        parser.exit(1, f"{exc}\n")
    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
