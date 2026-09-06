"""Scope edge decisions by source attempt so loop iterations can re-decide."""

from alembic import op
import sqlalchemy as sa


revision = "0011_decision_attempts"
down_revision = "0010_verification_records"
branch_labels = None
depends_on = None

COLUMNS = (
    "run_id", "edge_index", "source_attempt", "source_node_id",
    "target_node_id", "selected", "reason", "condition", "evaluator",
    "evaluator_version", "evaluation_context_hash", "evidence_ref",
    "decided_at",
)


def upgrade():
    bind = op.get_bind()
    if bind.dialect.name == "postgresql":
        op.add_column("edge_decisions", sa.Column("source_attempt", sa.Integer(), nullable=True))
        op.execute("UPDATE edge_decisions SET source_attempt = 0 WHERE source_attempt IS NULL")
        op.alter_column("edge_decisions", "source_attempt",
                        existing_type=sa.Integer(), nullable=False)
        op.drop_constraint("pk_edge_decisions", "edge_decisions", type_="primary")
        op.create_primary_key("pk_edge_decisions", "edge_decisions",
                              ["run_id", "edge_index", "source_attempt"])
        return
    # SQLite cannot alter primary keys: rebuild explicitly (indexes included).
    op.create_table(
        "_edge_decisions_new",
        sa.Column("run_id", sa.String(36), nullable=False),
        sa.Column("edge_index", sa.Integer(), nullable=False),
        sa.Column("source_attempt", sa.Integer(), nullable=False,
                  server_default="0"),
        sa.Column("source_node_id", sa.String(64), nullable=False),
        sa.Column("target_node_id", sa.String(64), nullable=False),
        sa.Column("selected", sa.Boolean(), nullable=False),
        sa.Column("reason", sa.String(32), nullable=False),
        sa.Column("condition", sa.Text(), nullable=True),
        sa.Column("evaluator", sa.String(100), nullable=False),
        sa.Column("evaluator_version", sa.String(100), nullable=False),
        sa.Column("evaluation_context_hash", sa.String(64), nullable=True),
        sa.Column("evidence_ref", sa.String(1000), nullable=True),
        sa.Column("decided_at", sa.DateTime(timezone=True), nullable=False),
        sa.PrimaryKeyConstraint("run_id", "edge_index", "source_attempt",
                                name="pk_edge_decisions"),
        sa.ForeignKeyConstraint(["run_id"], ["runs.id"],
                                name="fk_edge_decisions_run_id_runs"),
    )
    rest = [c for c in COLUMNS if c not in ("run_id", "edge_index", "source_attempt")]
    op.execute(
        "INSERT INTO _edge_decisions_new (%s) SELECT run_id, edge_index, 0, %s"
        " FROM edge_decisions" % (", ".join(COLUMNS), ", ".join(rest)))
    op.drop_table("edge_decisions")
    op.rename_table("_edge_decisions_new", "edge_decisions")
    op.create_index("ix_edge_decisions_run_id", "edge_decisions", ["run_id"])


def downgrade():
    bind = op.get_bind()
    if bind.dialect.name == "postgresql":
        op.drop_constraint("pk_edge_decisions", "edge_decisions", type_="primary")
        op.create_primary_key("pk_edge_decisions", "edge_decisions",
                              ["run_id", "edge_index"])
        op.drop_column("edge_decisions", "source_attempt")
        return
    op.create_table(
        "_edge_decisions_old",
        sa.Column("run_id", sa.String(36), nullable=False),
        sa.Column("edge_index", sa.Integer(), nullable=False),
        sa.Column("source_node_id", sa.String(64), nullable=False),
        sa.Column("target_node_id", sa.String(64), nullable=False),
        sa.Column("selected", sa.Boolean(), nullable=False),
        sa.Column("reason", sa.String(32), nullable=False),
        sa.Column("condition", sa.Text(), nullable=True),
        sa.Column("evaluator", sa.String(100), nullable=False),
        sa.Column("evaluator_version", sa.String(100), nullable=False),
        sa.Column("evaluation_context_hash", sa.String(64), nullable=True),
        sa.Column("evidence_ref", sa.String(1000), nullable=True),
        sa.Column("decided_at", sa.DateTime(timezone=True), nullable=False),
        sa.PrimaryKeyConstraint("run_id", "edge_index", name="pk_edge_decisions"),
        sa.ForeignKeyConstraint(["run_id"], ["runs.id"],
                                name="fk_edge_decisions_run_id_runs"),
    )
    op.execute(
        "INSERT INTO _edge_decisions_old (%s) SELECT %s FROM edge_decisions" % (
            ", ".join(c for c in COLUMNS if c != "source_attempt"),
            ", ".join(c for c in COLUMNS if c != "source_attempt"),
        ))
    op.drop_table("edge_decisions")
    op.rename_table("_edge_decisions_old", "edge_decisions")
    op.create_index("ix_edge_decisions_run_id", "edge_decisions", ["run_id"])
