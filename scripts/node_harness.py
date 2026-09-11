#!/usr/bin/env python
"""Re-run one node of a finished run against the input it actually received.

A campaign costs half an hour and is not deterministic, so a context policy cannot
be judged from one: change the policy, run a campaign, and the outcome moves for
reasons nobody can separate. One node attempt against its own frozen input can be
compared, and it takes a minute.

    # what the node really saw, for free, no model call
    .venv/bin/python scripts/node_harness.py prompt RUN_UUID write

    # run it once, live, and report the spend
    .venv/bin/python scripts/node_harness.py run RUN_UUID write

    # vary one part of the request and compare
    .venv/bin/python scripts/node_harness.py run RUN_UUID write --drop-memory

Nothing here writes to the run it reads. A tool-using node is refused rather than
executed outside a ledger; run a whole run for those.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from anchor.runtime.artifacts import LocalArtifactStore  # noqa: E402
from anchor.runtime.capabilities import CapabilityRegistry  # noqa: E402
from anchor.runtime.config import load_runtime_config  # noqa: E402
from anchor.runtime.model_gateway import build_model_gateway  # noqa: E402
from anchor.runtime.node_harness import (  # noqa: E402
    FrozenAttempt,
    HarnessUnsupported,
    NodeHarness,
    compare,
)
from anchor.runtime.secrets import (  # noqa: E402
    ChainedSecretProvider,
    EnvironmentSecretProvider,
    JsonFileSecretProvider,
)
from anchor.runtime.settings import AnchorSettings  # noqa: E402
from anchor.state.relational import RelationalStateStore  # noqa: E402


def _emit(value: Any) -> None:
    print(json.dumps(value, ensure_ascii=False, indent=2, sort_keys=False))


def _harness(settings: AnchorSettings, model_ref: str, *,
             with_gateway: bool = True) -> tuple[NodeHarness, Any, Any]:
    """Build the harness. ``prompt`` needs no gateway, so it builds none.

    That is not only an optimisation: an httpx client constructed for a command
    that never calls a model still has to be closed, and closing it requires the
    loop it was built in. Not building it removes the problem.
    """
    store = RelationalStateStore(settings.require_database_url())
    config = load_runtime_config(settings.runtime_config)
    providers: list[Any] = [EnvironmentSecretProvider()]
    if config.secret_file:
        providers.append(JsonFileSecretProvider(config.secret_file))
    registry = CapabilityRegistry(models=config.models, agents=config.agents,
                                  tools=config.tools, verifiers=config.verifiers)
    gateway = None
    if with_gateway:
        profile = next((item for item in config.models if item.ref == model_ref), None)
        if profile is None:
            raise SystemExit(f"no model profile {model_ref!r} in {settings.runtime_config}")
        gateway = build_model_gateway(profile, ChainedSecretProvider(*providers))
    harness = NodeHarness(store, LocalArtifactStore(settings.artifact_root),
                          gateway=gateway, registry=registry)
    return harness, gateway, store


async def _execute(harness: NodeHarness, gateway: Any, attempt: FrozenAttempt,
                   snapshot: dict[str, Any] | None, include_memory: bool,
                   *, second: bool = False, drop_memory: bool = False) -> Any:
    """One call, or two, inside a single event loop, closing the client in it.

    Everything async happens here on purpose. The gateway's HTTP client binds to
    the loop it is first used in, so a second ``asyncio.run`` — even for closing —
    fails with an error that looks like a provider fault and is not one.
    """
    try:
        if not second:
            return await harness.run(attempt, snapshot=snapshot,
                                     include_memory=include_memory)
        before = await harness.run(attempt, snapshot=snapshot, include_memory=True)
        after = await harness.run(attempt, snapshot=snapshot,
                                  include_memory=not drop_memory)
        return before, after
    finally:
        if gateway is not None:
            await gateway.close()


def main(argv: list[str] | None = None) -> int:
    settings = AnchorSettings()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("prompt", "run", "compare", "attempts"))
    parser.add_argument("run_id")
    parser.add_argument("node_id", nargs="?")
    parser.add_argument("--attempt", type=int, default=None,
                        help="which attempt (default: the latest)")
    parser.add_argument("--model-ref", default="models.academic")
    parser.add_argument("--drop-memory", action="store_true",
                        help="assemble the prompt without any memory block")
    parser.add_argument("--snapshot", help="replace the snapshot with this JSON file")
    args = parser.parse_args(argv)

    needs_model = args.command in ("run", "compare")
    harness, gateway, store = _harness(settings, args.model_ref,
                                       with_gateway=needs_model)
    try:
        if args.command == "attempts":
            _emit({args.node_id or "*": [
                {"attempt": item.attempt, "status": item.status.value,
                 "node_run_id": str(item.id)}
                for item in store.list_node_runs(args.run_id)
                if args.node_id in (None, item.node_id)]})
            return 0
        if not args.node_id:
            raise SystemExit("a node id is required for prompt and run")
        attempt = harness.frozen_attempt(args.run_id, args.node_id, args.attempt)
        snapshot = None
        if args.snapshot:
            snapshot = json.loads(Path(args.snapshot).read_text(encoding="utf-8"))
        if args.command == "prompt":
            prompt = harness.prompt_for(attempt, snapshot=snapshot,
                                        include_memory=not args.drop_memory)
            _emit({"node_id": attempt.node_id, "attempt": attempt.attempt,
                   "agent_ref": attempt.agent_ref, "tools": list(attempt.tools),
                   "instructions_chars": len(attempt.instructions),
                   "prompt_chars": len(prompt), "prompt": prompt})
            return 0
        if args.command == "compare":
            # The point of the harness: one attempt, two policies, two live calls,
            # and a report that says whether the answer moved without judging it.
            before, after = asyncio.run(
                _execute(harness, gateway, attempt, snapshot, True, second=True,
                         drop_memory=args.drop_memory))
            _emit({"before": before.summary(), "after": after.summary(),
                   "comparison": compare(before, after),
                   "after_text": after.text})
            return 0
        result = asyncio.run(_execute(harness, gateway, attempt, snapshot,
                                      not args.drop_memory))
        _emit({"result": result.summary(), "text": result.text})
        return 0
    except HarnessUnsupported as exc:
        _emit({"error": {"code": "unsupported", "detail": str(exc)}})
        return 2
    except KeyError as exc:
        _emit({"error": {"code": "not_found", "detail": str(exc)}})
        return 2
    except Exception as exc:  # noqa: BLE001 - a provider fault is a report, not a crash
        # A live call can fail at the provider. Say so with the type and message
        # rather than a traceback: this is an experiment tool, and a failed
        # experiment should read as one.
        _emit({"error": {"code": "model_call_failed",
                          "detail": f"{type(exc).__name__}: {exc}"}})
        return 1
    finally:
        store.close()


if __name__ == "__main__":
    raise SystemExit(main())
