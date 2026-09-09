"""Portable relational schema. PostgreSQL uses JSONB for structured fields."""

import sqlalchemy as sa
from sqlalchemy.dialects.postgresql import JSONB

metadata = sa.MetaData(naming_convention={
    "pk": "pk_%(table_name)s", "fk": "fk_%(table_name)s_%(column_0_name)s_%(referred_table_name)s",
    "uq": "uq_%(table_name)s_%(column_0_name)s", "ix": "ix_%(table_name)s_%(column_0_name)s",
})
json_type = sa.JSON().with_variant(JSONB(), "postgresql")


def identifier(name="id", *args, **kwargs):
    return sa.Column(name, sa.String(36), *args, **kwargs)


def timestamp(name):
    return sa.Column(name, sa.DateTime(timezone=True), nullable=False)


tasks = sa.Table("tasks", metadata,
    identifier(primary_key=True), sa.Column("objective", sa.Text, nullable=False),
    sa.Column("constraints", json_type, nullable=False), sa.Column("success_criteria", json_type, nullable=False),
    sa.Column("status", sa.String(32), nullable=False), sa.Column("revision", sa.Integer, nullable=False),
    timestamp("created_at"), timestamp("updated_at"))

graph_versions = sa.Table("graph_versions", metadata,
    identifier("graph_version_id", primary_key=True), sa.Column("graph_id", sa.String(128), nullable=False),
    sa.Column("version", sa.Integer, nullable=False), sa.Column("definition", json_type, nullable=False),
    sa.Column("content_hash", sa.String(64), nullable=False), sa.Column("status", sa.String(32), nullable=False),
    timestamp("published_at"), sa.UniqueConstraint("graph_id", "version"))

triggers = sa.Table("triggers", metadata,
    identifier(primary_key=True), identifier("graph_version_id", sa.ForeignKey(graph_versions.c.graph_version_id), nullable=False),
    sa.Column("type", sa.String(32), nullable=False), sa.Column("enabled", sa.Boolean, nullable=False),
    sa.Column("cron", sa.Text), sa.Column("interval_seconds", sa.Integer), sa.Column("event_type", sa.Text),
    sa.Column("timezone", sa.Text, nullable=False), sa.Column("filter_expression", sa.Text),
    sa.Column("idempotency_field", sa.Text), sa.Column("webhook_secret_ref", sa.String(200)))

retention_audit = sa.Table("retention_audit", metadata,
    identifier("audit_id", primary_key=True),
    timestamp("created_at"),
    sa.Column("trigger", sa.String(64), nullable=False),
    sa.Column("evicted_runs", sa.Integer, nullable=False),
    sa.Column("freed_bytes", sa.BigInteger, nullable=False),
    sa.Column("detail", json_type, nullable=False))

projects = sa.Table("projects", metadata,
    sa.Column("project_id", sa.String(200), primary_key=True),
    sa.Column("name", sa.String(200), nullable=False),
    sa.Column("backend", sa.String(32), nullable=False),
    sa.Column("root", sa.String(1000), nullable=False),
    sa.Column("default_revision", sa.String(200)),
    timestamp("created_at"), timestamp("updated_at"))

storage_budgets = sa.Table("storage_budgets", metadata,
    sa.Column("scope", sa.String(200), primary_key=True),
    sa.Column("bytes", sa.BigInteger),
    timestamp("updated_at"))

runs = sa.Table("runs", metadata,
    identifier(primary_key=True), identifier("task_id", sa.ForeignKey(tasks.c.id), nullable=False),
    identifier("graph_version_id", sa.ForeignKey(graph_versions.c.graph_version_id), nullable=False),
    sa.Column("status", sa.String(32), nullable=False), sa.Column("current_phase", sa.Text, nullable=False),
    sa.Column("revision", sa.Integer, nullable=False), sa.Column("last_event_sequence", sa.Integer, nullable=False),
    sa.Column("workflow_version", sa.Text, nullable=False), sa.Column("context_generation", sa.Integer, nullable=False),
    sa.Column("archived_at", sa.DateTime(timezone=True)),
    timestamp("created_at"), timestamp("updated_at"))

node_runs = sa.Table("node_runs", metadata,
    identifier(primary_key=True), identifier("run_id", sa.ForeignKey(runs.c.id), nullable=False),
    sa.Column("node_id", sa.String(64), nullable=False), sa.Column("status", sa.String(32), nullable=False),
    sa.Column("attempt", sa.Integer, nullable=False), sa.Column("revision", sa.Integer, nullable=False),
    sa.Column("input_hash", sa.Text), sa.Column("output_ref", sa.Text), sa.Column("error_code", sa.Text),
    sa.Column("last_error_class", sa.String(64)),
    sa.Column("next_attempt_at", sa.DateTime(timezone=True)),
    sa.Column("context_generation", sa.Integer, nullable=False), timestamp("created_at"), timestamp("updated_at"),
    sa.UniqueConstraint("run_id", "node_id", "attempt"))
