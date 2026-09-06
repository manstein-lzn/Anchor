"""Evidence reference for reconciling unknown tool outcomes."""

from alembic import op
import sqlalchemy as sa

revision = "0006"
down_revision = "0005"
branch_labels = None
depends_on = None


def upgrade():
    op.add_column("tool_operations", sa.Column("reconciliation_ref", sa.String(1000)))


def downgrade():
    op.drop_column("tool_operations", "reconciliation_ref")
