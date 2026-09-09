"""Content references: the only boundary type between the two planes.

The control plane records *which* content a node consumed and produced; the
content plane owns the bytes. A `ContentRef` is the single place where the two
meet, so parsing, validation and serialization live here and nowhere else.

The revision of a workspace reference must be immutable. A mutable name such as
`main`, `HEAD` or a branch ref would make replay depend on whatever the name
points at later, which breaks I2 and I9; such references are rejected at parse
time rather than at read time.
"""

from __future__ import annotations

import re
from enum import StrEnum

from pydantic import BaseModel, ConfigDict, model_validator

ARTIFACT_PREFIX = "artifact://sha256/"
WORKSPACE_PREFIX = "workspace://"

_DIGEST_RE = re.compile(r"^[0-9a-f]{64}$")
_WORKSPACE_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,199}$")
_REVISION_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:@+-]{3,199}$")

# Names that can move. A reference carrying one of these is not replayable.
MUTABLE_REVISIONS = frozenset({
    "main", "master", "head", "latest", "trunk", "develop", "development",
    "default", "release", "staging", "production", "prod", "dev", "current",
})


class ContentRefError(Exception):
    """A content reference is malformed or not immutable.

    Deliberately not a ``ValueError``: pydantic converts ValueError raised in a
    validator into a ``ValidationError``, which would hide the precise reason a
    reference was refused. A non-ValueError propagates unchanged.
    """


class ContentKind(StrEnum):
    ARTIFACT = "artifact"
    WORKSPACE = "workspace"


class ContentRef(BaseModel):
    """An immutable reference to bytes owned by the content plane."""

    model_config = ConfigDict(frozen=True, extra="forbid")

    kind: ContentKind
    artifact_digest: str | None = None
    workspace_id: str | None = None
    revision: str | None = None
    path: str | None = None

    @model_validator(mode="after")
    def validate_shape(self) -> "ContentRef":
        if self.kind is ContentKind.ARTIFACT:
            if not self.artifact_digest or not _DIGEST_RE.match(self.artifact_digest):
                raise ContentRefError("artifact reference requires a 64-character sha256 digest")
            if any(value is not None for value in (self.workspace_id, self.revision, self.path)):
                raise ContentRefError("artifact reference must not carry workspace fields")
        else:
            if not self.workspace_id or not _WORKSPACE_ID_RE.match(self.workspace_id):
                raise ContentRefError("workspace reference requires a valid workspace id")
            if not self.revision:
                raise ContentRefError("workspace reference requires an immutable revision")
            lowered = self.revision.lower()
            if lowered in MUTABLE_REVISIONS:
                raise ContentRefError(f"revision {self.revision!r} is mutable and cannot be recorded")
            if lowered.startswith("refs") or "/" in self.revision:
                raise ContentRefError("revision must be an immutable id, not a ref or branch name")
            if not _REVISION_RE.match(self.revision):
                raise ContentRefError(f"revision {self.revision!r} is not a valid immutable id")
            if self.path is not None:
                validate_workspace_path(self.path)
            if self.artifact_digest is not None:
                raise ContentRefError("workspace reference must not carry an artifact digest")
        return self

    @property
    def is_artifact(self) -> bool:
        return self.kind is ContentKind.ARTIFACT

    @property
    def is_workspace(self) -> bool:
        return self.kind is ContentKind.WORKSPACE

    def __str__(self) -> str:
        if self.kind is ContentKind.ARTIFACT:
            return f"{ARTIFACT_PREFIX}{self.artifact_digest}"
        base = f"{WORKSPACE_PREFIX}{self.workspace_id}@{self.revision}"
        return f"{base}/{self.path}" if self.path else base


def validate_workspace_path(path: str) -> None:
    if not path or path.startswith("/") or "\x00" in path or len(path) > 1000:
        raise ContentRefError("workspace path must be a non-empty relative path")
    parts = path.split("/")
    if any(part in ("", ".", "..") for part in parts):
        raise ContentRefError("workspace path must not contain empty, '.' or '..' segments")
    if "\\" in path:
        raise ContentRefError("workspace path must use '/' separators")


def parse(text: str) -> ContentRef:
    """Parse a reference string, rejecting anything not replayable."""
    if not isinstance(text, str):
        raise ContentRefError("content reference must be a string")
    if text.startswith(ARTIFACT_PREFIX):
        digest = text[len(ARTIFACT_PREFIX):]
        try:
            return ContentRef(kind=ContentKind.ARTIFACT, artifact_digest=digest)
        except ContentRefError as exc:
            raise ContentRefError(f"invalid artifact reference {text!r}: {exc}") from exc
    if text.startswith(WORKSPACE_PREFIX):
        body = text[len(WORKSPACE_PREFIX):]
        head, _, path = body.partition("/")
        workspace_id, separator, revision = head.partition("@")
        if not separator:
            raise ContentRefError(f"workspace reference {text!r} is missing '@<revision>'")
        try:
            return ContentRef(kind=ContentKind.WORKSPACE, workspace_id=workspace_id,
                              revision=revision, path=path or None)
        except ContentRefError as exc:
            raise ContentRefError(f"invalid workspace reference {text!r}: {exc}") from exc
    raise ContentRefError(f"unsupported content reference scheme: {text!r}")


def artifact_ref(digest: str) -> ContentRef:
    return ContentRef(kind=ContentKind.ARTIFACT, artifact_digest=digest)


def workspace_ref(workspace_id: str, revision: str, path: str | None = None) -> ContentRef:
    return ContentRef(kind=ContentKind.WORKSPACE, workspace_id=workspace_id,
                      revision=revision, path=path)
