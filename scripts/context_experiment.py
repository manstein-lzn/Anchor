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
        "check": "REVIEWED-BY-ALPHA", "check_is": "every_file_ends",
        "files": ["first.md", "second.md", "third.md"],
        "human_review": ["每个文件的内容是否真的解释了它的用途"],
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
        # **Structural, and checked line by line**: every quoted block is followed by a line carrying
        # a source (a URL or a DOI). Whether the source is the *right* one is a judgement about the
        # work; whether there is one at all is not, and it is the part an agent dropping the
        # requirement fails.
        "check": "source", "check_is": "quotes_cited", "evidence": 6,
        "human_review": ["引用的来源是否**真的**支持该引用", "综合段是否引入了未被引用的事实"],
    },
    "tail": {
        "task": (
            "Run a command that prints a long listing (at least two thousand lines) and write "
            "`summary.md` with the **last line** of that listing quoted exactly.\n"
            "**The constraint: the last line must be quoted verbatim, not paraphrased.**"),
        # Also a proxy: "quoted verbatim" needs the listing itself, and the node produced it inside a
        # command whose output only the node saw. That the file exists is what can be checked from here.
        # **Exact**: the last line of the listing the node itself produced has to appear verbatim in
        # `summary.md`. The listing is a file on disk, so this needs no judgement at all.
        "check": "summary.md", "check_is": "last_line_verbatim",
        "human_review": ["清单本身是否至少两千行"],
    },
}


def artifacts(attempt: Path, node: str) -> Path:
    """Where an executor actually puts what the node wrote.

    **The two do not agree, and that is the whole of the acceptance's first finding.** The mini path
    works inside a run directory of its own — `runs/<run>/<node>/` — while the new entry point is given
    a workspace directly. A judge that filters out everything under `runs/` therefore sees nothing from
    the baseline at all, and reports it as having failed every requirement. Found by the acceptance
    after this experiment had already been reported once with the wrong conclusion on it.
    """
    if node != "mini":
        return attempt
    runs = sorted((attempt / "runs").glob("*")) if (attempt / "runs").is_dir() else []
    if not runs:
        return attempt
    latest = runs[-1]
    inside = [item for item in latest.iterdir() if item.is_dir()]
    return inside[0] if inside else latest


def _holds(spec: dict, root: Path, produced: list[str], body: str) -> bool:
    """Whether the task's own requirement is met — exactly where it can be, and said so where it cannot.

    One rule per function, because each is a claim about a task that has to be checked against its own
    examples. This dispatcher used to be one long function, and a rule inside it was wrong three times
    in a row without anything noticing.
    """
    kind = spec.get("check_is", "contains")
    if kind == "contains":
        return spec["check"] in body
    if kind == "every_file_ends":
        return _every_file_ends(spec, root, produced)
    if kind == "last_line_verbatim":
        return _last_line_verbatim(spec, root, produced)
    if kind == "quotes_cited":
        return _quotes_cited(spec, root, produced, body)
    if kind == "file":
        return _file_written(spec, root, produced)
    raise ValueError(f"unknown check in {spec!r}")


def _file_written(spec: dict, root: Path, produced: list[str]) -> bool:
    """By **basename**: the two arms do not put their artefacts in the same place, so a full-path test
    compares the layouts rather than whether the file was written."""
    del root
    return any(Path(item).name == spec["check"] for item in produced)


def _every_file_ends(spec: dict, root: Path, produced: list[str]) -> bool:
    """**The exact set of files the task named**, each ending with the line it named.

    The first version took the first three sorted `.md` files, so a run that wrote four files, or wrote
    `draft.md` instead of `third.md`, passed as long as three of them ended correctly. The task names
    three files; the check names the same three.
    """
    del produced
    wanted = list(spec["files"])
    # **Exactly these.** An extra top-level `.md` means the node wrote something other than what it was
    # asked for, and a check that only looked for the three would call that a pass.
    present = sorted(item.name for item in root.glob("*.md")) if root.is_dir() else []
    if present != sorted(wanted):
        return False
    for name in wanted:
        path = root / name
        if not path.is_file():
            return False
        lines = [line for line in path.read_text(encoding="utf-8", errors="replace").splitlines()
                 if line.strip()]
        if not lines or lines[-1].strip() != spec["check"]:
            return False
    return True


