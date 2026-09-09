import secrets
import hashlib
import hmac
from dataclasses import asdict
from contextlib import asynccontextmanager
from typing import Annotated, Literal
from uuid import UUID, uuid4

from fastapi import Depends, FastAPI, Header, HTTPException, Path, Query, Request
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse
from fastapi.security import HTTPAuthorizationCredentials, HTTPBearer
from pydantic import Field, JsonValue, ValidationError
from sqlalchemy.exc import SQLAlchemyError
from starlette.middleware.trustedhost import TrustedHostMiddleware

from anchor.domain.admission import RunReceipt, RunRequest
from anchor.domain.bundle import GraphBundle, build_bundle
from anchor.domain.drafts import GraphDraft
from anchor.domain.graph import GraphDefinition, GraphValidationResult, GraphValidator, GraphVersion, Trigger, TriggerType
from anchor.domain.models import (ContextSnapshot, DomainModel, EdgeDecision, NodeRun, Run, Task,
                                  VerificationRecord, utc_now)
from anchor.domain.operations import ToolOperation
from anchor.state.errors import AdmissionConflict, ConcurrencyConflict, GraphVersionConflict
from anchor.state.relational import RelationalStateStore
from anchor.runtime.config import load_runtime_config
from anchor.runtime.context import canonical_json
from anchor.api.routes_content import register_content_routes
from anchor.runtime.settings import AnchorSettings
from anchor.runtime.secrets import ChainedSecretProvider, EnvironmentSecretProvider, JsonFileSecretProvider, SecretUnavailable
from anchor.runtime.memory import LocalMemoryStore, MemoryRecord
from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.academic import register_academic_behaviors
from anchor.runtime.behaviors import BehaviorRegistry
from anchor.runtime.capabilities import CapabilityRegistry, CapabilityRegistryError
from anchor.runtime.supervisor import assess_leases


class DraftWrite(DomainModel):
    expected_revision: int = Field(ge=0)
    definition: dict[str, JsonValue]
    layout: dict[str, JsonValue] = Field(default_factory=dict)


class PublishRequest(DomainModel):
    expected_revision: int = Field(ge=1)


class TriggerWrite(DomainModel):
    graph_version_id: UUID
    type: TriggerType = TriggerType.MANUAL
    enabled: bool = True
    cron: str | None = None
    interval_seconds: int | None = Field(default=None, gt=0)
    event_type: str | None = None
    timezone: str = "UTC"
    filter_expression: str | None = None
    idempotency_field: str | None = None
    webhook_secret_ref: str | None = None


class TriggerEnabled(DomainModel):
    enabled: bool


class StartRequest(DomainModel):
    objective: str = Field(min_length=1)
    constraints: list[str] = Field(default_factory=list)
    success_criteria: list[str] = Field(default_factory=list)
    inputs: dict[str, JsonValue] = Field(default_factory=dict)


class EventIngress(DomainModel):
    objective: str = Field(min_length=1)
    constraints: list[str] = Field(default_factory=list)
    success_criteria: list[str] = Field(default_factory=list)
    inputs: dict[str, JsonValue] = Field(default_factory=dict)


class LeaseRecovery(DomainModel):
    reason: str = Field(min_length=1, max_length=500)


class RunControl(DomainModel):
    reason: str = Field(min_length=1, max_length=2000)
    actor: str = Field(default="operator", min_length=1, max_length=200)


class StorageBudget(DomainModel):
    """Adjustable monitoring targets. Omit a field to leave it unchanged."""

    global_bytes: int | None = None
    graphs: dict[str, int | None] | None = None


class OperationReconciliation(DomainModel):
    status: Literal["succeeded", "failed"]
    reconciliation_ref: str = Field(min_length=1, max_length=1000)
    result_ref: str | None = Field(default=None, max_length=1000)
    error_code: str | None = Field(default=None, max_length=200)
    reason: str = Field(min_length=1, max_length=2000)
    actor: str = Field(default="operator", min_length=1, max_length=200)


class ApprovalDecision(DomainModel):
    reason: str = Field(min_length=1, max_length=2000)
    actor: str = Field(default="operator", min_length=1, max_length=200)


