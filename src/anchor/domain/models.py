from __future__ import annotations

from datetime import datetime, timezone
from enum import StrEnum
from uuid import UUID, uuid4

from pydantic import AwareDatetime, BaseModel, ConfigDict, Field, JsonValue, field_validator


def utc_now() -> datetime:
    return datetime.now(timezone.utc)


class TaskStatus(StrEnum):
    CREATED = "created"
    READY = "ready"
    RUNNING = "running"
    WAITING_APPROVAL = "waiting_approval"
    BLOCKED = "blocked"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"


class RunStatus(StrEnum):
    CREATED = "created"
    QUEUED = "queued"
    RUNNING = "running"
    WAITING_APPROVAL = "waiting_approval"
    PAUSED = "paused"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"


class NodeRunStatus(StrEnum):
    PENDING = "pending"
    READY = "ready"
    RUNNING = "running"
    WAITING_APPROVAL = "waiting_approval"
    WAITING_EVENT = "waiting_event"
    RETRYING = "retrying"
    COMPLETED = "completed"
    SKIPPED = "skipped"
    FAILED = "failed"
    CANCELLED = "cancelled"


class EdgeDecisionReason(StrEnum):
    UNCONDITIONAL = "unconditional"
    CONDITION_TRUE = "condition_true"
    CONDITION_FALSE = "condition_false"
    UPSTREAM_SKIPPED = "upstream_skipped"


class VerificationVerdict(StrEnum):
    PASSED = "passed"
    REJECTED = "rejected"
    ERROR = "error"


class DomainModel(BaseModel):
    model_config = ConfigDict(extra="forbid")


class Task(DomainModel):
    id: UUID = Field(default_factory=uuid4)
    objective: str = Field(min_length=1)
    constraints: list[str] = Field(default_factory=list)
    success_criteria: list[str] = Field(default_factory=list)
    status: TaskStatus = TaskStatus.CREATED
    revision: int = Field(default=0, ge=0)
    created_at: datetime = Field(default_factory=utc_now)
    updated_at: datetime = Field(default_factory=utc_now)


class Run(DomainModel):
    id: UUID = Field(default_factory=uuid4)
    task_id: UUID
    graph_version_id: UUID
    status: RunStatus = RunStatus.CREATED
    current_phase: str = "created"
    revision: int = Field(default=0, ge=0)
    last_event_sequence: int = Field(default=0, ge=0)
    workflow_version: str = "v0.1"
    context_generation: int = Field(default=0, ge=0)
    archived_at: datetime | None = None
    created_at: datetime = Field(default_factory=utc_now)
    updated_at: datetime = Field(default_factory=utc_now)


class NodeRun(DomainModel):
    id: UUID = Field(default_factory=uuid4)
    run_id: UUID
    node_id: str = Field(min_length=1, max_length=64)
    status: NodeRunStatus = NodeRunStatus.PENDING
    attempt: int = Field(default=0, ge=0)
    revision: int = Field(default=0, ge=0)
    input_hash: str | None = None
    output_ref: str | None = None
    error_code: str | None = None
    # Recovery scheduling is separate from business-cycle state. `last_error_class`
    # classifies the failed attempt; `next_attempt_at` gates re-claiming after a
    # transient failure so the plan survives a worker restart.
    last_error_class: str | None = Field(default=None, max_length=64)
    next_attempt_at: datetime | None = None
    context_generation: int = Field(default=0, ge=0)
    created_at: datetime = Field(default_factory=utc_now)
    updated_at: datetime = Field(default_factory=utc_now)


class ContextSnapshot(DomainModel):
    """Immutable, replayable input context used for one NodeRun attempt."""

    id: UUID = Field(default_factory=uuid4)
    run_id: UUID
    node_run_id: UUID
    generation: int = Field(ge=1)
    input_hash: str = Field(pattern=r"^[0-9a-f]{64}$")
    snapshot: dict[str, JsonValue]
    created_at: datetime = Field(default_factory=utc_now)


