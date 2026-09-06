from alembic import op
import sqlalchemy as sa

revision = "0007_webhook_secret"
down_revision = "0006"
branch_labels = None
depends_on = None

def upgrade():
    op.add_column("triggers", sa.Column("webhook_secret_ref", sa.String(200)))

def downgrade():
    op.drop_column("triggers", "webhook_secret_ref")