class EventResume(DomainModel):
    event_type: str = Field(min_length=1, max_length=200)
    payload: dict[str, JsonValue] = Field(default_factory=dict)
    actor: str = Field(default="event", min_length=1, max_length=200)


class MemoryProposal(DomainModel):
    content: str = Field(min_length=1)
    run_id: UUID | None = None
    domain: str = Field(default="", max_length=200)


class MemoryReview(DomainModel):
    status: str = Field(pattern=r"^(promoted|rejected)$")
    reviewer: str = Field(min_length=1, max_length=200)
    reason: str = Field(min_length=1, max_length=2000)


class BundleImport(DomainModel):
    bundle: dict[str, JsonValue]
    expected_revision: int = Field(default=0, ge=0)
    publish: bool = True
    import_triggers: bool = True


class LeaseFailure(DomainModel):
    error_code: str = Field(min_length=1, max_length=200, pattern=r"^[A-Za-z][A-Za-z0-9_.-]*$")
    phase: str = Field(default="execute", min_length=1, max_length=100)


def memory_store() -> LocalMemoryStore:
    return LocalMemoryStore(AnchorSettings().memory_path)


GraphId = Annotated[str, Path(pattern=r"^[A-Za-z][A-Za-z0-9_-]{0,127}$")]
PageSize = Annotated[int, Query(ge=1, le=200)]
Offset = Annotated[int, Query(ge=0)]