class EdgeDecision(DomainModel):
    """Immutable routing evidence for one edge of a pinned Graph Version."""

    run_id: UUID
    edge_index: int = Field(ge=0)
    source_attempt: int = Field(default=0, ge=0)
    source_node_id: str = Field(min_length=1, max_length=64)
    target_node_id: str = Field(min_length=1, max_length=64)
    selected: bool
    reason: EdgeDecisionReason
    condition: str | None = Field(default=None, max_length=1000)
    evaluator: str = Field(min_length=1, max_length=100)
    evaluator_version: str = Field(min_length=1, max_length=100)
    evaluation_context_hash: str | None = Field(default=None, pattern=r"^[0-9a-f]{64}$")
    evidence_ref: str | None = Field(default=None, max_length=1000)
    decided_at: datetime = Field(default_factory=utc_now)


class VerificationRecord(DomainModel):
    """Immutable verdict bound to the exact evidence and context it evaluated."""

    verification_id: UUID = Field(default_factory=uuid4)
    claim_id: UUID
    run_id: UUID
    node_run_id: UUID
    node_id: str = Field(min_length=1, max_length=64)
    verifier_ref: str = Field(min_length=1, max_length=200)
    verifier_version: str = Field(min_length=1, max_length=100)
    adapter: str = Field(min_length=1, max_length=100)
    adapter_version: str = Field(min_length=1, max_length=100)
    verdict: VerificationVerdict
    reason: str = Field(min_length=1, max_length=4000)
    evidence_ref: str = Field(min_length=1, max_length=1000)
    verified_artifact_hashes: list[str] = Field(default_factory=list)
    verified_context_hash: str = Field(pattern=r"^[0-9a-f]{64}$")
    model_ref: str | None = Field(default=None, max_length=200)
    model_provider: str | None = Field(default=None, max_length=100)
    model_name: str | None = Field(default=None, max_length=200)
    model_response_id: str | None = Field(default=None, max_length=500)
    decided_at: datetime = Field(default_factory=utc_now)

    @field_validator("verified_artifact_hashes")
    @classmethod
    def validate_hashes(cls, values: list[str]) -> list[str]:
        for value in values:
            if len(value) != 64 or any(char not in "0123456789abcdef" for char in value):
                raise ValueError("verified artifact hashes must be SHA-256 hex digests")
        if values != sorted(set(values)):
            raise ValueError("verified artifact hashes must be sorted and unique")
        return values


class NodeLease(DomainModel):
    claim_id: UUID
    node_run_id: UUID
    run_id: UUID
    node_id: str = Field(min_length=1, max_length=64)
    worker_id: str = Field(min_length=1, max_length=128)
    acquired_at: datetime = Field(default_factory=utc_now)
    heartbeat_at: datetime = Field(default_factory=utc_now)
    released_at: datetime | None = None


class ProgressEvidence(DomainModel):
    """Immutable observation of one execution unit's durable facts.

    This is an observation record, not a business verdict. It deliberately
    excludes timestamps and random ids from the progress signal so activity
    cannot be mistaken for verified progress.
    """

    evidence_id: UUID = Field(default_factory=uuid4)
    run_id: UUID
    node_run_id: UUID | None = None
    state_revision: int = Field(ge=0)
    phase: str = Field(min_length=1, max_length=100)
    artifact_refs: tuple[str, ...] = ()
    verifier_passes: int = Field(default=0, ge=0)
    hypothesis_hash: str | None = Field(default=None, min_length=64, max_length=64)
    tool_operation_ids: tuple[str, ...] = ()
    heartbeat_at: AwareDatetime
    waiting_for: str | None = Field(default=None, max_length=200)
    worker_expected: bool = True
    verified_progress_refs: tuple[str, ...] = ()
    cycle_iteration: int = Field(default=0, ge=0)
    cycle_fingerprint: str | None = Field(default=None, min_length=64, max_length=64)
    created_at: AwareDatetime = Field(default_factory=utc_now)


class DiagnosticRequest(DomainModel):
    """Durable request for diagnosis; never a failure verdict."""

    diagnostic_id: str = Field(min_length=1, max_length=200)
    run_id: UUID
    node_run_id: UUID | None = None
    reason: str = Field(min_length=1, max_length=4000)
    evidence_refs: tuple[str, ...] = ()
    suggested_actions: tuple[str, ...] = ()
    created_at: AwareDatetime
    superseded_by: str | None = Field(default=None, max_length=200)
    status: str = Field(default="open", pattern=r"^(open|superseded)$")
