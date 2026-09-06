"""Persist verifier verdicts and their exact evidence bindings."""

from alembic import op
import sqlalchemy as sa
from sqlalchemy.dialects import postgresql


revision = "0010_verification_records"
down_revision = "0009_edge_decisions"
branch_labels = None
depends_on = None


def upgrade():
    json_type = sa.JSON().with_variant(postgresql.JSONB(), "postgresql")
    op.create_table(
        "verification_records",
        sa.Column("verification_id", sa.String(36), primary_key=True),
        sa.Column("claim_id", sa.String(36), sa.ForeignKey("node_leases.claim_id"), nullable=False, unique=True),
        sa.Column("run_id", sa.String(36), sa.ForeignKey("runs.id"), nullable=False),
        sa.Column("node_run_id", sa.String(36), sa.ForeignKey("node_runs.id"), nullable=False, unique=True),
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
        sa.Column("decided_at", sa.DateTime(timezone=True), nullable=False),
    )
    op.create_index("ix_verification_records_run_id", "verification_records", ["run_id"])


def downgrade():
    op.drop_index("ix_verification_records_run_id", table_name="verification_records")
    op.drop_table("verification_records")