def create_app(store: RelationalStateStore | None = None, token: str | None = None) -> FastAPI:
    settings = AnchorSettings()
    secret = token if token is not None else settings.api_token
    if not secret or len(secret) < 32:
        raise RuntimeError("configure ANCHOR_API_TOKEN with at least 32 characters")
    database_url = settings.database_url
    if store is None and not database_url:
        raise RuntimeError("configure ANCHOR_DATABASE_URL explicitly and apply migrations first")
    owns_store = store is None

    @asynccontextmanager
    async def lifespan(app):
        current = store or RelationalStateStore(database_url)
        app.state.store = current
        try:
            current.check_schema()
            yield
        finally:
            if owns_store:
                current.close()

    app = FastAPI(title="Anchor API", version="0.1.0", lifespan=lifespan)
    app.add_middleware(TrustedHostMiddleware, allowed_hosts=["127.0.0.1", "localhost", "[::1]", "testserver"])

    @app.middleware("http")
    async def expose_server_clock(request: Request, call_next):
        response = await call_next(request)
        # Execution timestamps are written by the server. Browsers may run on
        # a forwarded remote client whose wall clock differs by minutes, so
        # give the UI an explicit server clock reference for elapsed times.
        response.headers["X-Anchor-Server-Time"] = utc_now().isoformat()
        return response

    bearer = HTTPBearer(auto_error=False)

    def authenticate(credentials: Annotated[HTTPAuthorizationCredentials | None, Depends(bearer)]):
        if credentials is None or not secrets.compare_digest(credentials.credentials.encode(), secret.encode()):
            raise HTTPException(401, "authentication required", headers={"WWW-Authenticate": "Bearer"})

    def database(request: Request) -> RelationalStateStore:
        return request.app.state.store

    DB = Annotated[RelationalStateStore, Depends(database)]
    auth = [Depends(authenticate)]

    def required(value):
        if value is None:
            raise HTTPException(404, "resource not found")
        return value

    @app.exception_handler(ConcurrencyConflict)
    @app.exception_handler(GraphVersionConflict)
    @app.exception_handler(AdmissionConflict)
    async def conflict(request, exc):
        return JSONResponse(status_code=409, content={"error": {"code": "conflict", "message": str(exc)}})

    @app.exception_handler(KeyError)
    async def missing(request, exc):
        return JSONResponse(status_code=404, content={"error": {"code": "not_found"}})

    @app.exception_handler(ValueError)
    async def invalid(request, exc):
        return JSONResponse(status_code=422, content={"error": {"code": "invalid_request", "message": str(exc)}})

    @app.exception_handler(RequestValidationError)
    @app.exception_handler(ValidationError)
    async def validation(request, exc):
        errors = [{"loc": list(error["loc"]), "type": error["type"], "msg": error["msg"]} for error in exc.errors()]
        return JSONResponse(status_code=422, content={"error": {"code": "validation_failed", "issues": errors}})

    @app.exception_handler(SQLAlchemyError)
    async def unavailable(request, exc):
        return JSONResponse(status_code=503, content={"error": {"code": "storage_unavailable"}})

    @app.get("/health/live")
    def live():
        return {"status": "ok"}

    @app.get("/health/ready", dependencies=auth)
    def ready(db: DB):
        try:
            db.check_schema()
        except (RuntimeError, SQLAlchemyError):
            raise HTTPException(503, "storage not ready") from None
        return {"status": "ready", "execution_connected": db.runtime_connected("execution_receiver"),
                "worker_connected": db.runtime_connected("agent_worker"),
                "control_worker_connected": db.runtime_connected("control_worker"),
                "verifier_worker_connected": db.runtime_connected("verifier_worker")}

    @app.get("/api/runtime/capabilities", dependencies=auth)
    def capabilities():
        """Expose reference metadata only; credentials never cross this boundary."""
        try:
            config = load_runtime_config()
        except RuntimeError:
            return {"configured": False, "models": [], "agents": [], "tools": [], "verifiers": []}
        return {
            "configured": True,
            "models": [{"ref": item.ref, "provider": item.provider, "model": item.model,
                         "wire_api": item.wire_api, "base_url": item.base_url} for item in config.models],
            "agents": [{"ref": item.ref, "model_ref": item.model_ref, "tool_refs": item.tool_refs,
                        "output_format": item.output_format, "behavior_ref": item.behavior_ref,
                        "max_retries": item.max_retries,
                        "max_tool_calls": item.max_tool_calls,
                        "max_parallel_tools": item.max_parallel_tools,
                        "tool_call_limits": item.tool_call_limits} for item in config.agents],
            "tools": [{"ref": item.ref, "description": item.description,
                       "side_effect": item.side_effect, "idempotent": item.idempotent,
                       "operation_kind": item.operation_kind, "evidence_json": item.evidence_json,
                       "model_excerpt_chars": item.model_excerpt_chars,
                       "retry_excerpt_chars": item.retry_excerpt_chars} for item in config.tools],
            "verifiers": [{"ref": item.ref, "version": item.version, "adapter": item.adapter,
                           "model_ref": item.model_ref} for item in config.verifiers],
        }

    register_content_routes(app, auth=auth, db=DB, required=required)

    @app.get("/api/graphs/ir", dependencies=auth)
    def graph_ir():
        """Authoring reference for the Graph IR: node contract, DSL, template."""
        from anchor.domain.ir import describe_ir
        return describe_ir()

    @app.post("/api/graphs/validate", response_model=GraphValidationResult, dependencies=auth)
    def validate(definition: GraphDefinition):
        return GraphValidator().validate(definition)

    @app.post("/api/graphs/capabilities/validate", dependencies=auth)
    def validate_capabilities(definition: GraphDefinition):
        try:
            config = load_runtime_config()
            registry = CapabilityRegistry(models=config.models, agents=config.agents, tools=config.tools,
                                          verifiers=config.verifiers)
        except RuntimeError:
            return {"valid": False, "issues": [{"code": "runtime_config_missing", "message": "runtime capability configuration is unavailable"}]}
        behaviors = BehaviorRegistry()
        register_academic_behaviors(behaviors)
        known_behaviors = set(behaviors.refs())
        issues = []
        for node in definition.nodes:
            if node.agent_ref:
                try: registry.validate_agent(node.agent_ref)
                except CapabilityRegistryError as exc: issues.append({"code": "missing_agent_capability", "node_id": node.id, "message": str(exc)})
            if node.tool_ref:
                try: registry.tool(node.tool_ref)
                except CapabilityRegistryError as exc: issues.append({"code": "missing_tool_capability", "node_id": node.id, "message": str(exc)})
            if node.type.value == "tool":
                owner = (node.metadata or {}).get("owner_agent")
                if not owner:
                    issues.append({"code": "missing_tool_owner", "node_id": node.id,
                                   "message": "tool node requires owner_agent metadata"})
                else:
                    try:
                        scope = registry.agent(owner)
                    except CapabilityRegistryError as exc:
                        issues.append({"code": "missing_agent_capability", "node_id": node.id,
                                       "message": str(exc)})
                    else:
                        if node.tool_ref and node.tool_ref not in scope.tool_refs:
                            issues.append({"code": "missing_tool_scope", "node_id": node.id,
                                           "message": f"agent {owner} may not use tool {node.tool_ref}"})
            if node.verifier_ref:
                try: registry.validate_verifier(node.verifier_ref)
                except CapabilityRegistryError as exc: issues.append({"code": "missing_verifier_capability", "node_id": node.id, "message": str(exc)})
            behavior_ref = (node.metadata or {}).get("behavior_ref")
            if behavior_ref and behavior_ref not in known_behaviors:
                issues.append({"code": "missing_node_behavior", "node_id": node.id,
                               "message": f"unknown node behavior: {behavior_ref}"})
            if node.agent_ref and node.type.value == "agent":
                try:
                    agent = registry.agent(node.agent_ref)
                except CapabilityRegistryError:
                    agent = None
                if agent is not None and agent.behavior_ref and agent.behavior_ref not in known_behaviors:
                    issues.append({"code": "missing_agent_behavior", "node_id": node.id,
                                   "message": f"unknown agent behavior: {agent.behavior_ref}"})
        return {"valid": not issues, "issues": issues}

    @app.get("/api/graph-versions/{version_id}/bundle", dependencies=auth)
    def export_bundle(version_id: UUID, db: DB):
        """Export one pinned version as a portable, tamper-evident bundle."""
        version = required(db.get_graph_version(version_id))
        return build_bundle(version.definition, version=version.version,
                            content_hash=version.content_hash,
                            triggers=db.list_triggers(version_id))

    @app.post("/api/bundles/import", dependencies=auth)
    def import_bundle(body: BundleImport, db: DB):
        """Import a bundle as a new draft; optionally publish and rebind triggers.

        Trigger bindings are recreated with fresh IDs against the new version;
        canvas layout is authoring-local and does not travel in bundles."""
        bundle = GraphBundle.model_validate(body.bundle).verify()
        draft = db.save_draft(bundle.graph.graph_id,
                              expected_revision=body.expected_revision,
                              definition=bundle.graph.model_dump(mode="json"), layout={})
        result: dict = {"draft": draft, "version": None, "triggers": []}
        if body.publish:
            version = db.publish_draft(bundle.graph.graph_id,
                                       expected_revision=draft.revision)
            result["version"] = version
            if body.import_triggers:
                for trigger in bundle.triggers:
                    fresh = Trigger(**{**trigger.model_dump(),
                                       "id": uuid4(),
                                       "graph_version_id": version.graph_version_id})
                    result["triggers"].append(db.register_trigger(fresh))
        return result

    @app.get("/api/graphs", response_model=list[GraphDraft], dependencies=auth)
    def drafts(db: DB, limit: PageSize = 50, offset: Offset = 0):
        return db.list_drafts(limit, offset)

    @app.put("/api/graphs/{graph_id}/draft", response_model=GraphDraft, dependencies=auth)
    def save_draft(graph_id: GraphId, body: DraftWrite, db: DB):
        return db.save_draft(graph_id, **body.model_dump())

    @app.get("/api/graphs/{graph_id}/draft", response_model=GraphDraft, dependencies=auth)
    def get_draft(graph_id: GraphId, db: DB):
        return required(db.get_draft(graph_id))

    @app.post("/api/graphs/{graph_id}/publish", response_model=GraphVersion, dependencies=auth)
    def publish(graph_id: GraphId, body: PublishRequest, db: DB):
        return db.publish_draft(graph_id, body.expected_revision)

    @app.get("/api/graphs/{graph_id}/versions", response_model=list[GraphVersion], dependencies=auth)
    def versions(graph_id: GraphId, db: DB, limit: PageSize = 50, offset: Offset = 0):
        return db.list_graph_versions(graph_id, limit, offset)

    @app.get("/api/graph-versions/{version_id}", response_model=GraphVersion, dependencies=auth)
    def version(version_id: UUID, db: DB):
        return required(db.get_graph_version(version_id))

    @app.put("/api/triggers/{trigger_id}", response_model=Trigger, dependencies=auth)
    def register(trigger_id: UUID, body: TriggerWrite, db: DB):
        return db.register_trigger(Trigger(id=trigger_id, **body.model_dump()))

    @app.get("/api/graph-versions/{version_id}/triggers", response_model=list[Trigger], dependencies=auth)
    def triggers(version_id: UUID, db: DB):
        required(db.get_graph_version(version_id))
        return db.list_triggers(version_id)

    @app.patch("/api/triggers/{trigger_id}", response_model=Trigger, dependencies=auth)
    def enable(trigger_id: UUID, body: TriggerEnabled, db: DB):
        return db.set_trigger_enabled(trigger_id, body.enabled)

    @app.post("/api/triggers/{trigger_id}/runs", response_model=RunReceipt, status_code=202, dependencies=auth)
    def start(trigger_id: UUID, body: StartRequest, db: DB,
              idempotency_key: Annotated[str, Header(alias="Idempotency-Key", min_length=1, max_length=256)]):
        trigger = required(db.get_trigger(trigger_id))
        if trigger.type.value != "manual" or trigger.filter_expression:
            raise HTTPException(409, "manual ingress cannot bypass event or schedule rules")
        return db.admit_run(RunRequest(trigger_id=trigger_id, idempotency_key=idempotency_key, **body.model_dump()))

    @app.post("/api/triggers/{trigger_id}/events", response_model=RunReceipt, status_code=202, dependencies=auth)
    def ingest_event(trigger_id: UUID, body: EventIngress, db: DB,
                     event_type: Annotated[str, Header(alias="X-Anchor-Event-Type", min_length=1, max_length=200)],
                     signature: Annotated[str | None, Header(alias="X-Anchor-Signature")] = None,
                     idempotency_key: Annotated[str | None, Header(alias="Idempotency-Key", max_length=256)] = None):
        trigger = required(db.get_trigger(trigger_id))
        if trigger.type is not TriggerType.INTERNAL_EVENT and trigger.type is not TriggerType.WEBHOOK:
            raise HTTPException(409, "trigger is not an event ingress")
        if trigger.event_type != event_type:
            raise HTTPException(409, "event type does not match trigger")
        if trigger.type is TriggerType.WEBHOOK:
            if not trigger.webhook_secret_ref or not signature or not signature.startswith("sha256="):
                raise HTTPException(401, "webhook signature required")
            try:
                cfg = load_runtime_config()
                providers = [EnvironmentSecretProvider()]
                if cfg.secret_file: providers.append(JsonFileSecretProvider(cfg.secret_file))
                secret = ChainedSecretProvider(*providers).get(trigger.webhook_secret_ref)
            except (RuntimeError, SecretUnavailable):
                raise HTTPException(503, "webhook verification unavailable") from None
            expected = hmac.new(secret.encode(), body.model_dump_json().encode(), hashlib.sha256).hexdigest()
            if not hmac.compare_digest(signature[7:], expected):
                raise HTTPException(401, "invalid webhook signature")
        key = idempotency_key
        if not key and trigger.idempotency_field:
            value = body.inputs.get(trigger.idempotency_field)
            if value is not None:
                key = str(value)
        if not key:
            raise HTTPException(422, "Idempotency-Key or configured idempotency_field is required")
        return db.admit_run(RunRequest(trigger_id=trigger_id, idempotency_key=key, **body.model_dump()))

    @app.get("/api/runs", response_model=list[Run], dependencies=auth)
    def runs(db: DB, limit: PageSize = 50, offset: Offset = 0, graph_id: str | None = None,
             include_archived: bool = False, status: list[str] | None = Query(None)):
        return db.list_runs(limit, offset, graph_id=graph_id,
                            include_archived=include_archived, statuses=status)

    @app.get("/api/runs/{run_id}", response_model=Run, dependencies=auth)
    def run(run_id: UUID, db: DB):
        return required(db.get_run(run_id))

    @app.post("/api/runs/{run_id}/stop", response_model=Run, dependencies=auth)
    def stop_run(run_id: UUID, body: LeaseRecovery, db: DB):
        return db.stop_run(run_id, reason=body.reason)

    @app.post("/api/runs/{run_id}/pause", response_model=Run, dependencies=auth)
    def pause_run(run_id: UUID, body: RunControl, db: DB):
        return db.pause_run(run_id, reason=body.reason, actor=body.actor)

    @app.post("/api/runs/{run_id}/resume", response_model=Run, dependencies=auth)
    def resume_run(run_id: UUID, body: RunControl, db: DB):
        return db.resume_run(run_id, reason=body.reason, actor=body.actor)

    @app.post("/api/runs/{run_id}/archive", response_model=Run, dependencies=auth)
    def archive_run(run_id: UUID, db: DB):
        return db.set_run_archived(run_id, archived=True)

    @app.post("/api/runs/{run_id}/unarchive", response_model=Run, dependencies=auth)
    def unarchive_run(run_id: UUID, db: DB):
        return db.set_run_archived(run_id, archived=False)

    @app.get("/api/tasks/{task_id}", response_model=Task, dependencies=auth)
    def task(task_id: UUID, db: DB):
        return required(db.get_task(task_id))

    @app.get("/api/runs/{run_id}/nodes", response_model=list[NodeRun], dependencies=auth)
    def nodes(run_id: UUID, db: DB):
        required(db.get_run(run_id))
        return db.list_node_runs(run_id)

    @app.get("/api/runs/{run_id}/contexts", response_model=list[ContextSnapshot], dependencies=auth)
    def contexts(run_id: UUID, db: DB):
        required(db.get_run(run_id))
        return db.list_context_snapshots(run_id)

    @app.get("/api/runs/{run_id}/decisions", response_model=list[EdgeDecision], dependencies=auth)
    def edge_decisions(run_id: UUID, db: DB):
        required(db.get_run(run_id))
        return db.list_edge_decisions(run_id)

    @app.get("/api/runs/{run_id}/verifications", response_model=list[VerificationRecord], dependencies=auth)
    def verifications(run_id: UUID, db: DB):
        required(db.get_run(run_id))
        return db.list_verifications(run_id)

    @app.get("/api/runs/{run_id}/progress", dependencies=auth)
    def progress_evidence(run_id: UUID, db: DB):
        required(db.get_run(run_id))
        items = db.list_progress_evidence(run_id)
        return [item.model_dump(mode="json") for item in items]

    @app.get("/api/runs/{run_id}/diagnostics", dependencies=auth)
    def diagnostics(run_id: UUID, db: DB):
        required(db.get_run(run_id))
        items = db.list_open_diagnostics(run_id)
        return [item.model_dump(mode="json") for item in items]

    @app.post("/api/runs/{run_id}/diagnostics/{diagnostic_id}/supersede", dependencies=auth)
    def supersede_diagnostic(run_id: UUID, diagnostic_id: str, db: DB):
        required(db.get_run(run_id))
        return db.supersede_diagnostic(diagnostic_id, superseded_by=f"operator:{run_id}")

    @app.get("/api/runs/{run_id}/events", dependencies=auth)
    def events(run_id: UUID, db: DB, after: Offset = 0, limit: PageSize = 100):
        required(db.get_run(run_id))
        return db.event_page(run_id, after, limit)

    def _graph_budgets(db) -> dict[str, int | None]:
        stored = db.get_storage_budgets()
        stored.pop(db.GLOBAL_SCOPE, None)
        return stored

    @app.get("/api/storage", dependencies=auth)
    def storage(db: DB):
        """Read-only footprint: real on-disk total plus per-graph attribution."""
        from anchor.state.storage import storage_report
        settings = AnchorSettings()
        stored = db.get_storage_budgets()
        return storage_report(
            db, artifact_root=settings.artifact_root,
            global_budget=stored.get(db.GLOBAL_SCOPE),
            graph_budgets=_graph_budgets(db))

    @app.get("/api/storage/budget", dependencies=auth)
    def storage_budget(db: DB):
        stored = db.get_storage_budgets()
        return {"global_bytes": stored.get(db.GLOBAL_SCOPE),
                "graphs": _graph_budgets(db)}

    @app.put("/api/storage/budget", dependencies=auth)
    def set_storage_budget(body: StorageBudget, db: DB):
        """Set budgets at runtime; a null value clears that budget."""
        fields = body.model_fields_set
        if "global_bytes" in fields:
            db.set_storage_budget(db.GLOBAL_SCOPE, body.global_bytes)
        for graph_id, value in (body.graphs or {}).items():
            if not graph_id or len(graph_id) > 200:
                raise HTTPException(status_code=422, detail="invalid graph id")
            db.set_storage_budget(graph_id, value)
        stored = db.get_storage_budgets()
        return {"global_bytes": stored.get(db.GLOBAL_SCOPE),
                "graphs": _graph_budgets(db)}

    @app.get("/api/retention/preview", dependencies=auth)
    def retention_preview(db: DB):
        """Dry run of the rolling sweep; deletes nothing."""
        from anchor.runtime.artifacts import LocalArtifactStore
        from anchor.runtime.retention import plan_storage_budgets
        return plan_storage_budgets(db, LocalArtifactStore(AnchorSettings().artifact_root))

    @app.post("/api/retention/sweep", dependencies=auth)
    def retention_sweep(db: DB):
        """Run the rolling sweep now: evict oldest finished runs over budget."""
        from anchor.runtime.artifacts import LocalArtifactStore
        from anchor.runtime.retention import enforce_storage_budgets
        return enforce_storage_budgets(db, LocalArtifactStore(AnchorSettings().artifact_root),
                                       trigger="api")

    @app.get("/api/retention/audit", dependencies=auth)
    def retention_audit(db: DB, limit: PageSize = 50):
        return db.list_retention_audit(limit)

    @app.get("/api/runs/{run_id}/operations", response_model=list[ToolOperation], dependencies=auth)
    def operations(run_id: UUID, db: DB):
        required(db.get_run(run_id))
        return db.list_tool_operations(run_id)

    @app.post("/api/operations/{operation_id}/reconcile", dependencies=auth)
    def reconcile_operation(operation_id: UUID, body: OperationReconciliation, db: DB):
        """Resolve an unknown side effect with external evidence, then apply it.

        The operation ledger is the source of truth: reconciliation records the
        evidence and the node consequence (complete on success, fail on failure)
        is deterministic. Unknown outcomes are never retried automatically.
        """
        from anchor.domain.operations import OperationStatus
        status = OperationStatus(body.status)
        required(db.get_tool_operation(operation_id))
        reconciled = db.reconcile_tool_operation(
            operation_id, status=status, reconciliation_ref=body.reconciliation_ref,
            result_ref=body.result_ref, error_code=body.error_code)
        node_run = db.resolve_reconciled_operation(
            operation_id, actor=body.actor, reason=body.reason,
            read_artifact=LocalArtifactStore(AnchorSettings().artifact_root).get_text)
        return {"operation": reconciled, "node_run": node_run}

    @app.post("/api/leases/{claim_id}/recover", response_model=NodeRun, dependencies=auth)
    def recover_lease(claim_id: UUID, body: LeaseRecovery, db: DB):
        return db.recover_node_lease(claim_id, reason=body.reason)

    @app.post("/api/leases/{claim_id}/fail", response_model=NodeRun, dependencies=auth)
    def fail_lease(claim_id: UUID, body: LeaseFailure, db: DB):
        lease = next((item for item in db.list_active_leases() if item.claim_id == claim_id), None)
        if lease is None:
            raise KeyError(claim_id)
        run = required(db.get_run(lease.run_id))
        graph = required(db.get_graph_version(run.graph_version_id))
        node = next((item for item in graph.definition.nodes if item.id == lease.node_id), None)
        if node is None or node.type.value != "agent":
            raise HTTPException(409, "only an Agent lease can be failed through this operator endpoint")
        return db.fail_node_and_propagate(
            claim_id, lease.worker_id, error_code=body.error_code, phase=body.phase)

    @app.get("/api/waits", dependencies=auth)
    def waiting_nodes(db: DB, run_id: UUID | None = None):
        """Operator view of nodes parked in approval/event waits."""
        items = []
        for node in db.list_waiting_nodes(run_id=run_id):
            run = db.get_run(node.run_id)
            node_type = ""
            if run is not None:
                graph = db.get_graph_version(run.graph_version_id)
                if graph is not None:
                    match = next((item for item in graph.definition.nodes
                                  if item.id == node.node_id), None)
                    node_type = match.type.value if match else ""
            items.append({"node_run": node, "run_id": node.run_id,
                          "node_type": node_type})
        return items

    @app.post("/api/waits/{node_run_id}/approve", response_model=NodeRun, dependencies=auth)
    def approve_node(node_run_id: UUID, body: ApprovalDecision, db: DB):
        ref = LocalArtifactStore(AnchorSettings().artifact_root).put_text(
            canonical_json({"decision": "approved", "actor": body.actor,
                            "reason": body.reason}),
            media_type="application/json")
        return db.decide_approval(node_run_id, approved=True, reason=body.reason,
                                  actor=body.actor, output_ref=ref)

    @app.post("/api/waits/{node_run_id}/reject", response_model=NodeRun, dependencies=auth)
    def reject_node(node_run_id: UUID, body: ApprovalDecision, db: DB):
        return db.decide_approval(node_run_id, approved=False, reason=body.reason,
                                  actor=body.actor, output_ref="decision://rejected")

    @app.post("/api/waits/{node_run_id}/resume", response_model=NodeRun, dependencies=auth)
    def resume_node(node_run_id: UUID, body: EventResume, db: DB):
        ref = LocalArtifactStore(AnchorSettings().artifact_root).put_text(
            canonical_json({"event_type": body.event_type, "actor": body.actor,
                            "payload": body.payload}),
            media_type="application/json")
        try:
            return db.resume_event(node_run_id, event_type=body.event_type,
                                   payload=body.payload, output_ref=ref, actor=body.actor)
        except ValueError as exc:
            if "does not match" in str(exc):
                raise HTTPException(409, str(exc)) from None
            raise

    @app.get("/api/leases/active", dependencies=auth)
    def active_leases(db: DB, stale_after: float = Query(default=30.0, gt=0), run_id: UUID | None = None):
        """Read-only lease liveness view; no lease is reclaimed implicitly."""
        leases = db.list_active_leases()
        if run_id is not None:
            leases = [lease for lease in leases if lease.run_id == run_id]
        node_types: dict[str, str] = {}
        for lease in leases:
            run = db.get_run(lease.run_id)
            graph = db.get_graph_version(run.graph_version_id) if run else None
            if graph:
                node = next((candidate for candidate in graph.definition.nodes if candidate.id == lease.node_id), None)
                if node:
                    node_types[str(lease.claim_id)] = node.type.value
        return [
            {**asdict(item), "node_type": node_types.get(str(item.lease.claim_id), "unknown")}
            for item in assess_leases(
                leases, stale_after=stale_after, node_types=node_types,
            )
        ]

    @app.get("/api/memory", dependencies=auth)
    def memories(run_id: UUID | None = None, include_deleted: bool = False,
                 status: str | None = None, domain: str | None = None):
        items = memory_store().list(run_id=run_id, include_deleted=include_deleted,
                                    status=status)
        if domain:
            items = [item for item in items if item.domain == domain]
        return items

    @app.post("/api/memory/propose", response_model=MemoryRecord, dependencies=auth)
    def propose_memory(body: MemoryProposal):
        return memory_store().put(MemoryRecord.propose(
            body.content, run_id=body.run_id, domain=body.domain))

    @app.post("/api/memory/{memory_id}/review", response_model=MemoryRecord, dependencies=auth)
    def review_memory(memory_id: UUID, body: MemoryReview):
        return memory_store().review(memory_id, status=body.status,
                                     reviewer=body.reviewer, reason=body.reason)

    @app.post("/api/memory/purge", dependencies=auth)
    def purge_memory():
        return {"purged": memory_store().purge_deleted()}

    @app.get("/api/artifacts/{digest}", dependencies=auth)
    def artifact(digest: str):
        if len(digest) != 64 or any(char not in "0123456789abcdef" for char in digest):
            raise HTTPException(404, "artifact not found")
        ref = f"artifact://sha256/{digest}"
        try:
            content = LocalArtifactStore(AnchorSettings().artifact_root).get_text(ref)
        except (OSError, ValueError):
            raise HTTPException(404, "artifact not found") from None
        return {"ref": ref, "content": content}

    @app.delete("/api/memory/{memory_id}", dependencies=auth)
    def delete_memory(memory_id: UUID):
        return memory_store().delete(memory_id)

    return app
