"""Disposable real API for browser tests; never opens the development database."""

import asyncio
import os
import json
import threading
import time
from pathlib import Path
from tempfile import TemporaryDirectory

import uvicorn
from alembic import command
from alembic.config import Config

from anchor.api.app import create_app
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.receiver import DurableExecutionReceiver


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

        def pump():
            """Accept admitted runs so the browser sees queued/running states.

            There is deliberately no model worker here: nodes stay ready, so the
            UI exercises real admission and operator controls without any
            provider call.
            """
            while True:
                time.sleep(0.3)
                store = getattr(app.state, "store", None)
                if store is None:
                    continue
                try:
                    asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
                except Exception:
                    pass

        threading.Thread(target=pump, daemon=True).start()
        uvicorn.run(app, host="127.0.0.1", port=8091)
