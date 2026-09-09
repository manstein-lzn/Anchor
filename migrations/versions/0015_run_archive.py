"""Let operators hide terminal runs without destroying evidence.

Runs are the root of the evidence chain, so the operator action is a
reversible archive, never a delete. Archived runs stay fully queryable by id
and remain visible with `include_archived=true`; retention/purge stays a
separate, explicitly audited concern.
"""

from alembic import op
import sqlalchemy as sa


revision = "0015_run_archive"
down_revision = "0014_recovery_schedule"
branch_labels = None
depends_on = None


def upgrade():
    with op.batch_alter_table("runs") as batch:
        batch.add_column(sa.Column("archived_at", sa.DateTime(timezone=True)))


def downgrade():
    with op.batch_alter_table("runs") as batch:
        batch.drop_column("archived_at")