sa.Index("ix_node_runs_next_attempt_at", node_runs.c.status, node_runs.c.next_attempt_at)

edge_decisions = sa.Table("edge_decisions", metadata,
    identifier("run_id", sa.ForeignKey(runs.c.id), primary_key=True),
    sa.Column("edge_index", sa.Integer, primary_key=True),
    sa.Column("source_attempt", sa.Integer, primary_key=True, default=0),
    sa.Column("source_node_id", sa.String(64), nullable=False),
    sa.Column("target_node_id", sa.String(64), nullable=False),
    sa.Column("selected", sa.Boolean, nullable=False),
    sa.Column("reason", sa.String(32), nullable=False),
    sa.Column("condition", sa.Text),
    sa.Column("evaluator", sa.String(100), nullable=False),
    sa.Column("evaluator_version", sa.String(100), nullable=False),
    sa.Column("evaluation_context_hash", sa.String(64)),
    sa.Column("evidence_ref", sa.String(1000)),
    timestamp("decided_at"))
sa.Index("ix_edge_decisions_run_id", edge_decisions.c.run_id)

context_snapshots = sa.Table("context_snapshots", metadata,
    identifier(primary_key=True), identifier("run_id", sa.ForeignKey(runs.c.id), nullable=False),
    identifier("node_run_id", sa.ForeignKey(node_runs.c.id), nullable=False),
    sa.Column("generation", sa.Integer, nullable=False), sa.Column("input_hash", sa.String(64), nullable=False),
    sa.Column("snapshot", json_type, nullable=False), timestamp("created_at"),
    sa.UniqueConstraint("run_id", "generation"))
sa.Index("ix_context_snapshots_run_id", context_snapshots.c.run_id)

verification_records = sa.Table("verification_records", metadata,
    identifier("verification_id", primary_key=True),
    identifier("claim_id", sa.ForeignKey("node_leases.claim_id"), nullable=False, unique=True),
    identifier("run_id", sa.ForeignKey(runs.c.id), nullable=False),
    identifier("node_run_id", sa.ForeignKey(node_runs.c.id), nullable=False, unique=True),
    sa.Column("node_id", sa.String(64), nullable=False),
    sa.Column("verifier_ref", sa.String(200), nullable=False),
    sa.Column("verifier_version", sa.String(100), nullable=False),
    sa.Column("adapter", sa.String(100), nullable=False),
    sa.Column("adapter_version", sa.String(100), nullable=False),
    sa.Column("verdict", sa.String(32), nullable=False),
    sa.Column("reason", sa.Text, nullable=False),
    sa.Column("evidence_ref", sa.String(1000), nullable=False),
    sa.Column("verified_artifact_hashes", json_type, nullable=False),
    sa.Column("verified_context_hash", sa.String(64), nullable=False),
    sa.Column("model_ref", sa.String(200)),
    sa.Column("model_provider", sa.String(100)),
    sa.Column("model_name", sa.String(200)),
    sa.Column("model_response_id", sa.String(500)),
    timestamp("decided_at"))
sa.Index("ix_verification_records_run_id", verification_records.c.run_id)

events = sa.Table("events", metadata,
    identifier("stream_id", primary_key=True), sa.Column("sequence", sa.Integer, primary_key=True),
    sa.Column("event_type", sa.Text, nullable=False), sa.Column("payload", json_type, nullable=False),
    sa.Column("idempotency_key", sa.Text, nullable=False), timestamp("created_at"),
    sa.UniqueConstraint("stream_id", "idempotency_key"))

run_outbox = sa.Table("run_outbox", metadata,
    sa.Column("sequence", sa.Integer, primary_key=True, autoincrement=True),
    identifier("message_id", nullable=False, unique=True),
    identifier("run_id", sa.ForeignKey(runs.c.id), nullable=False, unique=True),
    sa.Column("envelope", json_type, nullable=False), sa.Column("acknowledged_at", sa.DateTime(timezone=True)))

run_admissions = sa.Table("run_admissions", metadata,
    identifier("trigger_id", sa.ForeignKey(triggers.c.id), primary_key=True),
    sa.Column("idempotency_key", sa.Text, primary_key=True), sa.Column("request_json", sa.Text, nullable=False),
    identifier("task_id", sa.ForeignKey(tasks.c.id), nullable=False),
    identifier("run_id", sa.ForeignKey(runs.c.id), nullable=False, unique=True),
    identifier("graph_version_id", sa.ForeignKey(graph_versions.c.graph_version_id), nullable=False),
    identifier("message_id", sa.ForeignKey(run_outbox.c.message_id), nullable=False))

execution_inbox = sa.Table("execution_inbox", metadata,
    identifier("message_id", primary_key=True),
    identifier("run_id", sa.ForeignKey(runs.c.id), nullable=False, unique=True),
    sa.Column("envelope", json_type, nullable=False), timestamp("accepted_at"))

runtime_heartbeats = sa.Table("runtime_heartbeats", metadata,
    sa.Column("component", sa.String(64), primary_key=True),
    identifier("instance_id", nullable=False), timestamp("observed_at"))

