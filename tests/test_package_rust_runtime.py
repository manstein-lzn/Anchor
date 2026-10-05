import hashlib
import json
import subprocess
import sys
import tarfile
from pathlib import Path


SCRIPT = Path(__file__).parents[1] / "scripts" / "package_rust_runtime.py"


def write_bundle(root: Path, plugins: list[dict] | None = None) -> Path:
    bundle = root / "bundle"
    bundle.mkdir(parents=True)
    (bundle / "graph.json").write_text('{"entry":"node"}\n', encoding="utf-8")
    (bundle / "manifest.json").write_text(
        json.dumps({"format": 1, "graph": "graph.json", "plugins": plugins or []}),
        encoding="utf-8",
    )
    return bundle


def write_binary(root: Path, name: str = "anchor-runner-host") -> Path:
    binary = root / name
    binary.write_bytes(b"\x7fELF" + b"runtime")
    return binary


def package(binary: Path, bundle: Path, output: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPT), "--binary", str(binary), "--bundle", str(bundle), "--output", str(output)],
        check=False,
        capture_output=True,
        text=True,
    )


def test_package_is_deterministic_and_source_free(tmp_path):
    bundle = write_bundle(tmp_path)
    binary = write_binary(tmp_path)
    first = tmp_path / "first.tar.gz"
    second = tmp_path / "second.tar.gz"

    assert package(binary, bundle, first).returncode == 0
    assert package(binary, bundle, second).returncode == 0
    assert hashlib.sha256(first.read_bytes()).digest() == hashlib.sha256(second.read_bytes()).digest()

    with tarfile.open(first, "r:gz") as archive:
        names = archive.getnames()
    assert names == sorted(names)
    assert "anchor-runtime/bin/anchor-runner-host" in names
    assert "anchor-runtime/bundle/manifest.json" in names
    assert all(not name.endswith(".py") for name in names)


def test_package_rejects_symlink_and_unknown_top_level_entry(tmp_path):
    binary = write_binary(tmp_path)
    bundle = write_bundle(tmp_path)
    (bundle / "escape").symlink_to("/tmp")
    result = package(binary, bundle, tmp_path / "symlink.tar.gz")
    assert result.returncode != 0
    assert "symlink" in result.stderr

    bundle = write_bundle(tmp_path / "unknown")
    (bundle / "secret.txt").write_text("no", encoding="utf-8")
    result = package(binary, bundle, tmp_path / "unknown.tar.gz")
    assert result.returncode != 0
    assert "undeclared top-level" in result.stderr

    bundle = write_bundle(tmp_path / "manifest-root")
    (bundle / "manifest.json").write_text("[]", encoding="utf-8")
    result = package(binary, bundle, tmp_path / "manifest-root.tar.gz")
    assert result.returncode != 0
    assert "root must be an object" in result.stderr


def test_package_rejects_plugin_directory_mismatch_and_non_elf(tmp_path):
    plugin = {"id": "fixture", "digest": "digest", "resources": [], "mcp_servers": []}
    bundle = write_bundle(tmp_path, [plugin])
    (bundle / "plugins" / "wrong").mkdir(parents=True)
    binary = write_binary(tmp_path)
    result = package(binary, bundle, tmp_path / "plugin.tar.gz")
    assert result.returncode != 0
    assert "Plugin directories" in result.stderr

    bundle = write_bundle(tmp_path / "non-elf")
    non_elf = write_binary(tmp_path / "non-elf", "not-runtime")
    non_elf.write_bytes(b"not an executable")
    result = package(non_elf, bundle, tmp_path / "non-elf.tar.gz")
    assert result.returncode != 0
    assert "ELF" in result.stderr
