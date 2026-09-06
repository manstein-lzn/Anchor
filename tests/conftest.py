"""Shared fixtures: the single relational store on throwaway migrated SQLite.

There is exactly one state implementation (`anchor.state.relational`); the old
raw-sqlite prototype is deleted. Unit tests that need canonical state use a
fresh Alembic-migrated SQLite file per test through this module.
"""

from pathlib import Path

import pytest

sa = pytest.importorskip("sqlalchemy")
pytest.importorskip("alembic")
from alembic import command
from alembic.config import Config

from anchor.state.relational import RelationalStateStore

ROOT = Path(__file__).resolve().parents[1]


def migrate(url: str) -> None:
    config = Config(str(ROOT / "alembic.ini"))
    config.set_main_option("script_location", str(ROOT / "migrations"))
    config.attributes["database_url"] = url
    command.upgrade(config, "head")


def make_store(tmp_path: Path, name: str = "anchor.sqlite") -> RelationalStateStore:
    """Create a migrated throwaway store. Caller (or fixture) owns closing."""
    url = f"sqlite:///{tmp_path / name}"
    migrate(url)
    return RelationalStateStore(url)


def store_url(store: RelationalStateStore) -> str:
    return str(store.engine.url)


@pytest.fixture
def store(tmp_path):
    instance = make_store(tmp_path)
    try:
        yield instance
    finally:
        instance.close()
