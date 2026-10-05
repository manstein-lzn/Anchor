#!/usr/bin/env python3
"""Build a deterministic, source-free Rust Graph Runtime distribution.

This is a build-time helper. The resulting archive contains only the release
binary, an admitted format-1 Graph bundle, and deployment instructions.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import tarfile
import tempfile
from pathlib import Path


PLUGIN_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]*$")
REQUIRED_MANIFEST_KEYS = {"format", "graph", "plugins"}
REQUIRED_PLUGIN_KEYS = {"id", "digest", "resources", "mcp_servers"}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def reject_symlinks(root: Path) -> None:
    for path in [root, *sorted(root.rglob("*"))]:
        if path.is_symlink():
            raise ValueError(f"Graph bundle contains a symlink: {path.relative_to(root)}")


def validate_plugins(plugins: object) -> list[str]:
    if not isinstance(plugins, list):
        raise ValueError("manifest.plugins must be an array")
    ids: list[str] = []
    for plugin in plugins:
        if not isinstance(plugin, dict) or set(plugin) != REQUIRED_PLUGIN_KEYS:
            raise ValueError("manifest Plugin entries have unknown or missing fields")
        plugin_id = plugin["id"]
        if not isinstance(plugin_id, str) or not PLUGIN_ID.fullmatch(plugin_id):
            raise ValueError(f"invalid Plugin id: {plugin_id!r}")
        if plugin_id in ids:
            raise ValueError(f"duplicate Plugin id: {plugin_id}")
        ids.append(plugin_id)
        if not isinstance(plugin["digest"], str) or not plugin["digest"]:
            raise ValueError(f"Plugin {plugin_id} has no digest")
        resources = plugin["resources"]
        mcp_servers = plugin["mcp_servers"]
        if not isinstance(resources, list) or not all(isinstance(item, str) for item in resources):
            raise ValueError(f"Plugin {plugin_id} resources must be strings")
        if not isinstance(mcp_servers, list) or not all(isinstance(item, str) for item in mcp_servers):
            raise ValueError(f"Plugin {plugin_id} mcp_servers must be strings")
    return ids


def load_manifest(bundle: Path) -> dict:
    reject_symlinks(bundle)
    manifest_path = bundle / "manifest.json"
    graph_path = bundle / "graph.json"
    if not manifest_path.is_file() or not graph_path.is_file():
        raise ValueError("Graph bundle must contain manifest.json and graph.json")
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ValueError(f"invalid manifest.json: {exc}") from exc
    if not isinstance(manifest, dict):
        raise ValueError("manifest.json root must be an object")
    if set(manifest) != REQUIRED_MANIFEST_KEYS:
        raise ValueError("manifest.json has unknown or missing fields")
    if manifest["format"] != 1 or manifest["graph"] != "graph.json":
        raise ValueError("only format-1 graph.json bundles are supported")
    ids = validate_plugins(manifest["plugins"])

    allowed = {"manifest.json", "graph.json"}
    if ids:
        allowed.add("plugins")
        plugin_root = bundle / "plugins"
        if not plugin_root.is_dir():
            raise ValueError("manifest declares Plugins but bundle has no plugins directory")
        actual_ids = sorted(item.name for item in plugin_root.iterdir())
        if actual_ids != sorted(ids):
            raise ValueError("bundle Plugin directories do not match manifest")
    actual_top = {item.name for item in bundle.iterdir()}
    if actual_top != allowed:
        raise ValueError("bundle contains undeclared top-level resources")
    return manifest


def copy_tree(source: Path, target: Path) -> None:
    for path in sorted(source.rglob("*")):
        relative = path.relative_to(source)
        destination = target / relative
        if path.is_dir():
            destination.mkdir(parents=True, exist_ok=True)
        elif path.is_file():
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, destination)
        else:
            raise ValueError(f"unsupported bundle entry: {relative}")


def add_deterministic(tar: tarfile.TarFile, path: Path, arcname: str) -> None:
    info = tar.gettarinfo(str(path), arcname=arcname)
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    info.mtime = 0
    if path.is_file():
        with path.open("rb") as stream:
            tar.addfile(info, stream)
    else:
        tar.addfile(info)


def write_archive(staging: Path, output: Path) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("wb") as raw:
        import gzip

        with gzip.GzipFile(fileobj=raw, mode="wb", filename="", mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as tar:
                entries = sorted(staging.rglob("*"), key=lambda path: path.relative_to(staging).as_posix())
                for path in entries:
                    relative = path.relative_to(staging).as_posix()
                    add_deterministic(tar, path, f"anchor-runtime/{relative}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True, help="release anchor-runner-host binary")
    parser.add_argument("--bundle", type=Path, required=True, help="format-1 Graph bundle directory")
    parser.add_argument("--output", type=Path, required=True, help="output .tar.gz path")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    binary = args.binary.resolve()
    bundle = args.bundle.resolve()
    output = args.output.resolve()
    if not binary.is_file() or binary.is_symlink():
        raise SystemExit("--binary must name a regular file")
    if binary.read_bytes()[:4] != b"\x7fELF":
        raise SystemExit("--binary must be an ELF executable")
    if not bundle.is_dir():
        raise SystemExit("--bundle must name a directory")
    try:
        manifest = load_manifest(bundle)
    except ValueError as exc:
        raise SystemExit(str(exc)) from exc

    with tempfile.TemporaryDirectory(prefix="anchor-runtime-package-") as temporary:
        staging = Path(temporary) / "anchor-runtime"
        (staging / "bin").mkdir(parents=True)
        shutil.copy2(binary, staging / "bin/anchor-runner-host")
        copy_tree(bundle, staging / "bundle")
        (staging / "README.md").write_text(
            "# Anchor Rust Runtime\n\n"
            "This package contains the Rust runtime and one admitted Graph bundle.\n\n"
            "Required deployment environment:\n\n"
            "```sh\n"
            "export ANCHOR_RUNNER_BUNDLE_ROOT=\"$PWD/bundle\"\n"
            "export ANCHOR_RUNNER_STATE_ROOT=\"$PWD/state\"\n"
            "export ANCHOR_RUNNER_WORKSPACE_ROOT=\"$PWD/workspaces\"\n"
            "export ANCHOR_RUNNER_ALLOWED_COMMANDS=\"sh\"\n"
            "./bin/anchor-runner-host\n"
            "```\n\n"
            "The host requires a Linux runtime with Bubblewrap, a POSIX shell, and Git\n"
            "for immutable draft views used by existing Graphs. Credentials and model\n"
            "configuration are deployment inputs and are not included in this archive.\n\n"
            "Optional ANCHOR_MODEL_ALIASES maps models.* references to model names on\n"
            "the configured endpoint. For operator local inputs, set ANCHOR_RUNNER_GRAPH_NAME\n"
            "and ANCHOR_RUNNER_LOCAL_INPUTS_ROOT; the host reads\n"
            "<local-inputs-root>/<graph-name>/local-inputs.json. Keep these grants outside\n"
            "the bundle and archive. Plugin tool environments remain deployment inputs.\n",
            encoding="utf-8",
        )
        inventory = {
            "format": 1,
            "binary": {
                "path": "bin/anchor-runner-host",
                "sha256": sha256(staging / "bin/anchor-runner-host"),
            },
            "bundle": {
                "path": "bundle",
                "manifest_sha256": sha256(staging / "bundle/manifest.json"),
                "graph_sha256": sha256(staging / "bundle/graph.json"),
                "plugins": [plugin["id"] for plugin in manifest["plugins"]],
            },
        }
        (staging / "runtime-manifest.json").write_text(
            json.dumps(inventory, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
        write_archive(staging, output)

    print(json.dumps({"output": str(output), "sha256": sha256(output)}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
