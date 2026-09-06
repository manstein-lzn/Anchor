# PostgreSQL Storage

## Scope

`anchor.state.relational.RelationalStateStore` implements the state and admission
contracts using SQLAlchemy Core and psycopg. PostgreSQL JSONB stores structured graph
definitions, inputs and event payloads; identity, status, revision, references and
timestamps have relational columns and constraints. The same adapter can use a fresh
SQLite database for fast contract tests.

`RelationalStateStore` is the single state implementation for both PostgreSQL
and SQLite development databases. The old raw-sqlite prototype was deleted;
do not reuse its files. No existing database is copied, upgraded, cleared or
stamped automatically.

## Local setup

```bash
cd /home/mansteinl/Anchor
.venv/bin/pip install -e '.[dev,storage]'
```

Set `ANCHOR_POSTGRES_PASSWORD` in your shell without committing it. Start the optional
development service after installing the Docker Compose plugin (`docker compose
version` must succeed):

```bash
docker compose -f infra/compose.postgres.yaml up -d --wait
```

It binds only to `127.0.0.1:55432`; override `ANCHOR_POSTGRES_PORT` if occupied.
Data lives in the Compose project's named volume. Ordinary `docker compose down`
preserves it; `down -v` deletes it and should not be used for routine shutdown.

Set `ANCHOR_DATABASE_URL` to a SQLAlchemy URL for the intended database, of the form
`postgresql+psycopg://anchor:<url-encoded-password>@127.0.0.1:55432/anchor`.
Special characters in passwords must be URL-encoded. Then explicitly migrate:

```bash
.venv/bin/alembic upgrade head
.venv/bin/alembic current
.venv/bin/alembic check
```

The adapter does not create tables on startup. Revision `0001` creates the new
schema; it is not an importer for old SQLite data. Initial downgrade drops all
Anchor tables, so only use it in disposable databases. Back up production data and
review migrations before applying them. The Compose role is a local development
superuser, not a production least-privilege configuration.

On the current workstation Docker Engine is available but the Compose plugin is not
installed. The PostgreSQL contract suite was verified with an isolated `docker run`
container instead. The Compose file is provided for setup but has not been launched
or validated by Compose on this machine.

## Concurrency and delivery

- A stable transaction-scoped advisory lock serializes an occurrence's receipt
  check and insertion. Database uniqueness constraints are the final backstop.
- Graph publication locks its graph/version key. Event writes lock their stream.
- Admission also locks its trigger row while reading enablement/version binding.
  These locks cover database transactions, never model or tool execution.
- Task, Run, pending nodes, events, receipt and outbox commit together.
- Dispatch remains at-least-once and currently single-dispatcher. The real workflow
  target still needs durable acceptance/deduplication. No network call is made inside
  the admission transaction.

All cooperating writers must use the adapter's locking contract. This is not
protection against arbitrary SQL updates by database administrators, and not an
exactly-once guarantee for external tools.

## Verification

```bash
.venv/bin/python -m pytest -m 'not postgres'
```

For PostgreSQL tests, set `ANCHOR_TEST_POSTGRES_URL` to a disposable database using
the `postgresql+psycopg` driver, then run:

```bash
.venv/bin/python -m pytest -ra
```

The test role needs schema creation privileges. Each PostgreSQL test creates a unique
`anchor_test_<uuid>` schema, applies real Alembic migrations, and drops only that
schema on completion. Never configure the test URL against a production database.
Without the test URL, PostgreSQL cases explicitly skip rather than pretending to pass.

The shared relational suite covers publication, version pinning, node membership,
duplicate/conflicting occurrences, concurrent writers, event ordering, rollback,
process exit before/after commit, outbox redelivery and migration roundtrips.
It does not establish database failover, workflow recovery, power-loss durability,
tenant isolation or long-horizon Agent quality.
