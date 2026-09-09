"""Prepared content revisions: the window between freeze and control commit.

A revision can be frozen and persisted before the control event that makes it
part of the recovery closure. Recording that window lets a reconciler finish or
discard the commit instead of guessing content from a live workspace.
"""

from alembic import op
import sqlalchemy as sa


revision = "0020_prepared_revisions"
down_revision = "0019_workspaces"
branch_labels = None
depends_on = None


def upgrade():
    op.create_table(
        "prepared_revisions",
        sa.Column("node_run_id", sa.String(36), primary_key=True),
        sa.Column("attempt", sa.Integer, primary_key=True),
        sa.Column("run_id", sa.String(36), nullable=False),
        sa.Column("workspace_id", sa.String(200), nullable=False),
        sa.Column("revision", sa.String(200), nullable=False),
        sa.Column("manifest_digest", sa.String(64), nullable=False),
        sa.Column("verifier_result", sa.JSON().with_variant(
            sa.dialects.postgresql.JSONB(), "postgresql")),
        sa.Column("created_at", sa.DateTime(timezone=True), nullable=False),
    )


def downgrade():
    op.drop_table("prepared_revisions")
