"""Persist progress evidence and diagnostic requests for adaptive execution."""

from alembic import op
import sqlalchemy as sa
from sqlalchemy.dialects import postgresql


revision = "0013_progress_evidence"
down_revision = "0012_lease_history"
branch_labels = None
depends_on = None


def upgrade():
    json_type = sa.JSON().with_variant(postgresql.JSONB(), "postgresql")

    op.create_table(
        "progress_evidence",
        sa.Column("evidence_id", sa.String(36), primary_key=True),
        sa.Column("run_id", sa.String(36), sa.ForeignKey("runs.id"), nullable=False),
        sa.Column("node_run_id", sa.String(36), sa.ForeignKey("node_runs.id")),
        sa.Column("state_revision", sa.Integer, nullable=False),
        sa.Column("phase", sa.String(100), nullable=False),
        sa.Column("artifact_refs", json_type, nullable=False, server_default="[]"),
        sa.Column("verifier_passes", sa.Integer, nullable=False, server_default="0"),
        sa.Column("hypothesis_hash", sa.String(64)),
        sa.Column("tool_operation_ids", json_type, nullable=False, server_default="[]"),
        sa.Column("heartbeat_at", sa.DateTime(timezone=True), nullable=False),
        sa.Column("waiting_for", sa.String(200)),
        sa.Column("worker_expected", sa.Boolean, nullable=False, server_default=sa.text("true")),
        sa.Column("verified_progress_refs", json_type, nullable=False, server_default="[]"),
        sa.Column("cycle_iteration", sa.Integer, nullable=False, server_default="0"),
        sa.Column("cycle_fingerprint", sa.String(64)),
        sa.Column("created_at", sa.DateTime(timezone=True), nullable=False),
    )
    op.create_index("ix_progress_evidence_run_id", "progress_evidence", ["run_id"])
    op.create_index("ix_progress_evidence_run_revision", "progress_evidence", ["run_id", "state_revision"])

    op.create_table(
        "diagnostic_requests",
        sa.Column("diagnostic_id", sa.String(36), primary_key=True),
        sa.Column("run_id", sa.String(36), sa.ForeignKey("runs.id"), nullable=False),
        sa.Column("node_run_id", sa.String(36), sa.ForeignKey("node_runs.id")),
        sa.Column("reason", sa.Text, nullable=False),
        sa.Column("evidence_refs", json_type, nullable=False, server_default="[]"),
        sa.Column("suggested_actions", json_type, nullable=False, server_default="[]"),
        sa.Column("created_at", sa.DateTime(timezone=True), nullable=False),
        sa.Column("superseded_by", sa.String(36)),
        sa.Column("status", sa.String(32), nullable=False, server_default="open"),
    )
    op.create_index("ix_diagnostic_requests_run_id", "diagnostic_requests", ["run_id"])
    op.create_index("ix_diagnostic_requests_status", "diagnostic_requests", ["status"])


def downgrade():
    op.drop_index("ix_diagnostic_requests_status", table_name="diagnostic_requests")
    op.drop_index("ix_diagnostic_requests_run_id", table_name="diagnostic_requests")
    op.drop_table("diagnostic_requests")

    op.drop_index("ix_progress_evidence_run_revision", table_name="progress_evidence")
    op.drop_index("ix_progress_evidence_run_id", table_name="progress_evidence")
    op.drop_table("progress_evidence")
