"""The tools a node may call. Every one of them returns text.

Two families, and deliberately the same shape:

  workspace.*  read, write, list and exec inside the node's own tree
  scholarly.*  the literature tools, whose rate limiting lives in research_tools and is untouched

Nothing here records an operation, classifies a side effect, or consults a ledger. A workspace call
touches the node's tree and nothing else; that tree belongs to this node, and it is thrown away if
the run fails.
"""

from __future__ import annotations

import asyncio
import json
import shlex
from dataclasses import dataclass
from collections.abc import Sequence
from pathlib import Path

from anchor.runtime.model_gateway import ToolFunction
from anchor.runtime.research_tools import ResearchRequest, execute_research
from anchor.runtime.sandbox import BubblewrapWorkspaceSandbox, SandboxDenied, SandboxSpec

COMPLETE = "goal_complete"


@dataclass
class Completion:
    """Whether the node said it was done, and why.

    This is the only way a node ends. A model that replies in words has not finished anything — it
    has narrated — and the runner asks again. Marking the goal complete is an action, so it cannot
    be produced accidentally, and it cannot be produced without the model having decided that the
    work is done.
    """

    done: bool = False
    summary: str = ""


SCHOLARLY = ("scholarly.search", "scholarly.read", "scholarly.read_many", "scholarly.citations")
WORKSPACE = ("workspace.read", "workspace.write", "workspace.list", "workspace.exec")

DESCRIPTIONS = {
    COMPLETE: ("Call this only when the goal is fully met. It ends the run of this node, so call it "
               "once, at the end, with a one-sentence summary. Until it is called the node keeps "
               "being asked to continue. Arguments: {summary}"),
    "workspace.read": "Read a file from your workspace. Arguments: {path}",
    "workspace.write": ("Write a file into your workspace. Revise freely — the tree is yours and is "
                        "delivered as it stands when you finish. Arguments: {path, content}"),
    "workspace.list": "List the files in your workspace. Arguments: {prefix?}",
    "workspace.exec": ("Run an allowlisted command in your workspace (cat cut file find git grep "
                       "head ls python3 sort tail uniq wc). No network, and only your workspace is "
                       "writable. Arguments: {command}"),
    "scholarly.search": ('Search the literature. Arguments: {"query": str, "source": "crossref"|'
                         '"arxiv", "limit": int}'),
    "scholarly.read": 'Read one document. Arguments: {"url": str}',
    "scholarly.read_many": 'Read several documents at once. Arguments: {"urls": [str]}',
    "scholarly.citations": ('Follow the citation graph from a paper. Arguments: {"identifier": str, '
                            '"direction": "cites"|"cited_by"}'),
}


def _fail(message: str) -> str:
    """A tool failure is a message to the model. It is the only way a call reports failure, because
    the model can act on a message and cannot act on an exception."""
    return f"TOOL FAILED: {message}"


def _resolve(tree: Path, path: object) -> Path:
    if not isinstance(path, str) or not path:
        raise ValueError("a non-empty 'path' is required")
    relative = Path(path)
    if relative.is_absolute() or ".." in relative.parts:
        raise ValueError(f"path must stay inside the workspace: {path}")
    return tree / relative


def workspace_call(name: str, arguments: dict, *, tree: Path, sandbox: object) -> str:
    try:
        if name == "workspace.read":
            target = _resolve(tree, arguments.get("path"))
            if not target.is_file():
                return _fail(f"no such file: {arguments.get('path')}")
            return target.read_text(encoding="utf-8")
        if name == "workspace.write":
            content = arguments.get("content")
            if not isinstance(content, str):
                return _fail("'content' must be a string")
            target = _resolve(tree, arguments.get("path"))
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(content, encoding="utf-8")
            return json.dumps({"path": str(target.relative_to(tree)), "bytes": len(content)})
        if name == "workspace.list":
            prefix = arguments.get("prefix") if isinstance(arguments.get("prefix"), str) else ""
            found = sorted(str(item.relative_to(tree)) for item in tree.rglob("*")
                           if item.is_file() and ".git" not in item.relative_to(tree).parts
                           and str(item.relative_to(tree)).startswith(prefix))
            return json.dumps({"paths": found}, ensure_ascii=False)
        command = arguments.get("command")
        if isinstance(command, str):
            # A model asked for a command gives a command, not an argv array. Splitting it here is
            # not a shell — there is no shell, and nothing is expanded — but it is what the caller
            # meant, and refusing it only teaches the model a spelling.
            command = shlex.split(command)
        if not isinstance(command, Sequence) or not command:
            return _fail("'command' must be a non-empty list, for example [\"wc\", \"-c\", \"f.md\"]")
        result = sandbox.run(SandboxSpec(workspace=tree,
                                         command=tuple(str(item) for item in command)))
        return json.dumps({"returncode": result.returncode, "stdout": result.stdout,
                           "stderr": result.stderr, "timed_out": result.timed_out},
                          ensure_ascii=False)
    except (OSError, ValueError, SandboxDenied) as exc:
        return _fail(str(exc))


