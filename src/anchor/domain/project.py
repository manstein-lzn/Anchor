"""A registered, read-only content source.

A Project is a long-lived repository the content plane can read from. It is not
a workspace: a workspace is a run-scoped worktree (W1). Registering a project
only grants read access at immutable revisions, which is why the default
revision is stored as a *convenience*, never recorded in a content reference.
"""

from __future__ import annotations

import re
from datetime import datetime

from pydantic import Field

from .models import DomainModel, utc_now

_PROJECT_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,199}$")


class Project(DomainModel):
    project_id: str = Field(min_length=1, max_length=200)
    name: str = Field(min_length=1, max_length=200)
    backend: str = Field(default="git", pattern=r"^[a-z][a-z0-9_-]*$")
    root: str = Field(min_length=1, max_length=1000)
    default_revision: str | None = Field(default=None, max_length=200)
    created_at: datetime = Field(default_factory=utc_now)
    updated_at: datetime = Field(default_factory=utc_now)

    @property
    def is_git(self) -> bool:
        return self.backend == "git"

    def validate_identifier(self) -> "Project":
        if not _PROJECT_ID_RE.match(self.project_id):
            raise ValueError(f"invalid project id: {self.project_id!r}")
        return self
