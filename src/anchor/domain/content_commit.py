"""A frozen revision that is not yet part of the recovery closure.

The window between "the bytes exist" and "a control event references them" is
where split-brain lives. Recording it explicitly lets a reconciler converge
instead of guessing content from a live workspace.
"""

from __future__ import annotations

from datetime import datetime
from uuid import UUID

from pydantic import Field

from .content import workspace_ref
from .models import DomainModel, utc_now


class PreparedRevision(DomainModel):
    node_run_id: UUID
    attempt: int = Field(ge=0)
    run_id: UUID
    workspace_id: str = Field(min_length=1, max_length=200)
    revision: str = Field(min_length=1, max_length=200)
    manifest_digest: str = Field(min_length=64, max_length=64)
    verifier_result: dict | None = None
    created_at: datetime = Field(default_factory=utc_now)

    @property
    def content_ref(self) -> str:
        """Built through the boundary type so the prefix lives in one place."""
        return str(workspace_ref(self.workspace_id, self.revision))