async def scholarly_call(name: str, arguments: dict, *, timeout_seconds: float) -> str:
    try:
        request = ResearchRequest(**arguments)
    except Exception as exc:  # noqa: BLE001 - the model's arguments are untrusted input
        return _fail(f"arguments rejected: {exc}")
    try:
        return await asyncio.to_thread(execute_research, name, request,
                                       timeout_seconds=timeout_seconds)
    except Exception as exc:  # noqa: BLE001 - a source being down is a message, not a crash
        return _fail(f"{type(exc).__name__}: {exc}")


async def _dispatch(name: str, arguments: dict, *, tree: Path, sandbox: object,
                    timeout_seconds: float, completion: "Completion") -> str:
    if name == COMPLETE:
        summary = arguments.get("summary")
        completion.done = True
        completion.summary = summary if isinstance(summary, str) else ""
        return "The goal is marked complete. This node is finished."
    if name in WORKSPACE:
        return await asyncio.to_thread(workspace_call, name, arguments, tree=tree, sandbox=sandbox)
    return await scholarly_call(name, arguments, timeout_seconds=timeout_seconds)


def _bind(name: str, *, tree: Path, sandbox: object, timeout_seconds: float,
          completion: "Completion") -> ToolFunction:
    """One tool, bound to this node's tree.

    The callable takes exactly one argument and no others. That is not a style preference: the tool's
    schema is derived from this signature, so a second parameter — even one with a default — becomes
    a parameter the model is asked to supply, and a tool whose interface it cannot make sense of is
    one it talks about instead of calling.

    Every failure leaves as a string. A tool must never take the node down, whatever goes wrong
    inside it: the model can act on a message and cannot act on an exception.
    """

    async def call(arguments_json: str) -> str:
        try:
            arguments = json.loads(arguments_json or "{}")
        except json.JSONDecodeError:
            return _fail("arguments must be a JSON object")
        if not isinstance(arguments, dict):
            return _fail("arguments must be a JSON object")
        try:
            return await _dispatch(name, arguments, tree=tree, sandbox=sandbox,
                                   timeout_seconds=timeout_seconds, completion=completion)
        except Exception as exc:  # noqa: BLE001 - see above
            return _fail(f"{type(exc).__name__}: {exc}")

    return ToolFunction(name=name, description=DESCRIPTIONS[name], call=call)


def catalog(names: Sequence[str], *, tree: Path, sandbox: object, timeout_seconds: float,
            completion: Completion | None = None) -> list[ToolFunction]:
    """The tools a node is given, each wired to its own tree.

    `goal_complete` is always present and is not the caller's to remove: without it a node has no way
    to finish that is distinct from talking.
    """
    completion = completion if completion is not None else Completion()
    wanted = [name for name in names if name != COMPLETE]
    for name in wanted:
        if name not in DESCRIPTIONS:
            raise ValueError(f"unknown tool: {name}")
    return [_bind(name, tree=tree, sandbox=sandbox, timeout_seconds=timeout_seconds,
                  completion=completion)
            for name in [COMPLETE, *wanted]]


def new_sandbox() -> object:
    try:
        return BubblewrapWorkspaceSandbox()
    except RuntimeError:
        raise RuntimeError(
            "bubblewrap is required: workspace.exec runs commands, so it needs a sandbox to run "
            "them in") from None