def _last_line_verbatim(spec: dict, root: Path, produced: list[str]) -> bool:
    """The listing's last line appears in the summary **character for character**.

    `strip()` on the way in made this something weaker than the task asks for: a line with its leading
    or trailing spaces removed is not the same line, and a quotation that has been tidied is not a
    verbatim one. Only the line ending is dropped, because that is not part of the line.
    """
    del spec, produced
    listings = [item for item in sorted(root.rglob("*.txt")) if "listing" in item.name]
    summaries = list(root.rglob("summary.md"))
    if not listings or not summaries:
        return False
    raw = listings[0].read_text(encoding="utf-8", errors="replace").split("\n")
    lines = [line for line in raw if line != ""]
    if not lines:
        return False
    return lines[-1] in summaries[0].read_text(encoding="utf-8", errors="replace")


def _quote_blocks(lines: list[str]) -> list[int]:
    """Where each quotation starts, in either of the two shapes Markdown has for one.

    Two rounds of this being wrong says how easy it is: every `>` line counted separately reported a
    correct file as failing 22 times, on the continuation lines of eleven multi-line quotations; and
    counting only `>` reported a file that quoted inside fenced blocks as having quoted nothing. The
    task asks for quotations followed by their source, not for a markdown construct.
    """
    starts: list[int] = []
    fenced = False
    for index, line in enumerate(lines):
        stripped = line.strip()
        if stripped.startswith("```"):
            fenced = not fenced
            if fenced:
                starts.append(index)
            continue
        if not fenced and stripped.startswith(">") and (
                index == 0 or not lines[index - 1].strip().startswith(">")):
            starts.append(index)
    return starts


def _quotes_cited(spec: dict, root: Path, produced: list[str], body: str) -> bool:
    """At least the number of pieces of evidence the task asked for, each followed by its source.

    **Accepting both quotation shapes is not the task passing.** The task also asks for at least six
    separate pieces, so a file with one well-sourced quotation is not a file that did the task — and
    the first version of this rule would have said it was.
    """
    del root, produced
    lines = body.splitlines()
    starts = _quote_blocks(lines)
    if len(starts) < spec.get("evidence", 1):
        return False
    for start in starts:
        end = start
        if lines[start].strip().startswith("```"):
            while end + 1 < len(lines) and not lines[end + 1].strip().startswith("```"):
                end += 1
        else:
            while end + 1 < len(lines) and lines[end + 1].strip().startswith(">"):
                end += 1
        following = " ".join(lines[end + 1:end + 6]).lower()
        if not any(key in following for key in ("source", "http", "doi")):
            return False
    return True


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


def _run_mini(spec: dict, workspace: Path, config_path: str, model_ref: str):
    """One pass of the mini path, through the graph runner rather than through the new entry point.

    It gets a one-node graph so that the surrounding machinery — the scheduler, the sandbox, the commit
    — is the real one, and it is handed the same task text the other arm gets.
    """
    import json as _json
    from anchor.simple.run import run as run_graph
    (workspace / "graph.json").write_text(_json.dumps({
        "entry": "only", "objective": spec["task"],
        "agents": {"w": {"model": model_ref, "writes": ["*"]}},
        "nodes": [{"id": "only", "agent": "w"}], "edges": [],
    }), encoding="utf-8")
    state = run_graph(workspace, config_path=config_path)
    node = state.nodes.get("only", {})
    # **From its own trace.** The mini path's calls happen inside its client, where there is no seam to
    # count them — but it writes a trace, one entry per model response, so the number is readable
    # rather than reported as zero.
    traces = sorted(workspace.rglob("*.trace.jsonl")) if workspace.is_dir() else []
    requests = 0
    if traces:
        requests = sum(1 for line in traces[-1].read_text(encoding="utf-8").splitlines()
                       if json.loads(line).get("role") == "assistant")
    return (NodeOutcome(status="completed" if state.status == "finished" else state.status,
                        submission=node.get("submission", ""), route=None,
                        model_requests=requests, reason=state.error or ""),
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
        outcome, record = _run_mini(spec, directory, config_path, model_spec.get("ref", ""))
    else:
        outcome = await run_node(
            NodeRequest(execution_id=f"{node}-{name}-{attempt}", task=spec["task"],
                        workspace=directory, max_requests=40, trace=directory / "trace.jsonl"),
            model=_model(model_spec, secret), capabilities=capabilities)
    seconds = time.monotonic() - started

    root = artifacts(directory, node)
    produced, body = measure(root)
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
        constraint_present=_holds(spec, root, produced, body))


