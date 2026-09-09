"""Register read-only content sources the workspace resolver can read from.

A project is a long-lived repository, not a workspace. It grants read access at
immutable revisions only; the default revision is a convenience and is never
recorded in a content reference.
"""

from alembic import op
import sqlalchemy as sa


revision = "0018_projects"
down_revision = "0017_retention_audit"
branch_labels = None
depends_on = None


def upgrade():
    op.create_table(
        "projects",
        sa.Column("project_id", sa.String(200), primary_key=True),
        sa.Column("name", sa.String(200), nullable=False),
        sa.Column("backend", sa.String(32), nullable=False),
        sa.Column("root", sa.String(1000), nullable=False),
        sa.Column("default_revision", sa.String(200)),
        sa.Column("created_at", sa.DateTime(timezone=True), nullable=False),
        sa.Column("updated_at", sa.DateTime(timezone=True), nullable=False),
    )


def downgrade():
    op.drop_table("projects")
