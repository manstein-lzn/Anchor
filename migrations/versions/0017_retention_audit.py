"""Audit rolling retention: what the budget sweep evicted and when.

Deleting finished history is irreversible, so every sweep records counts and
freed bytes. The audit holds no run content: the evidence of a purged run is
gone by definition; only the fact and size of the action survive.
"""

from alembic import op
import sqlalchemy as sa
from sqlalchemy.dialects import postgresql
from sqlalchemy.dialects import postgresql


revision = "0017_retention_audit"
down_revision = "0016_storage_budgets"
branch_labels = None
depends_on = None


def upgrade():
    op.create_table(
        "retention_audit",
        sa.Column("audit_id", sa.String(36), primary_key=True),
        sa.Column("created_at", sa.DateTime(timezone=True), nullable=False),
        sa.Column("trigger", sa.String(64), nullable=False),
        sa.Column("evicted_runs", sa.Integer, nullable=False),
        sa.Column("freed_bytes", sa.BigInteger, nullable=False),
        sa.Column("detail", sa.JSON().with_variant(postgresql.JSONB(), "postgresql"),
                  nullable=False),
    )


def downgrade():
    op.drop_table("retention_audit")
