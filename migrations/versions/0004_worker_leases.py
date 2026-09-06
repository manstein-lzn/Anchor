"""Stable worker claims without automatic lease stealing."""

from alembic import op
import sqlalchemy as sa

revision = "0004"
down_revision = "0003"
branch_labels = None
depends_on = None


def upgrade():
    op.create_table("node_leases",
        sa.Column("claim_id", sa.String(36), primary_key=True),
        sa.Column("node_run_id", sa.String(36), sa.ForeignKey("node_runs.id"), nullable=False, unique=True),
        sa.Column("run_id", sa.String(36), sa.ForeignKey("runs.id"), nullable=False),
        sa.Column("node_id", sa.String(64), nullable=False),
        sa.Column("worker_id", sa.String(128), nullable=False),
        sa.Column("acquired_at", sa.DateTime(timezone=True), nullable=False),
        sa.Column("heartbeat_at", sa.DateTime(timezone=True), nullable=False),
        sa.Column("released_at", sa.DateTime(timezone=True)))
    op.create_index("ix_node_leases_worker_id", "node_leases", ["worker_id"])
    op.create_index("ix_node_leases_run_id", "node_leases", ["run_id"])


def downgrade():
    op.drop_index("ix_node_leases_run_id", table_name="node_leases")
    op.drop_index("ix_node_leases_worker_id", table_name="node_leases")
    op.drop_table("node_leases")
