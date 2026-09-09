"""Run-scoped writable worktrees with an auditable operation ledger.

A workspace is not a source of truth. Only the revisions it produces, once a
control event references them, enter the recovery closure.
"""

from alembic import op
import sqlalchemy as sa


revision = "0019_workspaces"
down_revision = "0018_projects"
branch_labels = None
depends_on = None


def upgrade():
    op.create_table(
        "workspaces",
        sa.Column("workspace_id", sa.String(200), primary_key=True),
        sa.Column("project_id", sa.String(200), nullable=False),
        sa.Column("base_revision", sa.String(200), nullable=False),
        sa.Column("branch", sa.String(300), nullable=False),
        sa.Column("path", sa.String(1000), nullable=False),
        sa.Column("state", sa.String(32), nullable=False),
        sa.Column("current_revision", sa.String(200)),
        sa.Column("created_at", sa.DateTime(timezone=True), nullable=False),
        sa.Column("updated_at", sa.DateTime(timezone=True), nullable=False),
    )
    op.create_table(
        "workspace_operations",
        sa.Column("operation_id", sa.String(36), primary_key=True),
        sa.Column("workspace_id", sa.String(200), nullable=False),
        sa.Column("kind", sa.String(32), nullable=False),
        sa.Column("path", sa.String(1000)),
        sa.Column("before_revision", sa.String(200)),
        sa.Column("after_revision", sa.String(200)),
        sa.Column("content_hash", sa.String(64)),
        sa.Column("actor", sa.String(200), nullable=False),
        sa.Column("created_at", sa.DateTime(timezone=True), nullable=False),
    )
    op.create_index("ix_workspace_operations_workspace_id", "workspace_operations",
                    ["workspace_id", "created_at"])


def downgrade():
    op.drop_index("ix_workspace_operations_workspace_id", table_name="workspace_operations")
    op.drop_table("workspace_operations")
    op.drop_table("workspaces")
