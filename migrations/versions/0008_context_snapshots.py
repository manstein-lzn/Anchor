"""Durable immutable input snapshots for replayable node execution."""

from alembic import op
import sqlalchemy as sa
from sqlalchemy.dialects import postgresql


revision = "0008_context_snapshots"
down_revision = "0007_webhook_secret"
branch_labels = None
depends_on = None


def upgrade():
    json_type = sa.JSON().with_variant(postgresql.JSONB(), "postgresql")
    op.create_table(
        "context_snapshots",
        sa.Column("id", sa.String(36), primary_key=True),
        sa.Column("run_id", sa.String(36), sa.ForeignKey("runs.id"), nullable=False),
        sa.Column("node_run_id", sa.String(36), sa.ForeignKey("node_runs.id"), nullable=False),
        sa.Column("generation", sa.Integer, nullable=False),
        sa.Column("input_hash", sa.String(64), nullable=False),
        sa.Column("snapshot", json_type, nullable=False),
        sa.Column("created_at", sa.DateTime(timezone=True), nullable=False),
        sa.UniqueConstraint("run_id", "generation"),
    )
    op.create_index("ix_context_snapshots_run_id", "context_snapshots", ["run_id"])


def downgrade():
    op.drop_index("ix_context_snapshots_run_id", table_name="context_snapshots")
    op.drop_table("context_snapshots")
