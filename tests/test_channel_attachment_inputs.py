"""Ingress snapshots preserve authorized bytes without a second extractor."""

import base64
import copy
import hashlib
import io
import os
from pathlib import Path
from types import SimpleNamespace

import pytest

from anchor.channel import media
from anchor.channel.attachments import attachment_payload, prepare_channel_input


def image():
    from PIL import Image

    output = io.BytesIO()
    Image.new("RGB", (6, 6), "green").save(output, "PNG")
    return output.getvalue()


def item(root, name, data, **metadata):
    path = root / "state/channels/wecom/events/fixture" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    return {"path": str(path), "name": name, "kind": "file", **metadata}


def test_source_is_read_once_and_extraction_payload_share_frozen_bytes(tmp_path, monkeypatch):
    source = item(tmp_path, "source.txt", b"original evidence")
    source_path = Path(source["path"])
    original_read = media._read_file
    source_reads = []

    def change_after_read(path, remaining):
        data = original_read(path, remaining)
        if path == source_path:
            source_reads.append(path)
            source_path.write_bytes(b"changed externally")
        return data

    monkeypatch.setattr(media, "_read_file", change_after_read)
    scheduler = SimpleNamespace(root=tmp_path)
    frozen = prepare_channel_input(scheduler, {"attachments": [source]})
    assert source_reads == [source_path]
    assert "original evidence" in frozen["attachment_content"]
    assert "changed externally" not in frozen["attachment_content"]
    source_path.unlink()
    payload = attachment_payload(scheduler, frozen)
    assert base64.b64decode(payload[0]["data_base64"]) == b"original evidence"
    assert frozen["attachment_snapshot"]["files"][0]["sha256"] == hashlib.sha256(b"original evidence").hexdigest()
    assert prepare_channel_input(scheduler, frozen) == frozen


@pytest.mark.parametrize("fault", ["outside", "symlink", "parent_symlink", "fifo", "directory", "traversal"])
def test_unauthorized_or_nonregular_source_is_rejected(tmp_path, fault):
    source = item(tmp_path, "source.txt", b"private source")
    path = Path(source["path"])
    if fault == "outside":
        path = tmp_path / "outside.txt"
        path.write_bytes(b"outside secret")
    elif fault == "symlink":
        path = path.parent / "link.txt"
        path.symlink_to(source["path"])
    elif fault == "parent_symlink":
        directory = path.parent.parent / "linked"
        directory.symlink_to(path.parent, target_is_directory=True)
        path = directory / path.name
    elif fault == "fifo":
        path = path.parent / "pipe.txt"
        os.mkfifo(path)
    elif fault == "directory":
        path = path.parent
    else:
        path = path.parent / ".." / "fixture" / path.name
    with pytest.raises((ValueError, OSError)):
        prepare_channel_input(SimpleNamespace(root=tmp_path), {"attachments": [{"path": str(path)}]})
    assert not (tmp_path / "state/channels/wecom/attachment-inputs").exists()


def test_byte_and_image_limits_fail_before_snapshot_publication(tmp_path, monkeypatch):
    scheduler = SimpleNamespace(root=tmp_path)
    monkeypatch.setattr(media, "MAX_FILE_BYTES", 8)
    oversized = item(tmp_path, "too-large.txt", b"x" * 9)
    with pytest.raises(ValueError, match="byte limit"):
        prepare_channel_input(scheduler, {"attachments": [oversized]})
    monkeypatch.setattr(media, "MAX_TOTAL_BYTES", 10)
    sources = [item(tmp_path, name, b"x" * 6) for name in ("first.txt", "second.txt")]
    with pytest.raises(ValueError, match="byte limit"):
        prepare_channel_input(scheduler, {"attachments": sources})
    monkeypatch.setattr(media, "MAX_FILE_BYTES", 20 * 1024 * 1024)
    monkeypatch.setattr(media, "MAX_TOTAL_BYTES", 50 * 1024 * 1024)
    monkeypatch.setattr(media, "MAX_IMAGES", 1)
    sources = [item(tmp_path, name, image(), kind="image") for name in ("first.png", "second.png")]
    with pytest.raises(ValueError, match="aggregate image"):
        prepare_channel_input(scheduler, {"attachments": sources})
    assert not (tmp_path / "state/channels/wecom/attachment-inputs").exists()


def test_image_mime_is_sniffed_and_all_valid_images_are_preserved(tmp_path):
    scheduler = SimpleNamespace(root=tmp_path)
    data = image()
    sources = [item(tmp_path, name, data, kind="image", mime_type="image/jpeg")
               for name in ("first.png", "second.png")]
    frozen = prepare_channel_input(scheduler, {"attachments": sources})
    assert [entry["media_type"] for entry in attachment_payload(scheduler, frozen)] == ["image/png", "image/png"]
    assert all(entry["mime_type"] == "image/png" for entry in frozen["attachments"])


@pytest.mark.parametrize("changed", ["hash", "path", "virtual", "bytes", "media_type"])
def test_prepared_snapshot_is_checked_before_it_can_replace_a_turn(tmp_path, changed):
    scheduler = SimpleNamespace(root=tmp_path)
    frozen = prepare_channel_input(scheduler, {"attachments": [item(tmp_path, "source.txt", b"frozen")]})
    forged = copy.deepcopy(frozen)
    file = forged["attachment_snapshot"]["files"][0]
    if changed == "hash":
        file["sha256"] = "0" * 64
    elif changed == "path":
        file["path"] = str(tmp_path / "outside.txt")
    elif changed == "virtual":
        forged["attachments"][0]["path"] = "/in/channel/someone-else.txt"
    elif changed == "media_type":
        file["media_type"] = {"invalid": True}
    else:
        Path(file["path"]).write_bytes(b"edited")
    with pytest.raises(ValueError):
        prepare_channel_input(scheduler, forged)


def test_snapshot_reuse_never_overwrites_existing_bytes(tmp_path):
    scheduler = SimpleNamespace(root=tmp_path)
    source = item(tmp_path, "source.txt", b"immutable")
    first = prepare_channel_input(scheduler, {"attachments": [source]})
    assert prepare_channel_input(scheduler, {"attachments": [source]}) == first
    Path(first["attachment_snapshot"]["files"][0]["path"]).write_bytes(b"different")
    with pytest.raises(ValueError, match="content hash"):
        prepare_channel_input(scheduler, {"attachments": [source]})


def test_wrong_event_directory_and_duplicate_virtual_names_are_rejected(tmp_path):
    first = item(tmp_path, "first.txt", b"one")
    second = item(tmp_path, "second.txt", b"two")
    second_path = Path(second["path"])
    second_path.rename(second_path.parent.parent / "second.txt")
    second["path"] = str(second_path.parent.parent / "second.txt")
    scheduler = SimpleNamespace(root=tmp_path)
    with pytest.raises(ValueError, match="one event directory"):
        prepare_channel_input(scheduler, {"attachments": [first, second]})
    with pytest.raises(ValueError, match="unique"):
        prepare_channel_input(scheduler, {"attachments": [first, first]})
