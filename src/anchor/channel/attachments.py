"""Freeze authorized channel files before admitting or replacing a Turn."""

from __future__ import annotations

import base64
import hashlib
import os
from pathlib import Path
from typing import Any
from uuid import uuid4

from anchor.channel import media


def _managed_root(scheduler: Any) -> Path:
    return (scheduler.root / "state" / "channels" / "wecom").absolute()


def source_descriptors(items: object) -> list[dict]:
    if not isinstance(items, list) or len(items) > media.MAX_ATTACHMENTS:
        raise ValueError("at most 16 channel attachments are allowed")
    descriptors = []
    for item in items:
        if not isinstance(item, dict) or not isinstance(item.get("path"), str):
            raise ValueError("channel attachment path is invalid")
        if any(key not in {"path", "name", "kind", "mime_type", "size"} for key in item):
            raise ValueError("channel attachment descriptor has unsupported fields")
        for key in ("name", "kind", "mime_type"):
            if key in item and (not isinstance(item[key], str) or len(item[key]) > 200):
                raise ValueError("channel attachment metadata is invalid")
        if "size" in item and (type(item["size"]) is not int or item["size"] < 0):
            raise ValueError("channel attachment size is invalid")
        path = Path(item["path"])
        if (not path.is_absolute() or ".." in path.parts or
                any(ord(char) < 32 for char in item["path"])):
            raise ValueError("channel attachment requires an absolute path without ..")
        name = item.get("name", path.name)
        if not name or Path(name).name != name or "\\" in name:
            raise ValueError("channel attachment name is invalid")
        descriptors.append(dict(item))
    return descriptors


def _image_requested(item: dict, data: bytes) -> bool:
    name = str(item.get("name") or item["path"])
    return (data.startswith((b"\x89PNG", b"\xff\xd8\xff", b"GIF8")) or
            data[:4] == b"RIFF" and data[8:12] == b"WEBP" or
            item.get("kind") == "image" or Path(name).suffix.lower() in media._IMAGE_SUFFIXES or
            str(item.get("mime_type", "")).lower().startswith("image/"))


