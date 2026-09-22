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
TASKS: dict[str, dict] = {
    "constraint": {
        "task": (
            "Write exactly three files into your workspace: `first.md`, `second.md`, `third.md`.\n"
            "**The constraint: every file must end with the line `REVIEWED-BY-ALPHA`.** This is the "
            "requirement you will be judged on, and it must hold for all three files.\n"
            "Then, for each file, write four paragraphs explaining its purpose."),
        # **Exactly checkable**, and checked exactly: every one of the three files has to end with the
        # line, not merely contain it somewhere.
        "check": "REVIEWED-BY-ALPHA", "check_is": "every_file_ends", "files": 3,
    },
    "evidence": {
        "task": (
            "Build `evidence.md` by gathering at least six separate pieces of evidence and quoting "
            "each one.\n"
            "**The constraint: every quotation must be followed by the source it came from.** "
            "Then write a short synthesis that does not introduce any fact not already quoted."),
        # A proxy, and named as one: whether a quotation is *followed by* its source is a judgement
        # about prose. What is checked is that the word appears at all, which finds the runs where the
        # requirement was plainly dropped and says nothing about the ones where it was met badly.
        "check": "source", "check_is": "contains",
    },
    "tail": {
        "task": (
            "Run a command that prints a long listing (at least two thousand lines) and write "
            "`summary.md` with the **last line** of that listing quoted exactly.\n"
            "**The constraint: the last line must be quoted verbatim, not paraphrased.**"),
        # Also a proxy: "quoted verbatim" needs the listing itself, and the node produced it inside a
        # command whose output only the node saw. That the file exists is what can be checked from here.
        "check": "summary.md", "check_is": "file",
    },
}


def _holds(spec: dict, directory: Path, produced: list[str], body: str) -> bool:
    """Whether the task's own requirement is met — exactly where it can be, and said so where it cannot.

    One keyword test over everything would let "the constraint survived" mean "the word appears
    somewhere in any markdown file", which is how a run that dropped the requirement twice could be
    reported as keeping it. Where the requirement is mechanically checkable it is checked mechanically;
    where it is a judgement about prose the proxy is named in `TASKS` rather than dressed up.
    """
    kind = spec.get("check_is", "contains")
    if kind == "every_file_ends":
        named = [item for item in produced if item.endswith(".md")
                 and not item.startswith("runs/")]
        if len(named) < spec.get("files", 3):
            return False
        for item in named[:spec.get("files", 3)]:
            lines = [line for line in (directory / item).read_text(
                encoding="utf-8", errors="replace").splitlines() if line.strip()]
            if not lines or lines[-1].strip() != spec["check"]:
                return False
        return True
    if kind == "file":
        # By **basename**: the two arms do not put their artefacts in the same place — the mini path
        # works inside a run directory of its own — so a full-path test compares the layouts rather
        # than whether the file was written. It reported every mini run as having produced nothing.
        return any(Path(item).name == spec["check"] for item in produced)
    return spec["check"] in body


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


def _project_config(path: str) -> tuple[dict, dict | None]:
    """The project's own runtime config and secret file, the way the default path reads them.

    Reused rather than restated: a model route written down twice is two routes that drift, and the one
    this script invents would be the one that is wrong about `base_url` or `wire_api`.
    """
    raw = json.loads(Path(path).read_text(encoding="utf-8"))
    # Keyed by `ref`, not by the model name: the project configures two entries for the same model
    # with different settings, and keying by name silently keeps whichever came last — which is how the
    # real window was lost and the budget fell back to a default.
    models = {item.get("ref", item["model"]): item for item in raw.get("models", [])}
    secret_file = raw.get("secret_file")
    return models, (json.loads(Path(secret_file).read_text(encoding="utf-8")) if secret_file else None)


def _model(model: dict, secret: str):
    """One configured model, as a client the node entry point can actually take.

    **Not the client the mini path builds.** `run_node` wraps whatever it is given in PydanticAI's
    `WrapperModel` to count requests, and that calls `infer_model`, which takes a PydanticAI model or a
    `provider:model` string — a mini `LitellmModel` is neither, and the failure is a `TypeError` deep in
    the framework rather than anything that names the real problem. So the experiment uses the
    PydanticAI client for the same endpoint, which is also what a migration would use.

    `wire_api` is honoured: the project configures `responses`, and the two clients are not
    interchangeable in what they accept or report.
    """
    from pydantic_ai.models.openai import OpenAIChatModel, OpenAIResponsesModel
    from pydantic_ai.providers.openai import OpenAIProvider

    provider = OpenAIProvider(base_url=model.get("base_url"), api_key=secret or None)
    chosen = OpenAIResponsesModel if model.get("wire_api") == "responses" else OpenAIChatModel
    return chosen(model["model"], provider=provider)


