"""File-backed Plugins and shared tools. Nodes carry references, never resource copies."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
from dataclasses import dataclass
from pathlib import Path


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


@dataclass(frozen=True)
class Attached:
    records: tuple[dict, ...] = ()
    binds: tuple[tuple[str, str], ...] = ()

    @property
    def instructions(self) -> str:
        if not self.records:
            return ""
        entries = [f"- {item['id']}: {json.dumps(item['name'], ensure_ascii=False)} — "
                   f"{json.dumps(item['description'], ensure_ascii=False)}. "
                   f"Read /plugins/{item['id']}/instructions.md when relevant."
                   for item in self.records]
        return ("Available Plugins (read-only; read their instructions when needed):\n"
                + "\n".join(entries)
                + "\nAfter context compaction, reread the relevant Plugin instructions as needed. "
                  "Save task notes and results in /workspace, never in the Plugin library.")


class Library:
    def __init__(self, root: Path):
        self.root = Path(root).resolve()

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
        for field in ("name", "description"):
            if not isinstance(spec.get(field), str) or not spec[field].strip():
                raise ValueError(f"Plugin {plugin_id}: {field} is required")
        _inside(directory, "instructions.md")
        # Supplemental documents are mounted too; do not let an internal symlink escape the Plugin.
        files = sorted(directory.rglob("*"))
        if any(p.is_symlink() for p in files):
            raise ValueError(f"Plugin {plugin_id}: internal symlinks are not supported")
        digest = hashlib.sha256()
        for path in files:
            if path.is_file():
                digest.update(str(path.relative_to(directory)).encode())
                digest.update(bytes.fromhex(_digest(path)))
        binds = [(str(directory), f"/plugins/{plugin_id}")]
        tools = []
        for tool_id in references(spec.get("tools", [])):
            tool, mounts = self.tool(tool_id)
            tools.append(tool)
            binds.extend(mounts)
        return ({"id": plugin_id, "name": spec["name"], "description": spec["description"],
                 "tools": tools, "digest": digest.hexdigest()}, tuple(dict.fromkeys(binds)))

    def attach(self, ids: tuple[str, ...]) -> Attached:
        records: list[dict] = []
        binds: list[tuple[str, str]] = []
        for plugin_id in ids:
            record, mounts = self.plugin(plugin_id)
            records.append(record)
            binds.extend(mounts)
        return Attached(tuple(records), tuple(dict.fromkeys(binds)))

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
                               "tools": [], "available": False, "error": str(exc)})
        return result

    def detail(self, plugin_id: str) -> dict:
        record, _ = self.plugin(plugin_id)
        instructions = _inside(self.root / "plugins" / plugin_id, "instructions.md")
        return {**record, "available": True, "instructions": instructions.read_text(encoding="utf-8")}

    def file(self, plugin_id: str, name: str) -> Path:
        return _inside(self.root / "plugins" / reference(plugin_id), name)


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
    parser = argparse.ArgumentParser(description="Inspect Anchor's file-backed Plugin library")
    parser.add_argument("--root", type=Path, required=True, help="Anchor data root (contains library/)")
    parser.add_argument("command", choices=["list", "check"])
    parser.add_argument("plugin", nargs="?")
    args = parser.parse_args()
    library = Library(args.root / "library")
    result: dict | list[dict]
    if args.command == "check":
        if not args.plugin:
            parser.error("check requires a Plugin id")
        try:
            result = library.detail(args.plugin)
        except (ValueError, OSError) as exc:
            parser.exit(1, f"{exc}\n")
    else:
        result = library.catalog()
    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