def _open_directory(path: Path) -> int:
    directory = os.open("/", os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in path.parts[1:]:
            try:
                os.mkdir(part, mode=0o700, dir_fd=directory)
                os.fsync(directory)
            except FileExistsError:
                pass
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
            os.close(directory)
            directory = child
        return directory
    except BaseException:
        os.close(directory)
        raise


def _save_file(root: Path, name: str, data: bytes) -> dict:
    digest = hashlib.sha256(data).hexdigest()
    path = root / "attachment-inputs" / digest / name
    directory = _open_directory(path.parent)
    temporary = "." + uuid4().hex + ".tmp"
    try:
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                     0o600, dir_fd=directory)
        with os.fdopen(fd, "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        # A competing admission may already have published the same immutable bytes.
        try:
            os.link(temporary, name, src_dir_fd=directory, dst_dir_fd=directory, follow_symlinks=False)
            os.fsync(directory)
        except FileExistsError:
            if media._read_file(path, len(data)) != data:
                raise ValueError("channel attachment snapshot differs from its content hash") from None
    finally:
        try:
            try:
                os.unlink(temporary, dir_fd=directory)
            except FileNotFoundError:
                pass
        finally:
            os.close(directory)
    return {"name": name, "path": str(path), "sha256": digest, "size": len(data)}


def snapshot_manifest(scheduler: Any, channel_input: dict) -> list[dict]:
    snapshot = channel_input.get("attachment_snapshot")
    if not isinstance(snapshot, dict) or snapshot.get("format") != 1:
        raise ValueError("channel attachment snapshot is missing or invalid")
    files = snapshot.get("files")
    virtual = channel_input.get("attachments")
    sources = source_descriptors(channel_input.get("attachment_sources"))
    if (not isinstance(files, list) or not files or len(files) > media.MAX_ATTACHMENTS or
            not isinstance(virtual, list) or len(virtual) != len(files) or len(sources) != len(files) or
            not isinstance(channel_input.get("attachment_content"), str) or
            len(channel_input["attachment_content"]) > media.MAX_TEXT_CHARS):
        raise ValueError("channel attachment snapshot does not match the Turn input")
    root = _managed_root(scheduler) / "attachment-inputs"
    names = set()
    manifest = []
    total = image_total = images = 0
    for file, item, source in zip(files, virtual, sources, strict=True):
        if not isinstance(file, dict) or not isinstance(item, dict):
            raise ValueError("channel attachment snapshot metadata is invalid")
        name, digest, size, mime = (file.get(key) for key in ("name", "sha256", "size", "media_type"))
        if (not isinstance(name, str) or not name or name in {".", ".."} or
                Path(name).name != name or "\\" in name or any(ord(char) < 32 for char in name) or
                name in names or Path(source["path"]).name != name or
                not isinstance(digest, str) or len(digest) != 64 or
                any(char not in "0123456789abcdef" for char in digest) or
                type(size) is not int or not 0 <= size <= media.MAX_FILE_BYTES or
                not (mime is None or isinstance(mime, str) and mime in {"image/png", "image/jpeg", "image/webp"}) or
                file.get("path") != str(root / digest / name) or
                item.get("path") != "/in/channel/" + name or item.get("size") != size):
            raise ValueError("channel attachment snapshot metadata is invalid")
        names.add(name)
        total += size
        if mime:
            images += 1
            image_total += size
            if size == 0 or size > media.MAX_IMAGE_BYTES or item.get("mime_type") != mime:
                raise ValueError("channel attachment image metadata is invalid")
        expected = {key: source[key] for key in ("kind", "name", "mime_type") if key in source}
        expected.update(path="/in/channel/" + name, size=size)
        if mime:
            expected["mime_type"] = mime
        if item != expected:
            raise ValueError("channel attachment virtual metadata changed")
        manifest.append({"name": name, "sha256": digest, "size": size, "media_type": mime})
    if (total > media.MAX_TOTAL_BYTES or images > media.MAX_IMAGES or
            image_total > media.MAX_IMAGE_TOTAL_BYTES):
        raise ValueError("channel attachment aggregate limit exceeded")
    return manifest


def attachment_payload(scheduler: Any, channel_input: dict) -> list[dict]:
    manifest = snapshot_manifest(scheduler, channel_input)
    payload = []
    for file, expected in zip(channel_input["attachment_snapshot"]["files"], manifest, strict=True):
        data = media._read_file(Path(file["path"]), expected["size"])
        if len(data) != expected["size"] or hashlib.sha256(data).hexdigest() != expected["sha256"]:
            raise ValueError("channel attachment snapshot differs from its content hash")
        if expected["media_type"] and media._image_mime(data) != expected["media_type"]:
            raise ValueError("channel attachment snapshot image type changed")
        payload.append({"name": expected["name"], "data_base64": base64.b64encode(data).decode("ascii"),
                        "media_type": expected["media_type"]})
    return payload


def _read_sources(sources: list[dict], root: Path) -> tuple[tuple[bytes, ...], int]:
    paths = [Path(item["path"]) for item in sources]
    if any(not path.is_relative_to(root) for path in paths):
        raise ValueError("channel attachment is outside the managed state directory")
    if any(path.parent != paths[0].parent for path in paths):
        raise ValueError("channel attachments must belong to one event directory")
    if len({path.name for path in paths}) != len(paths):
        raise ValueError("channel attachment names must be unique")
    contents = []
    total = image_total = images = 0
    for item, path in zip(sources, paths, strict=True):
        data = media._read_file(path, media.MAX_TOTAL_BYTES - total)
        contents.append(data)
        total += len(data)
        if _image_requested(item, data):
            images += 1
            image_total += len(data)
            if not data or len(data) > media.MAX_IMAGE_BYTES:
                raise ValueError("image must be nonempty and at most 10 MiB")
    if images > media.MAX_IMAGES or image_total > media.MAX_IMAGE_TOTAL_BYTES:
        raise ValueError("channel attachment aggregate image limit exceeded")
    return tuple(contents), images


def prepare_channel_input(scheduler: Any, channel_input: dict | None) -> dict | None:
    """Prepare outside admission; a prepared input can be checked without extraction."""
    if channel_input is None:
        return None
    if not isinstance(channel_input, dict):
        raise ValueError("channel input must be an object")
    items = channel_input.get("attachments", [])
    if not isinstance(items, list):
        raise ValueError("channel attachments must be a list")
    if "attachment_snapshot" in channel_input:
        attachment_payload(scheduler, channel_input)
        return channel_input
    if not items:
        return channel_input
    sources = source_descriptors(items)
    root = _managed_root(scheduler)
    contents, images = _read_sources(sources, root)
    content, prompt_images = media.prepare_attachments(sources, contents=contents)
    if len(prompt_images) != images:
        raise ValueError("channel attachment image is invalid or unsupported")
    image_types = {data: mime for data, mime in prompt_images}
    files, virtual = [], []
    for item, data in zip(sources, contents, strict=True):
        path = Path(item["path"])
        mime = image_types.get(data)
        file = {**_save_file(root, path.name, data), "media_type": mime}
        files.append(file)
        value = {key: item[key] for key in ("kind", "name", "mime_type") if key in item}
        value.update(path="/in/channel/" + path.name, size=len(data))
        if mime:
            value["mime_type"] = mime
        virtual.append(value)
    prepared = {"attachments": virtual, "attachment_content": content, "attachment_sources": sources,
                "attachment_snapshot": {"format": 1, "files": files}}
    snapshot_manifest(scheduler, prepared)
    return prepared
