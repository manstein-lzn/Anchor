"""Persist transient-failure recovery schedule on node attempts.

A fault retry is a recovery action, not a business cycle. Persisting the error
class and the earliest next attempt time makes the plan survive a worker or
supervisor restart instead of relying on an in-process sleep.
"""

from alembic import op
import sqlalchemy as sa


revision = "0014_recovery_schedule"
down_revision = "0013_progress_evidence"
branch_labels = None
depends_on = None


def upgrade():
    with op.batch_alter_table("node_runs") as batch:
        batch.add_column(sa.Column("last_error_class", sa.String(64)))
        batch.add_column(sa.Column("next_attempt_at", sa.DateTime(timezone=True)))
    op.create_index("ix_node_runs_next_attempt_at", "node_runs", ["status", "next_attempt_at"])


def downgrade():
    op.drop_index("ix_node_runs_next_attempt_at", table_name="node_runs")
    with op.batch_alter_table("node_runs") as batch:
        batch.drop_column("next_attempt_at")
        batch.drop_column("last_error_class")
