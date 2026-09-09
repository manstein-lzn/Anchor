"""Fail-closed content resolution.

I2 requires that a reference the control plane recorded either resolves to its
bytes or fails the run. There is no third option: silently falling back to a
live workspace or an empty value would make recovery depend on state that is not
part of the recovery closure.

Resolvers are injected per content kind, so the kernel never imports a git or
artifact backend directly.
"""

from __future__ import annotations

from typing import Protocol

from anchor.domain.content import ContentKind, ContentRef
from anchor.runtime.artifacts import ArtifactStore


class ContentUnavailable(RuntimeError):
    """A recorded reference could not be resolved; the caller must fail closed."""

    def __init__(self, ref: ContentRef, reason: str) -> None:
        self.ref = ref
        self.reason = reason
        super().__init__(f"content unavailable for {ref}: {reason}")


class ContentResolver(Protocol):
    """Read access to one content kind."""

    kind: ContentKind

    def exists(self, ref: ContentRef) -> bool: ...

    def read_text(self, ref: ContentRef) -> str: ...


class ArtifactResolver:
    """Resolve ``artifact://`` references through the artifact store."""

    kind = ContentKind.ARTIFACT

    def __init__(self, artifacts: ArtifactStore) -> None:
        self.artifacts = artifacts

    def exists(self, ref: ContentRef) -> bool:
        if ref.kind is not self.kind:
            return False
        try:
            self.artifacts.get_text(str(ref))
        except (OSError, ValueError):
            return False
        return True

    def read_text(self, ref: ContentRef) -> str:
        if ref.kind is not self.kind:
            raise ContentUnavailable(ref, f"{self.kind.value} resolver cannot read this reference")
        try:
            return self.artifacts.get_text(str(ref))
        except (OSError, ValueError) as exc:
            raise ContentUnavailable(ref, str(exc)) from exc


class ContentRegistry:
    """Route a reference to its resolver; fail closed when none is registered."""

    def __init__(self, resolvers: list[ContentResolver] | None = None) -> None:
        self._resolvers = {resolver.kind: resolver for resolver in resolvers or []}

    def register(self, resolver: ContentResolver) -> None:
        self._resolvers[resolver.kind] = resolver

    def resolver_for(self, ref: ContentRef) -> ContentResolver:
        resolver = self._resolvers.get(ref.kind)
        if resolver is None:
            raise ContentUnavailable(
                ref, f"no {ref.kind.value} resolver is registered for this deployment")
        return resolver

    def exists(self, ref: ContentRef) -> bool:
        try:
            return self.resolver_for(ref).exists(ref)
        except ContentUnavailable:
            return False

    def read_text(self, ref: ContentRef) -> str:
        return self.resolver_for(ref).read_text(ref)

    def require_all(self, refs: list[ContentRef]) -> dict[str, str]:
        """Resolve every reference or raise on the first unavailable one."""
        return {str(ref): self.read_text(ref) for ref in refs}
