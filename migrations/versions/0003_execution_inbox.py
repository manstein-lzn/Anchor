"""Durable execution inbox for accepted run dispatches."""

from alembic import op
import sqlalchemy as sa
from sqlalchemy.dialects import postgresql

revision = "0003"
down_revision = "0002"
branch_labels = None
depends_on = None


def upgrade():
    json_type = sa.JSON().with_variant(postgresql.JSONB(), "postgresql")
    op.create_table("execution_inbox",
        sa.Column("message_id", sa.String(36), primary_key=True),
        sa.Column("run_id", sa.String(36), sa.ForeignKey("runs.id"), nullable=False, unique=True),
        sa.Column("envelope", json_type, nullable=False),
        sa.Column("accepted_at", sa.DateTime(timezone=True), nullable=False))
    op.create_table("runtime_heartbeats",
        sa.Column("component", sa.String(64), primary_key=True),
        sa.Column("instance_id", sa.String(36), nullable=False),
        sa.Column("observed_at", sa.DateTime(timezone=True), nullable=False))


def downgrade():
    op.drop_table("runtime_heartbeats")
    op.drop_table("execution_inbox")
