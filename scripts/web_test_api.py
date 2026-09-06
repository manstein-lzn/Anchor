"""Disposable real API for browser tests; never opens the development database."""

import os
import json
from pathlib import Path
from tempfile import TemporaryDirectory

import uvicorn
from alembic import command
from alembic.config import Config

from anchor.api.app import create_app


if __name__ == "__main__":
    root = Path(__file__).resolve().parents[1]
    with TemporaryDirectory(prefix="anchor-web-e2e-") as directory:
        os.environ["ANCHOR_DATABASE_URL"] = f"sqlite:///{directory}/state.sqlite"
        # Browser tests exercise capability validation and must use a
        # credential-free profile that mirrors the references used by their
        # graph fixtures. No provider call is made by this API process.
        runtime = Path(directory) / "runtime.json"
        runtime.write_text(json.dumps({
            "secret_file": None,
            "models": [{"ref": "models.browser-test", "provider": "test",
                         "model": "browser-test", "secret_ref": "TEST_ONLY"}],
            "agents": [
                {"ref": "agents.researcher", "model_ref": "models.browser-test"},
                {"ref": "agents.reviewer", "model_ref": "models.browser-test"},
            ],
            "tools": [],
            "verifiers": [
                {"ref": "verifiers.evidence", "version": "v1", "adapter": "model",
                 "model_ref": "models.browser-test", "instructions": "test only"},
            ],
        }), encoding="utf-8")
        os.environ["ANCHOR_RUNTIME_CONFIG"] = str(runtime)
        config = Config(str(root / "alembic.ini"))
        config.set_main_option("script_location", str(root / "migrations"))
        command.upgrade(config, "head")
        app = create_app(token="anchor-browser-tests-only-not-a-real-secret")
        uvicorn.run(app, host="127.0.0.1", port=8091)
