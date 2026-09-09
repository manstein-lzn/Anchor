"""One writer per workspace, enforced.

Reads are pinned to the revision a node declared, so concurrent readers are safe.
Writes are not: two nodes committing into one worktree would interleave. A
workspace therefore records the single node that currently owns its write claim;
any other writer fails closed instead of corrupting the tree.
"""

from alembic import op
import sqlalchemy as sa


revision = "0021_workspace_writer"
down_revision = "0020_prepared_revisions"
branch_labels = None
depends_on = None


def upgrade():
    with op.batch_alter_table("workspaces") as batch:
        batch.add_column(sa.Column("writer_node_run_id", sa.String(36)))


def downgrade():
    with op.batch_alter_table("workspaces") as batch:
        batch.drop_column("writer_node_run_id")