node_leases = sa.Table("node_leases", metadata,
    identifier("claim_id", primary_key=True),
    identifier("node_run_id", sa.ForeignKey(node_runs.c.id), nullable=False),
    identifier("run_id", sa.ForeignKey(runs.c.id), nullable=False),
    sa.Column("node_id", sa.String(64), nullable=False),
    sa.Column("worker_id", sa.String(128), nullable=False),
    timestamp("acquired_at"), timestamp("heartbeat_at"),
    sa.Column("released_at", sa.DateTime(timezone=True)))
sa.Index("ix_node_leases_worker_id", node_leases.c.worker_id)
sa.Index("ix_node_leases_run_id", node_leases.c.run_id)
sa.Index("uq_node_leases_active", node_leases.c.node_run_id, unique=True,
         sqlite_where=node_leases.c.released_at.is_(None),
         postgresql_where=node_leases.c.released_at.is_(None))

tool_operations = sa.Table("tool_operations", metadata,
    identifier("operation_id", primary_key=True),
    identifier("claim_id", sa.ForeignKey(node_leases.c.claim_id), nullable=False),
    identifier("node_run_id", sa.ForeignKey(node_runs.c.id), nullable=False),
    identifier("run_id", sa.ForeignKey(runs.c.id), nullable=False),
    sa.Column("tool_ref", sa.String(500), nullable=False),
    sa.Column("arguments", json_type, nullable=False),
    sa.Column("request_hash", sa.String(64), nullable=False),
    sa.Column("status", sa.String(32), nullable=False),
    sa.Column("result_ref", sa.String(1000)), sa.Column("error_code", sa.String(200)),
    sa.Column("reconciliation_ref", sa.String(1000)),
    timestamp("created_at"), timestamp("updated_at"))
sa.Index("ix_tool_operations_run_id", tool_operations.c.run_id)
sa.Index("ix_tool_operations_node_run_id", tool_operations.c.node_run_id)

graph_drafts = sa.Table("graph_drafts", metadata,
    sa.Column("graph_id", sa.String(128), primary_key=True),
    sa.Column("revision", sa.Integer, nullable=False),
    sa.Column("definition", json_type, nullable=False), sa.Column("layout", json_type, nullable=False),
    timestamp("updated_at"))

draft_publications = sa.Table("draft_publications", metadata,
    sa.Column("graph_id", sa.String(128), sa.ForeignKey(graph_drafts.c.graph_id), primary_key=True),
    sa.Column("draft_revision", sa.Integer, primary_key=True),
    identifier("graph_version_id", sa.ForeignKey(graph_versions.c.graph_version_id), nullable=False, unique=True))

progress_evidence = sa.Table("progress_evidence", metadata,
    identifier("evidence_id", primary_key=True),
    identifier("run_id", sa.ForeignKey(runs.c.id), nullable=False),
    identifier("node_run_id", sa.ForeignKey(node_runs.c.id), nullable=True),
    sa.Column("state_revision", sa.Integer, nullable=False),
    sa.Column("phase", sa.String(100), nullable=False),
    sa.Column("artifact_refs", json_type, nullable=False, server_default="[]"),
    sa.Column("verifier_passes", sa.Integer, nullable=False, server_default="0"),
    sa.Column("hypothesis_hash", sa.String(64)),
    sa.Column("tool_operation_ids", json_type, nullable=False, server_default="[]"),
    timestamp("heartbeat_at"),
    sa.Column("waiting_for", sa.String(200)),
    sa.Column("worker_expected", sa.Boolean, nullable=False, server_default=sa.text("true")),
    sa.Column("verified_progress_refs", json_type, nullable=False, server_default="[]"),
    sa.Column("cycle_iteration", sa.Integer, nullable=False, server_default="0"),
    sa.Column("cycle_fingerprint", sa.String(64)),
    timestamp("created_at"))
sa.Index("ix_progress_evidence_run_id", progress_evidence.c.run_id)
sa.Index("ix_progress_evidence_run_revision", progress_evidence.c.run_id, progress_evidence.c.state_revision)

diagnostic_requests = sa.Table("diagnostic_requests", metadata,
    identifier("diagnostic_id", primary_key=True),
    identifier("run_id", sa.ForeignKey(runs.c.id), nullable=False),
    identifier("node_run_id", sa.ForeignKey(node_runs.c.id)),
    sa.Column("reason", sa.Text, nullable=False),
    sa.Column("evidence_refs", json_type, nullable=False, server_default="[]"),
    sa.Column("suggested_actions", json_type, nullable=False, server_default="[]"),
    sa.Column("created_at", sa.DateTime(timezone=True), nullable=False),
    sa.Column("superseded_by", sa.String(36)),
    sa.Column("status", sa.String(32), nullable=False, server_default="open"),
)
sa.Index("ix_diagnostic_requests_run_id", diagnostic_requests.c.run_id)
sa.Index("ix_diagnostic_requests_status", diagnostic_requests.c.status)