def measure(root: Path) -> tuple[list[str], str]:
    """What a finished attempt left behind, read from the executor's own artefact root."""
    produced = sorted(str(item.relative_to(root)) for item in root.rglob("*")
                      if item.is_file() and ".git" not in item.relative_to(root).parts
                      and item.name not in ("trace.jsonl",))
    body = "\n".join(item.read_text(encoding="utf-8", errors="replace")
                     for item in sorted(root.rglob("*.md")) if item.is_file())
    return produced, body


def rejudge(results: Path, workspace: Path) -> int:
    """Re-apply the judge to a run that has already happened, and call nothing."""
    rows = json.loads(results.read_text(encoding="utf-8"))
    print(f"re-judging {len(rows)} attempts from {results} — no model is called\n")
    for row in rows:
        directory = workspace / f"{row['node']}-{row['task']}-{row['attempt']}"
        root = artifacts(directory, row["node"])
        produced, body = measure(root)
        row["constraint_present"] = _holds(TASKS[row["task"]], root, produced, body)
        row["decided_from"] = str(root)
    print(f"{'node':10s} {'task':12s} {'run':3s} {'status':16s} {'constraint':10s} decided from")
    for row in rows:
        print(f"{row['node']:10s} {row['task']:12s} {row['attempt']:3d} {row['status']:16s} "
              f"{'kept' if row['constraint_present'] else 'ABSENT':10s} {row['decided_from']}")
    out = results.with_name(results.stem + "-rejudged.json")
    out.write_text(json.dumps(rows, indent=2, ensure_ascii=False), encoding="utf-8")
    print(f"\nwrote {out}")

    # **What the judge cannot decide, said out loud.** The mechanical rules answer "was a quotation
    # followed by a source"; they cannot answer "is the source the right one" or "does the synthesis
    # add a fact nobody quoted". Reporting only the first as if it were the whole task is how a
    # passing row reads as a passing piece of work.
    needing_a_person = {row["task"] for row in rows}
    print("\n机械判定覆盖不到的（需人工核验）:")
    for name in sorted(needing_a_person):
        for item in TASKS[name].get("human_review", []):
            print(f"  {name:12s} {item}")
    return 0


async def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", default=".local/runtime.json",
                        help="the project's runtime config; its model and secret file are used as-is")
    parser.add_argument("--model", default="", help="which configured model to use (default: the first)")
    parser.add_argument("--summarizer", default="", help="the compaction model; defaults to --model")
    parser.add_argument("--runs", type=int, default=2)
    parser.add_argument("--tasks", default=",".join(TASKS), help=f"any of: {', '.join(TASKS)}")
    parser.add_argument("--workspace", default=".local/context-experiment")
    parser.add_argument("--rejudge", default="",
                        help="re-apply the judge to a finished run's artefacts; no model is called")
    parser.add_argument("--window", type=int, default=0,
                        help="default: the configured model's own context_window")
    parser.add_argument("--input-target", type=int, default=0,
                        help="default: three quarters of the window, leaving room for the answer")
    args = parser.parse_args()

    if args.rejudge:
        # **No credentials, no model, no cost.** The judge reads what is already on disk, because the
        # acceptance asked for the existing results to be re-judged rather than for more paid samples
        # to be bought against a judge that was wrong.
        return rejudge(Path(args.rejudge), Path(args.workspace))

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
