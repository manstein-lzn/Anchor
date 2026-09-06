from typing import Literal
from uuid import UUID

from pydantic import AwareDatetime, Field, JsonValue

from .models import DomainModel


class RunRequest(DomainModel):
    """Normalized, trusted trigger occurrence, after authentication and filtering."""

    trigger_id: UUID
    idempotency_key: str = Field(min_length=1)
    objective: str = Field(min_length=1)
    constraints: list[str] = Field(default_factory=list)
    success_criteria: list[str] = Field(default_factory=list)
    inputs: dict[str, JsonValue] = Field(default_factory=dict)


class RunReceipt(DomainModel):
    task_id: UUID
    run_id: UUID
    graph_version_id: UUID
    message_id: UUID


class RunDispatch(DomainModel):
    message_id: UUID
    event_type: Literal["run.requested"] = "run.requested"
    schema_version: Literal[1] = 1
    run_id: UUID
    task_id: UUID
    graph_version_id: UUID
    inputs: dict[str, JsonValue]
    created_at: AwareDatetime
