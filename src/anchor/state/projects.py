"""Read-only content source registration."""

from __future__ import annotations

import sqlalchemy as sa

from anchor.domain.project import Project

from . import schema as s
from .base import _StoreHost, decode


class ProjectStoreMixin(_StoreHost):
    def create_project(self, project: Project) -> Project:
        """Register a project; re-registering the same id updates it."""
        project.validate_identifier()
        with self._transaction() as connection:
            existing = connection.execute(sa.select(s.projects).where(
                s.projects.c.project_id == project.project_id)).mappings().first()
            if existing is not None:
                connection.execute(sa.update(s.projects).where(
                    s.projects.c.project_id == project.project_id).values(
                    name=project.name, backend=project.backend, root=project.root,
                    default_revision=project.default_revision, updated_at=project.updated_at))
            else:
                self._insert(connection, s.projects, project)
        return project

    def get_project(self, project_id: str) -> Project | None:
        with self.engine.connect() as connection:
            row = connection.execute(sa.select(s.projects).where(
                s.projects.c.project_id == project_id)).mappings().first()
        return decode(Project, row) if row is not None else None

    def list_projects(self) -> list[Project]:
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(s.projects).order_by(
                s.projects.c.project_id)).mappings()
            return [decode(Project, row) for row in rows]
