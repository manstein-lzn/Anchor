#!/usr/bin/env python3
"""The real-model experiment §5 asks for, as a command rather than a paragraph.

It runs the same three small tasks against two nodes — the current mini path and the Pydantic node with
the context capabilities — and writes down what happened to each: whether it finished, how large the
inputs were, how many model calls that took, and whether the constraints stated early were still there
at the end. The last one is the only measurement here that a deterministic model cannot make, which is
the whole reason the experiment exists.

**Nothing in this file invents a result.** With no authorised endpoint it exits saying so, because a
placeholder success is worse than a documented absence — a reader who sees a green run has no way to
learn that no model was involved.

    scripts/context_experiment.py --model deepseek:deepseek-chat --summarizer deepseek:deepseek-chat

Credentials come from the environment as the provider SDKs expect. This script does not read, print,
store or pass them anywhere else.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import sys
import time
from dataclasses import asdict, dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from anchor.node import NodeRequest, NodeOutcome                              # noqa: E402
from anchor.node.context import Budget, Record, context_capabilities          # noqa: E402
from anchor.node.pydantic_adapter import run_node                             # noqa: E402

#: §70's three tasks. Each states a constraint **early** and then asks for work that buries it, because
#: the thing being measured is whether an early constraint survives the compaction that follows.
TASKS: dict[str, dict[str, str]] = {
    "constraint": {
        "task": (
            "Write exactly three files into your workspace: `first.md`, `second.md`, `third.md`.\n"
            "**The constraint: every file must end with the line `REVIEWED-BY-ALPHA`.** This is the "
            "requirement you will be judged on, and it must hold for all three files.\n"
            "Then, for each file, write four paragraphs explaining its purpose."),
        "check": "REVIEWED-BY-ALPHA",
    },
    "evidence": {
        "task": (
            "Build `evidence.md` by gathering at least six separate pieces of evidence and quoting "
            "each one.\n"
            "**The constraint: every quotation must be followed by the source it came from.** "
            "Then write a short synthesis that does not introduce any fact not already quoted."),
        "check": "source",
    },
    "tail": {
        "task": (
            "Run a command that prints a long listing (at least two thousand lines) and write "
            "`summary.md` with the **last line** of that listing quoted exactly.\n"
            "**The constraint: the last line must be quoted verbatim, not paraphrased.**"),
        "check": "summary.md",
    },
}


@dataclass
class Attempt:
    """One run of one task, recorded as the numbers §72 asks for and not as an impression."""

    node: str
    task: str
    attempt: int
    status: str
    reason: str
    model_requests: int
    largest_sent_tokens: int
    largest_arriving_tokens: int
    compactions: int
    summaries: int
    commands: int
    seconds: float
    produced: list[str]
    constraint_present: bool


def _model(name: str):
    """The provider route, by the same client the default path uses.

    Imported here rather than at module level so that `--help` and the no-endpoint path work on a
    machine where litellm's dependencies are not installed.
    """
    from minisweagent.models.litellm_model import LitellmModel
    provider, _, model = name.partition(":")
    if not model:
        raise SystemExit(f"--model wants `provider:model`, got {name!r}")
    base_url = os.environ.get("ANCHOR_MODEL_BASE_URL")
    if base_url:
        return LitellmModel(model_name=f"{provider}/{model}", cost_tracking="ignore_errors",
                            api_base=base_url)
    return LitellmModel(model_name=f"{provider}/{model}", cost_tracking="ignore_errors")


def _endpoint_configured(model: str) -> bool:
    """Whether anything suggests a key for this provider exists — without reading or printing it."""
    provider = model.partition(":")[0].upper().replace("-", "_")
    return any(os.environ.get(f"{provider}_API_KEY") or os.environ.get(f"{provider}__API_KEY")
               or (provider == "DEEPSEEK" and os.environ.get("DEEPSEEK_API_KEY")) for _ in (0,))


async def one(node: str, name: str, spec: dict[str, str], attempt: int, workspace: Path,
              model_name: str, summarizer_name: str, budget: Budget) -> Attempt:
    import shutil

    directory = workspace / f"{node}-{name}-{attempt}"
    shutil.rmtree(directory, ignore_errors=True)
    directory.mkdir(parents=True, exist_ok=True)
    record = Record(directory / "record")
    summarizer = _model(summarizer_name) if summarizer_name else None
    capabilities = (context_capabilities(budget, record=record, summarizer=summarizer)
                    if node == "pydantic" else ())

    started = time.monotonic()
    outcome: NodeOutcome = await run_node(
        NodeRequest(execution_id=f"{node}-{name}-{attempt}", task=spec["task"], workspace=directory,
                    max_requests=40, trace=directory / "trace.jsonl"),
        model=_model(model_name), capabilities=capabilities)
    seconds = time.monotonic() - started

    produced = sorted(str(item.relative_to(directory)) for item in directory.rglob("*")
                      if item.is_file() and ".git" not in item.relative_to(directory).parts
                      and item.name not in ("trace.jsonl",))
    body = "\n".join((directory / item).read_text(encoding="utf-8", errors="replace")
                     for item in produced if (directory / item).suffix == ".md")
    return Attempt(
        node=node, task=name, attempt=attempt, status=outcome.status, reason=outcome.reason[:200],
        model_requests=outcome.model_requests,
        largest_sent_tokens=max((item["tokens"] for item in record.sent), default=0),
        largest_arriving_tokens=max((item["tokens"] for item in record.arriving), default=0),
        compactions=len(record.compactions), summaries=len(record.summaries),
        commands=len(record.commands), seconds=round(seconds, 1), produced=produced,
        # A keyword check, and named as one: whether the *specific* requirement is honoured needs a
        # person. This only finds the cases where it is plainly absent, which are the informative ones.
        constraint_present=spec["check"] in body)


async def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", help="the node's model, as `provider:model`")
    parser.add_argument("--summarizer", default="", help="the compaction model; defaults to --model")
    parser.add_argument("--runs", type=int, default=2)
    parser.add_argument("--tasks", default=",".join(TASKS), help=f"any of: {', '.join(TASKS)}")
    parser.add_argument("--workspace", default=".local/context-experiment")
    parser.add_argument("--window", type=int, default=32_000)
    parser.add_argument("--input-target", type=int, default=24_000)
    args = parser.parse_args()

    if not args.model:
        print(__doc__)
        return 2
    if not _endpoint_configured(args.model):
        # The honest outcome, and the one §68 asks for: what is missing, and how to run it.
        print(f"no credentials found for {args.model.partition(':')[0]!r}.\n"
              f"Set the provider's usual API key environment variable and re-run:\n"
              f"    {Path(__file__).name} --model {args.model} --summarizer "
              f"{args.summarizer or args.model}\n"
              f"Nothing was run and no result is being reported.", file=sys.stderr)
        return 3

    budget = Budget(window=args.window, output_reserve=args.window // 8,
                    input_target=args.input_target, keep_messages=6)
    workspace = Path(args.workspace)
    chosen = [name for name in args.tasks.split(",") if name in TASKS]
    if not chosen:
        raise SystemExit(f"no known task in {args.tasks!r}; known: {', '.join(TASKS)}")

    attempts: list[Attempt] = []
    for name in chosen:
        for attempt in range(1, args.runs + 1):
            for node in ("mini", "pydantic"):
                # The mini node does not take the context capabilities; that is the comparison.
                print(f"  {node:9s} {name:11s} run {attempt} …", flush=True)
                attempts.append(await one(node, name, TASKS[name], attempt, workspace,
                                          args.model, args.summarizer, budget))

    out = workspace / "results.json"
    out.write_text(json.dumps([asdict(item) for item in attempts], indent=2, ensure_ascii=False),
                   encoding="utf-8")
    print(f"\nwrote {out}")

    print(f"\n{'node':10s} {'task':12s} {'run':3s} {'status':16s} {'calls':>5s} "
          f"{'sent':>7s} {'arriv':>7s} {'comp':>4s} {'sec':>6s}  constraint")
    for item in attempts:
        print(f"{item.node:10s} {item.task:12s} {item.attempt:3d} {item.status:16s} "
              f"{item.model_requests:5d} {item.largest_sent_tokens:7d} "
              f"{item.largest_arriving_tokens:7d} {item.compactions:4d} {item.seconds:6.1f}  "
              f"{'kept' if item.constraint_present else 'ABSENT'}")
    print("\n**These are migration signals, not stability.** Failures are kept in the file rather "
          "than filtered out, and a keyword check is not a judgement about the work.")
    return 0


if __name__ == "__main__":
    raise SystemExit(asyncio.run(main()))
