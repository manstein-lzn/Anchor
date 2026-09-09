"""A run-scoped, writable worktree with an auditable operation ledger.

A workspace is where work happens; it is never a source of truth. Only the
revisions it produces, once referenced by a control event, enter the recovery
closure (I2). The workspace record therefore tracks a lifecycle and a lineage,
not the bytes.
"""

from __future__ import annotations

from datetime import datetime
from enum import StrEnum
from uuid import UUID, uuid4

from pydantic import Field

from .models import DomainModel, utc_now


class WorkspaceState(StrEnum):
    ACTIVE = "active"
    FROZEN = "frozen"
    ARCHIVED = "archived"


class WorkspaceOperationKind(StrEnum):
    CREATE = "create"
    WRITE = "write"
    DELETE = "delete"
    FREEZE = "freeze"
    ARCHIVE = "archive"


class Workspace(DomainModel):
    workspace_id: str = Field(min_length=1, max_length=200)
    project_id: str = Field(min_length=1, max_length=200)
    base_revision: str = Field(min_length=1, max_length=200)
    branch: str = Field(min_length=1, max_length=300)
    path: str = Field(min_length=1, max_length=1000)
    state: WorkspaceState = WorkspaceState.ACTIVE
    current_revision: str | None = Field(default=None, max_length=200)
    writer_node_run_id: UUID | None = None
    created_at: datetime = Field(default_factory=utc_now)
    updated_at: datetime = Field(default_factory=utc_now)

    @property
    def is_writable(self) -> bool:
        return self.state is WorkspaceState.ACTIVE


class WorkspaceOperation(DomainModel):
    operation_id: UUID = Field(default_factory=uuid4)
    workspace_id: str = Field(min_length=1, max_length=200)
    kind: WorkspaceOperationKind
    path: str | None = Field(default=None, max_length=1000)
    before_revision: str | None = Field(default=None, max_length=200)
    after_revision: str | None = Field(default=None, max_length=200)
    content_hash: str | None = Field(default=None, max_length=64)
    actor: str = Field(default="operator", min_length=1, max_length=200)
    created_at: datetime = Field(default_factory=utc_now)
