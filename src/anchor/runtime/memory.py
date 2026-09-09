"""Durable memory boundary with provenance and tombstone deletion."""
from __future__ import annotations
import hashlib
from threading import RLock
from datetime import datetime, timezone
from pathlib import Path
from typing import Protocol
from uuid import UUID, uuid4
from pydantic import BaseModel, ConfigDict, Field

MemoryStatus = str
"""One of: active (working memory), proposed, promoted, rejected."""

ACTIVE_STATUSES = frozenset({"active", "proposed", "promoted"})


class MemoryRecord(BaseModel):
    model_config = ConfigDict(extra="forbid")
    memory_id: UUID = Field(default_factory=uuid4)
    content: str = Field(min_length=1)
    content_hash: str = Field(min_length=64, max_length=64)
    run_id: UUID | None = None
    node_run_id: UUID | None = None
    status: str = Field(default="active", pattern=r"^(active|proposed|promoted|rejected)$")
    domain: str = Field(default="", max_length=200)
    reviewed_by: str | None = Field(default=None, max_length=200)
    review_reason: str | None = Field(default=None, max_length=2000)
    created_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))
    deleted_at: datetime | None = None

    @classmethod
    def create(cls, content: str, *, run_id: UUID | None = None, node_run_id: UUID | None = None):
        digest = hashlib.sha256(content.encode()).hexdigest()
        return cls(content=content, content_hash=digest, run_id=run_id, node_run_id=node_run_id)

    @classmethod
    def propose(cls, content: str, *, run_id: UUID | None = None, domain: str = ""):
        digest = hashlib.sha256(content.encode()).hexdigest()
        return cls(content=content, content_hash=digest, run_id=run_id,
                   status="proposed", domain=domain)

class MemoryStore(Protocol):
    def put(self, record: MemoryRecord) -> MemoryRecord: ...
    def list(self, *, run_id: UUID | None = None, include_deleted: bool = False,
             status: str | None = None) -> list[MemoryRecord]: ...
    def delete(self, memory_id: UUID) -> MemoryRecord: ...
    def review(self, memory_id: UUID, *, status: str, reviewer: str, reason: str) -> MemoryRecord: ...
    def purge_deleted(self) -> int: ...

class LocalMemoryStore:
    def __init__(self, path: str | Path):
        self.path = Path(path).expanduser(); self.path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        if not self.path.exists(): self.path.touch(mode=0o600)
        self._lock = RLock()

    def _read(self):
        with self._lock:
            with self.path.open(encoding="utf-8") as f:
                return [MemoryRecord.model_validate_json(line) for line in f if line.strip()]

    def put(self, record: MemoryRecord) -> MemoryRecord:
        record = MemoryRecord.model_validate(record)
        with self._lock:
            with self.path.open("a", encoding="utf-8") as f: f.write(record.model_dump_json() + "\n")
        return record

    def list(self, *, run_id: UUID | None = None, include_deleted: bool = False,
             status: str | None = None) -> list[MemoryRecord]:
        records = self._read(); latest = {}
        for record in records: latest[record.memory_id] = record
        result = [r for r in latest.values()
                  if (include_deleted or r.deleted_at is None)
                  and (run_id is None or r.run_id == run_id)
                  and (status is None or r.status == status)]
        return sorted(result, key=lambda r: (r.created_at, str(r.memory_id)))

    def review(self, memory_id: UUID, *, status: str, reviewer: str, reason: str) -> MemoryRecord:
        """Review a proposed lesson into promoted organizational knowledge."""
        if status not in ("promoted", "rejected"):
            raise ValueError("review must promote or reject")
        if not reviewer or not reason:
            raise ValueError("reviewer and reason are required")
        current = next((r for r in self.list(include_deleted=True) if r.memory_id == memory_id), None)
        if current is None:
            raise KeyError(memory_id)
        if current.status != "proposed":
            raise ValueError(f"only proposed lessons can be reviewed, found {current.status}")
        return self.put(current.model_copy(update={
            "status": status, "reviewed_by": reviewer, "review_reason": reason}))

    def delete(self, memory_id: UUID) -> MemoryRecord:
        current = next((r for r in self.list(include_deleted=True) if r.memory_id == memory_id), None)
        if current is None: raise KeyError(memory_id)
        if current.deleted_at is not None: return current
        return self.put(current.model_copy(update={"deleted_at": datetime.now(timezone.utc)}))

    def purge_deleted(self) -> int:
        """Physically remove tombstoned rows only when explicitly requested."""
        with self._lock:
            all_records = self._read(); latest = {}
            for record in all_records: latest[record.memory_id] = record
            records = [r for r in latest.values() if r.deleted_at is None]
            deleted = sum(1 for record in latest.values() if record.deleted_at is not None)
            tmp = self.path.with_suffix(self.path.suffix + ".tmp")
            with tmp.open("w", encoding="utf-8") as f:
                for record in records: f.write(record.model_dump_json() + "\n")
            tmp.chmod(0o600); tmp.replace(self.path)
            return deleted
