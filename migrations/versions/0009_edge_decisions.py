"""Persist immutable per-Run edge routing decisions."""

from alembic import op
import sqlalchemy as sa


revision = "0009_edge_decisions"
down_revision = "0008_context_snapshots"
branch_labels = None
depends_on = None


def upgrade():
    op.create_table(
        "edge_decisions",
        sa.Column("run_id", sa.String(36), sa.ForeignKey("runs.id"), primary_key=True),
        sa.Column("edge_index", sa.Integer, primary_key=True),
        sa.Column("source_node_id", sa.String(64), nullable=False),
        sa.Column("target_node_id", sa.String(64), nullable=False),
        sa.Column("selected", sa.Boolean, nullable=False),
        sa.Column("reason", sa.String(32), nullable=False),
        sa.Column("condition", sa.Text),
        sa.Column("evaluator", sa.String(100), nullable=False),
        sa.Column("evaluator_version", sa.String(100), nullable=False),
        sa.Column("evaluation_context_hash", sa.String(64)),
        sa.Column("evidence_ref", sa.String(1000)),
        sa.Column("decided_at", sa.DateTime(timezone=True), nullable=False),
    )
    op.create_index("ix_edge_decisions_run_id", "edge_decisions", ["run_id"])


def downgrade():
    op.drop_index("ix_edge_decisions_run_id", table_name="edge_decisions")
    op.drop_table("edge_decisions")
