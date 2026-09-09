"""Persist adjustable storage budgets for the whole install and per graph.

A budget is a monitoring target, not an execution limit: it never terminates a
running node. Storing it in the database lets operators adjust it at runtime
through the API without restarting any service.
"""

from alembic import op
import sqlalchemy as sa


revision = "0016_storage_budgets"
down_revision = "0015_run_archive"
branch_labels = None
depends_on = None


def upgrade():
    op.create_table(
        "storage_budgets",
        sa.Column("scope", sa.String(200), primary_key=True),
        sa.Column("bytes", sa.BigInteger),
        sa.Column("updated_at", sa.DateTime(timezone=True), nullable=False),
    )


def downgrade():
    op.drop_table("storage_budgets")
