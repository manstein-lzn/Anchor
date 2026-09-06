"""Durable tool operation ledger with explicit unknown outcomes."""

from alembic import op
import sqlalchemy as sa
from sqlalchemy.dialects import postgresql

revision = "0005"
down_revision = "0004"
branch_labels = None
depends_on = None


def upgrade():
    json_type = sa.JSON().with_variant(postgresql.JSONB(), "postgresql")
    op.create_table("tool_operations",
        sa.Column("operation_id", sa.String(36), primary_key=True),
        sa.Column("claim_id", sa.String(36), sa.ForeignKey("node_leases.claim_id"), nullable=False),
        sa.Column("node_run_id", sa.String(36), sa.ForeignKey("node_runs.id"), nullable=False),
        sa.Column("run_id", sa.String(36), sa.ForeignKey("runs.id"), nullable=False),
        sa.Column("tool_ref", sa.String(500), nullable=False),
        sa.Column("arguments", json_type, nullable=False),
        sa.Column("request_hash", sa.String(64), nullable=False),
        sa.Column("status", sa.String(32), nullable=False),
        sa.Column("result_ref", sa.String(1000)),
        sa.Column("error_code", sa.String(200)),
        sa.Column("created_at", sa.DateTime(timezone=True), nullable=False),
        sa.Column("updated_at", sa.DateTime(timezone=True), nullable=False))
    op.create_index("ix_tool_operations_run_id", "tool_operations", ["run_id"])
    op.create_index("ix_tool_operations_node_run_id", "tool_operations", ["node_run_id"])


def downgrade():
    op.drop_index("ix_tool_operations_node_run_id", table_name="tool_operations")
    op.drop_index("ix_tool_operations_run_id", table_name="tool_operations")
    op.drop_table("tool_operations")
