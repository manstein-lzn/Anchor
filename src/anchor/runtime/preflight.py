"""Refuse to start against a broken environment, and say exactly what is broken.

A service that starts with a bad environment does not fail visibly. It either crashes in
a restart loop that systemd reports as ``activating``, or it starts and silently does
nothing useful. Both look healthy from outside, which is the worst property a startup path
can have: the operator sees a running unit and no work happening.

So every service checks its environment before entering its loop and exits immediately with
a structured reason when the environment cannot support it. The check is deliberately about
*preconditions*, not about reachability of other Anchor services — no service here depends
on another one. Workers claim nodes from the store; none of them calls the API.

What is checked:

- the database URL is configured, with no silent fallback to a developer default
- the schema is migrated to a version this build understands
- the runtime profile parses, when the service needs one
- the artifact root is writable, when the service writes artifacts

Exit code 2 marks an environment failure, distinct from a crash.
"""

from __future__ import annotations

import json
import logging
import os
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

logger = logging.getLogger("anchor.preflight")

#: Distinct from 1 so a supervisor can tell "the environment is wrong" from "it crashed".
ENVIRONMENT_EXIT_CODE = 2


@dataclass(frozen=True)
class Problem:
    """One reason this process must not start."""

    code: str
    detail: str

    def as_dict(self) -> dict[str, str]:
        return {"code": self.code, "detail": self.detail}


def check_database(url: str | None, *, store_factory: Any = None) -> list[Problem]:
    """The URL is configured and the schema is one this build understands.

    No fallback. A missing URL used to resolve to a developer's local SQLite file, which
    meant a misconfigured production unit wrote to a path nobody was looking at instead of
    refusing to start.
    """
    if not url:
        return [Problem("database_url_missing",
                        "ANCHOR_DATABASE_URL is not set; refusing to guess a default")]
    if store_factory is None:
        from anchor.state.relational import RelationalStateStore

        store_factory = RelationalStateStore
    try:
        store = store_factory(url)
    except Exception as exc:  # noqa: BLE001 - any failure here means "cannot start"
        return [Problem("database_unreachable", f"{type(exc).__name__}: {exc}")]
    try:
        ping = getattr(store, "ping", None)
        if callable(ping):
            ping()
    except Exception as exc:  # noqa: BLE001 - reported, never raised past the caller
        return [Problem("database_unreachable",
                        f"ANCHOR_DATABASE_URL does not point at a reachable database: "
                        f"{type(exc).__name__}: {exc}")]
    try:
        store.check_schema()
    except Exception as exc:  # noqa: BLE001 - reported, never raised past the caller
        # Reachable but not migrated. Checked separately from the ping above so that a
        # mistyped path is never reported as "run the migrations".
        return [Problem("schema_not_migrated",
                        f"the database is reachable but its schema is not current; run "
                        f"`alembic upgrade head` against it: "
                        f"{type(exc).__name__}: {exc}")]
    finally:
        close = getattr(store, "close", None)
        if callable(close):
            close()
    return []


def check_runtime_config(path: str) -> list[Problem]:
    """The runtime profile exists and parses, for services that resolve capabilities."""
    selected = Path(path).expanduser()
    if not selected.is_file():
        return [Problem("runtime_config_missing", f"ANCHOR_RUNTIME_CONFIG is absent: {selected}")]
    try:
        from anchor.runtime.config import load_runtime_config

        config = load_runtime_config(str(selected))
    except Exception as exc:  # noqa: BLE001 - reported
        return [Problem("runtime_config_invalid", f"{type(exc).__name__}: {exc}")]
    if not config.models:
        return [Problem("runtime_config_empty",
                        f"{selected} declares no models, so no node could ever be executed")]
    return []


def check_artifacts(root: str) -> list[Problem]:
    """The artifact root can be created and written to."""
    target = Path(root).expanduser()
    try:
        target.mkdir(mode=0o700, parents=True, exist_ok=True)
        probe = target / ".preflight"
        probe.write_text("ok", encoding="utf-8")
        probe.unlink()
    except OSError as exc:
        return [Problem("artifact_root_unwritable", f"{target}: {exc}")]
    return []


def check_environment(*, role: str, database_url: str | None,
                      runtime_config: str | None = None,
                      artifact_root: str | None = None) -> list[Problem]:
    """Every precondition for ``role``, in the order an operator would fix them."""
    problems = check_database(database_url)
    if runtime_config is not None:
        problems += check_runtime_config(runtime_config)
    if artifact_root is not None:
        problems += check_artifacts(artifact_root)
    return problems


def require_environment(*, role: str, database_url: str | None,
                        runtime_config: str | None = None,
                        artifact_root: str | None = None) -> None:
    """Exit with a structured reason instead of looping against a broken environment.

    Written to stderr as one JSON object so a unit's journal carries something a person
    or a script can act on without reading a traceback.
    """
    problems = check_environment(role=role, database_url=database_url,
                                 runtime_config=runtime_config,
                                 artifact_root=artifact_root)
    if not problems:
        return
    report = {"role": role, "status": "environment_not_ready",
              "pid": os.getpid(), "problems": [p.as_dict() for p in problems]}
    print(json.dumps(report, ensure_ascii=False), file=sys.stderr)
    for problem in problems:
        logger.error("%s: %s", problem.code, problem.detail)
    raise SystemExit(ENVIRONMENT_EXIT_CODE)
