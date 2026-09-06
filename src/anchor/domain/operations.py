from __future__ import annotations

import hashlib
import json
from datetime import datetime
from enum import StrEnum
from uuid import UUID

from pydantic import Field, JsonValue, model_validator

from .models import DomainModel, utc_now


class OperationStatus(StrEnum):
    REGISTERED = "registered"
    RUNNING = "running"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    OUTCOME_UNKNOWN = "outcome_unknown"


class ToolOperation(DomainModel):
    operation_id: UUID
    claim_id: UUID
    node_run_id: UUID
    run_id: UUID
    tool_ref: str = Field(min_length=1, max_length=500)
    arguments: dict[str, JsonValue] = Field(default_factory=dict)
    request_hash: str = Field(min_length=64, max_length=64)
    status: OperationStatus = OperationStatus.REGISTERED
    result_ref: str | None = Field(default=None, max_length=1000)
    error_code: str | None = Field(default=None, max_length=200)
    reconciliation_ref: str | None = Field(default=None, max_length=1000)
    created_at: datetime = Field(default_factory=utc_now)
    updated_at: datetime = Field(default_factory=utc_now)

    @staticmethod
    def hash_request(tool_ref: str, arguments: dict[str, JsonValue]) -> str:
        canonical = json.dumps({"tool_ref": tool_ref, "arguments": arguments}, sort_keys=True,
                               separators=(",", ":"), ensure_ascii=False, allow_nan=False)
        return hashlib.sha256(canonical.encode("utf-8")).hexdigest()

    @classmethod
    def register(cls, *, operation_id: UUID, claim_id: UUID, node_run_id: UUID,
                 run_id: UUID, tool_ref: str, arguments: dict[str, JsonValue]) -> ToolOperation:
        return cls(operation_id=operation_id, claim_id=claim_id, node_run_id=node_run_id,
                   run_id=run_id, tool_ref=tool_ref, arguments=arguments,
                   request_hash=cls.hash_request(tool_ref, arguments))

    @model_validator(mode="after")
    def verify_hash_and_outcome(self) -> ToolOperation:
        if self.request_hash != self.hash_request(self.tool_ref, self.arguments):
            raise ValueError("operation request hash does not match its content")
        if self.status is OperationStatus.SUCCEEDED and not self.result_ref:
            raise ValueError("successful operation requires result_ref")
        if self.status in {OperationStatus.FAILED, OperationStatus.OUTCOME_UNKNOWN} and not self.error_code:
            raise ValueError(f"{self.status.value} operation requires error_code")
        if self.status is OperationStatus.OUTCOME_UNKNOWN and self.reconciliation_ref:
            raise ValueError("unknown operation cannot claim reconciliation evidence")
        return self
