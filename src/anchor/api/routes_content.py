"""Project and workspace routes.

Extracted from the composition root so ``app.py`` stays within its module budget.
The routes only orchestrate the store and the workspace manager; no policy lives
here beyond translating a workspace error into a 422.
"""

from fastapi import FastAPI, HTTPException
from pydantic import Field

from anchor.domain.models import DomainModel
from anchor.domain.project import Project
from anchor.domain.workspace import Workspace, WorkspaceOperation
from anchor.runtime.settings import AnchorSettings


class ProjectWrite(DomainModel):
    project_id: str = Field(min_length=1, max_length=200)
    name: str = Field(min_length=1, max_length=200)
    root: str = Field(min_length=1, max_length=1000)
    backend: str = Field(default="git", pattern=r"^[a-z][a-z0-9_-]*$")
    default_revision: str | None = Field(default=None, max_length=200)


class WorkspaceWrite(DomainModel):
    project_id: str = Field(min_length=1, max_length=200)
    base_revision: str = Field(min_length=1, max_length=200)
    workspace_id: str | None = Field(default=None, max_length=200)
    actor: str = Field(default="operator", min_length=1, max_length=200)


class WorkspaceFileWrite(DomainModel):
    path: str = Field(min_length=1, max_length=1000)
    content: str
    actor: str = Field(default="operator", min_length=1, max_length=200)


class WorkspacePathWrite(DomainModel):
    path: str = Field(min_length=1, max_length=1000)
    actor: str = Field(default="operator", min_length=1, max_length=200)


class WorkspaceControl(DomainModel):
    actor: str = Field(default="operator", min_length=1, max_length=200)


class WorkspaceFork(DomainModel):
    new_workspace_id: str | None = Field(default=None, max_length=200)
    base_revision: str | None = Field(default=None, max_length=200)
    actor: str = Field(default="operator", min_length=1, max_length=200)


class WorkspaceMerge(DomainModel):
    source_revision: str = Field(min_length=1, max_length=200)
    policy: str = Field(default="require_clean", pattern=r"^[a-z_]+$")
    actor: str = Field(default="operator", min_length=1, max_length=200)


def register_content_routes(app: FastAPI, *, auth, db, required) -> None:
    """Register the content-plane routes; ``db`` is the store dependency type."""
    _register_projects(app, auth=auth, db=db, required=required)
    _register_workspaces(app, auth=auth, db=db, required=required)


def _register_projects(app: FastAPI, *, auth, db, required) -> None:
    @app.post("/api/projects", response_model=Project, dependencies=auth)
    def register_project(body: ProjectWrite, store: db):
        """Register a read-only content source; registration never writes to it."""
        from anchor.domain.project import Project as ProjectModel
        from anchor.runtime.workspace import WorkspaceError, validate_project_root
        project = ProjectModel(**body.model_dump())
        try:
            project.root = validate_project_root(project.root, project.backend)
        except WorkspaceError as exc:
            raise HTTPException(status_code=422, detail=str(exc)) from exc
        return store.create_project(project)

    @app.get("/api/projects", response_model=list[Project], dependencies=auth)
    def projects(store: db):
        return store.list_projects()

    @app.get("/api/projects/{project_id}", response_model=Project, dependencies=auth)
    def project(project_id: str, store: db):
        return required(store.get_project(project_id))

def _register_workspaces(app: FastAPI, *, auth, db, required) -> None:
    def _manager(store):
        from anchor.runtime.workspaces import WorkspaceManager
        return WorkspaceManager(store, root=AnchorSettings().workspace_root)

    def _refuse(call):
        from anchor.runtime.workspace import WorkspaceError
        try:
            return call()
        except WorkspaceError as exc:
            raise HTTPException(status_code=422, detail=str(exc)) from exc

    @app.post("/api/workspaces", response_model=Workspace, dependencies=auth)
    def create_workspace(body: WorkspaceWrite, store: db):
        """Fork a run-scoped writable worktree from an immutable base revision."""
        return _refuse(lambda: _manager(store).create(
            project_id=body.project_id, base_revision=body.base_revision,
            workspace_id=body.workspace_id, actor=body.actor))

    @app.get("/api/workspaces", response_model=list[Workspace], dependencies=auth)
    def workspaces(store: db, project_id: str | None = None):
        return store.list_workspaces(project_id=project_id)

    @app.get("/api/workspaces/{workspace_id}", response_model=Workspace, dependencies=auth)
    def workspace(workspace_id: str, store: db):
        return required(store.get_workspace(workspace_id))

    @app.get("/api/workspaces/{workspace_id}/operations",
             response_model=list[WorkspaceOperation], dependencies=auth)
    def workspace_operations(workspace_id: str, store: db):
        required(store.get_workspace(workspace_id))
        return store.list_workspace_operations(workspace_id)

    @app.post("/api/workspaces/{workspace_id}/write",
              response_model=WorkspaceOperation, dependencies=auth)
    def workspace_write(workspace_id: str, body: WorkspaceFileWrite, store: db):
        return _refuse(lambda: _manager(store).write_text(
            workspace_id, body.path, body.content, actor=body.actor))

    @app.post("/api/workspaces/{workspace_id}/delete",
              response_model=WorkspaceOperation, dependencies=auth)
    def workspace_delete(workspace_id: str, body: WorkspacePathWrite, store: db):
        return _refuse(lambda: _manager(store).delete(workspace_id, body.path, actor=body.actor))

    @app.post("/api/workspaces/{workspace_id}/fork", response_model=Workspace, dependencies=auth)
    def workspace_fork(workspace_id: str, body: WorkspaceFork, store: db):
        """Fork an independent worktree; parallel writers must not share one."""
        return _refuse(lambda: _manager(store).fork(
            workspace_id, new_workspace_id=body.new_workspace_id,
            base_revision=body.base_revision, actor=body.actor))

    @app.post("/api/workspaces/{workspace_id}/merge",
              response_model=WorkspaceOperation, dependencies=auth)
    def workspace_merge(workspace_id: str, body: WorkspaceMerge, store: db):
        """Merge an immutable revision; a conflict fails closed."""
        return _refuse(lambda: _manager(store).merge(
            workspace_id, body.source_revision, policy=body.policy, actor=body.actor))

    @app.post("/api/workspaces/{workspace_id}/freeze",
              response_model=Workspace, dependencies=auth)
    def workspace_freeze(workspace_id: str, body: WorkspaceControl, store: db):
        return _refuse(lambda: _manager(store).freeze(workspace_id, actor=body.actor))

    @app.post("/api/workspaces/{workspace_id}/archive",
              response_model=Workspace, dependencies=auth)
    def workspace_archive(workspace_id: str, body: WorkspaceControl, store: db):
        return _refuse(lambda: _manager(store).archive(workspace_id, actor=body.actor))
