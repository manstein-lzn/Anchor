"""Retain released leases referenced by tool and verification evidence."""

from alembic import op
import sqlalchemy as sa


revision = "0012_lease_history"
down_revision = "0011_decision_attempts"
branch_labels = None
depends_on = None


def upgrade():
    with op.batch_alter_table("node_leases") as batch:
        batch.drop_constraint("uq_node_leases_node_run_id", type_="unique")
    op.create_index("uq_node_leases_active", "node_leases", ["node_run_id"], unique=True,
                    sqlite_where=sa.text("released_at IS NULL"),
                    postgresql_where=sa.text("released_at IS NULL"))


def downgrade():
    duplicates = op.get_bind().execute(sa.text(
        "SELECT node_run_id FROM node_leases GROUP BY node_run_id HAVING COUNT(*) > 1 LIMIT 1"
    )).first()
    if duplicates:
        raise RuntimeError("Cannot downgrade without losing lease history; retain the current schema")
    op.drop_index("uq_node_leases_active", table_name="node_leases")
    with op.batch_alter_table("node_leases") as batch:
        batch.create_unique_constraint("uq_node_leases_node_run_id", ["node_run_id"])
