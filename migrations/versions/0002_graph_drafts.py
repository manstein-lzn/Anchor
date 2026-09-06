"""Versioned editor drafts and publication receipts."""

from alembic import op
import sqlalchemy as sa
from sqlalchemy.dialects import postgresql

revision = "0002"
down_revision = "0001"
branch_labels = None
depends_on = None


def upgrade():
    json_type = sa.JSON().with_variant(postgresql.JSONB(), "postgresql")
    op.create_table("graph_drafts",
        sa.Column("graph_id", sa.String(128), primary_key=True),
        sa.Column("revision", sa.Integer, nullable=False),
        sa.Column("definition", json_type, nullable=False),
        sa.Column("layout", json_type, nullable=False),
        sa.Column("updated_at", sa.DateTime(timezone=True), nullable=False))
    op.create_table("draft_publications",
        sa.Column("graph_id", sa.String(128), sa.ForeignKey("graph_drafts.graph_id"), primary_key=True),
        sa.Column("draft_revision", sa.Integer, primary_key=True),
        sa.Column("graph_version_id", sa.String(36), sa.ForeignKey("graph_versions.graph_version_id"),
                  nullable=False, unique=True))


def downgrade():
    op.drop_table("draft_publications")
    op.drop_table("graph_drafts")
