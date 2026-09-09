"""SQLAlchemy store for PostgreSQL and isolated SQLite contract-test databases.

The implementation is composed from cohesive mixins so each concern stays
readable while every transaction still runs through one shared core.
"""

from __future__ import annotations

from .base import StoreBase, decode, values, wait_status_for
from .checkpoints import CheckpointStoreMixin
from .execution import ExecutionStoreMixin
from .graphs import GraphStoreMixin
from .operations import OperationStoreMixin
from .progress import ProgressStoreMixin
from .projects import ProjectStoreMixin
from .retention import RetentionStoreMixin
from .storage import StorageStoreMixin


class RelationalStateStore(RetentionStoreMixin, StorageStoreMixin, ProjectStoreMixin,
                           ProgressStoreMixin, OperationStoreMixin,
                           CheckpointStoreMixin, ExecutionStoreMixin,
                           GraphStoreMixin, StoreBase):
    """Canonical state store used by every worker, service and the API."""


__all__ = ["RelationalStateStore", "decode", "values", "wait_status_for"]
