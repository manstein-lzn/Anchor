import os

from alembic import context
from sqlalchemy import create_engine, pool

from anchor.state.schema import metadata


url = context.config.attributes.get("database_url") or os.environ.get("ANCHOR_DATABASE_URL")
if not url:
    raise RuntimeError("set ANCHOR_DATABASE_URL explicitly; migrations have no default target")

if context.is_offline_mode():
    context.configure(url=url, target_metadata=metadata, literal_binds=True, compare_type=True)
    with context.begin_transaction():
        context.run_migrations()
else:
    engine = create_engine(url, poolclass=pool.NullPool)
    try:
        with engine.connect() as connection:
            context.configure(connection=connection, target_metadata=metadata, compare_type=True)
            with context.begin_transaction():
                context.run_migrations()
    finally:
        engine.dispose()