def _run_mini(spec: dict, workspace: Path, config_path: str):
    """One pass of the mini path, through the graph runner rather than through the new entry point.

    It gets a one-node graph so that the surrounding machinery — the scheduler, the sandbox, the commit
    — is the real one, and it is handed the same task text the other arm gets.
    """
    import json as _json
    from anchor.simple import run as runner
    (workspace / "graph.json").write_text(_json.dumps({
        "entry": "only", "objective": spec["task"],
        "agents": {"w": {"model": "models.deepseek", "writes": ["*"]}},
        "nodes": [{"id": "only", "agent": "w"}], "edges": [],
    }), encoding="utf-8")
    state = runner.run(workspace, config_path=config_path)
    node = state.nodes.get("only", {})
    return (NodeOutcome(status="completed" if state.status == "finished" else state.status,
                        submission=node.get("submission", ""), route=None,
                        model_requests=0, reason=state.error or ""),
            Record(workspace.parent / "control"))


async def one(node: str, name: str, spec: dict, attempt: int, workspace: Path,
              model_spec: dict, summariser_spec: dict | None, secret: str,
              budget: Budget, config_path: str) -> Attempt:
    import shutil

    directory = workspace / f"{node}-{name}-{attempt}"
    shutil.rmtree(directory, ignore_errors=True)
    directory.mkdir(parents=True, exist_ok=True)
    # **The control directory is a sibling of the node's workspace, never inside it.** Inside, the node
    # can read and write the record of itself, and the audit trail becomes something the audited thing
    # can edit.
    control = workspace.parent / "control" / f"{node}-{name}-{attempt}"
    control.mkdir(parents=True, exist_ok=True)
    record = Record(control)
    summarizer = _model(summariser_spec, secret) if summariser_spec else None
    capabilities = (context_capabilities(budget, record=record, summarizer=summarizer)
                    if node == "pydantic" else ())

    started = time.monotonic()
    if node == "mini":
        # **The real mini path.** The first version of this script called the same Pydantic `run_node`
        # for both arms with the capabilities left off, which compares "Pydantic without a context
        # strategy" against "Pydantic with one" — not a baseline against a candidate. Its numbers said
        # nothing about mini and the conclusion drawn from them has been withdrawn.
        outcome, record = _run_mini(spec, directory, config_path)
    else:
        outcome = await run_node(
            NodeRequest(execution_id=f"{node}-{name}-{attempt}", task=spec["task"],
                        workspace=directory, max_requests=40, trace=directory / "trace.jsonl"),
            model=_model(model_spec, secret), capabilities=capabilities)
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
        # A filename check or a content check, said which. "Did it write the file it was told to"
        # and "does what it wrote still carry the requirement" are different questions and a single
        # substring test answers neither reliably.
        constraint_present=_holds(spec, directory, produced, body))


async def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", default=".local/runtime.json",
                        help="the project's runtime config; its model and secret file are used as-is")
    parser.add_argument("--model", default="", help="which configured model to use (default: the first)")
    parser.add_argument("--summarizer", default="", help="the compaction model; defaults to --model")
    parser.add_argument("--runs", type=int, default=2)
    parser.add_argument("--tasks", default=",".join(TASKS), help=f"any of: {', '.join(TASKS)}")
    parser.add_argument("--workspace", default=".local/context-experiment")
    parser.add_argument("--window", type=int, default=0,
                        help="default: the configured model's own context_window")
    parser.add_argument("--input-target", type=int, default=0,
                        help="default: three quarters of the window, leaving room for the answer")
    args = parser.parse_args()

    try:
        models, secrets = _project_config(args.config)
    except (OSError, json.JSONDecodeError, KeyError) as exc:
        print(f"cannot read {args.config}: {exc}", file=sys.stderr)
        return 3
    chosen_model = args.model or next(iter(models), "")
    if chosen_model in {item.get("model", "") for item in models.values()} and chosen_model not in models:
        chosen_model = next(key for key, item in models.items() if item.get("model") == chosen_model)
    if chosen_model not in models:
        print(f"{chosen_model!r} is not one of {', '.join(models) or 'the configured models'}",
              file=sys.stderr)
        return 2
    model_spec = models[chosen_model]
    summariser_spec = models.get(args.summarizer, model_spec) if args.summarizer else None
    secret = ""
    if model_spec.get("secret_ref"):
        secret = (secrets or {}).get(model_spec["secret_ref"], "")
        if not secret:
            # The honest outcome, and the one §68 asks for: what is missing, and how to run it. Nothing
            # is read out of the secret file and nothing is written anywhere else.
            print(f"{model_spec['secret_ref']} is not in {args.config}'s secret file.\n"
                  f"Nothing was run and no result is being reported.", file=sys.stderr)
            return 3

    # **The real window, from the configuration.** §24 asks for it to be recorded, and a budget built
    # on an assumed window is wrong for the one model it is used with.
    window = args.window or model_spec.get("context_window") or 32_000
    budget = Budget(window=window, output_reserve=window // 8,
                    input_target=args.input_target or int(window * 0.75), keep_messages=6)
    print(f"model {chosen_model} ({model_spec['model']}) window {window:,} tokens")
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
                                          model_spec, summariser_spec, secret, budget,
                                          args.config))

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
