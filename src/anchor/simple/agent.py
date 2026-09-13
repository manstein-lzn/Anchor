"""mini-swe-agent is the body of a node; where its commands run is ours.

The loop is theirs on purpose. What it does that a loop written here did not:

  - a response without a command is a format error, and the loop retries it. A node cannot stop by
    talking, which is the whole point: in a CLI someone follows up, and in a node nobody does.
  - the run ends only when a command's output asks for submission. Ending is an action, not a
    sentence, so it cannot happen by accident.

What is ours is the boundary: every command runs inside bwrap with the node's own directory
writable, everything else read-only, and the network only for the nodes whose work needs it.

`SandboxEnvironment` subclasses their `LocalEnvironment` rather than reimplementing the interface,
so the submission sentinel stays theirs and cannot drift from the loop that reads it.
"""

from __future__ import annotations

import os
import shutil
import sys
from pathlib import Path

from minisweagent.environments.local import LocalEnvironment

from anchor.runtime.sandbox import BubblewrapWorkspaceSandbox, SandboxSpec

RULES = """\
A node works in one directory and hands back what it leaves there.

Your response must contain exactly ONE bash code block with ONE command (or commands joined with &&
or ||). Put a short THOUGHT section before it saying what you are doing and why.

<format_example>
I need to see what is already here before deciding anything.

```mswea_bash_command
ls -la
```
</format_example>

A response without a command block is rejected and you will be asked again. Never end a turn by
describing what you intend to do — do it.

The literature tools are on your PATH:

  anchor-scholarly search     --query "..." [--source crossref|arxiv] [--limit N] [--offset N]
  anchor-scholarly read       --url "https://..."
  anchor-scholarly read-many  --urls "u1,u2"
  anchor-scholarly citations  --identifier "..." [--direction cited_by|cites]

They print JSON on success and exit non-zero with a message on stderr on failure. Sources are rate
limited and sometimes refuse: that is information, not a dead end. Try another source, narrow the
query, or move on — and never claim a source was read when it was not.
"""

INSTANCE_TEMPLATE = """\
{{task}}

You work in a directory that already contains everything the previous step produced. When you are
finished, run this exact command and nothing after it:

echo COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT

Then, on the following lines, write your summary. It is the only thing that leaves this step, so it
must say what you did and what you could not do.
"""


def system_prompt(instructions: str) -> str:
    """The role, then the rules that hold for every node.

    Split because they change for different reasons: the role comes from the graph and is the
    author's business, and the rules are the contract of running inside a directory with a shell —
    which is ours, and must not be something each node may quietly reword.
    """
    return f"{instructions.strip()}\n\n{RULES.strip()}\n" if instructions.strip() else RULES


def _console_script(name: str) -> str | None:
    """Where a console script of *this* installation is.

    Next to the interpreter, not looked up on PATH. The runner is started as `python -m …`, which
    does not put its own environment's `bin` on PATH, so a lookup there finds nothing — and then the
    node is told about a tool that is not on its PATH and cannot be called.
    """
    # Not resolved. `sys.executable` is `<venv>/bin/python`, which is a symlink to whatever
    # interpreter the environment was built from — and following it walks out of the environment,
    # where the console scripts are not.
    candidate = Path(sys.executable).parent / name
    if candidate.is_file():
        return str(candidate)
    return shutil.which(name)


def _tool_binds(console_script: str | None) -> tuple[tuple[str, str], ...]:
    """What has to be visible for the literature tool to run: its interpreter and its package.

    Both at their real paths. A virtual environment is not relocatable — the interpreter looks for
    its libraries relative to itself, and the console script's shebang names the interpreter — so a
    bind at some tidier location would produce a tool that cannot start.
    """
    if not console_script:
        return ()
    venv = Path(console_script).parents[1]
    package = Path(__file__).resolve().parents[2]
    binds = [(str(venv), str(venv))]
    if package.is_dir():
        binds.append((str(package), str(package)))
    # And the Python the environment is built on, which is not inside it: `<venv>/bin/python` is a
    # symlink into an installed interpreter, and a console script's shebang names the symlink. There
    # are two links in that chain — the environment points at a stable alias like `cpython-3.12`,
    # which points at the versioned directory actually on disk — so both have to be present or the
    # script cannot start. Binding only the resolved one produced
    # `bad interpreter: No such file or directory`, which is what a missing alias looks like.
    for candidate in (sys.executable, os.readlink(sys.executable) if os.path.islink(sys.executable) else None):
        if not candidate:
            continue
        prefix = Path(candidate).parent
        prefix = prefix.parent if prefix.name == "bin" else prefix
        if prefix.is_dir() and str(prefix) != str(venv):
            binds.append((str(prefix), str(prefix)))
    return tuple(dict.fromkeys(binds))


class SandboxEnvironment(LocalEnvironment):
    """Their local environment, with bwrap between it and the machine.

    A real subclass, not a copy: `_check_finished` is inherited, and that method is where the
    submission sentinel is recognised. Reimplementing it would work until the day it did not.
    """

    def __init__(self, *, tree: Path, network: bool, timeout_seconds: float, **kwargs) -> None:
        super().__init__(cwd=str(tree), **kwargs)
        self.tree = Path(tree)
        self.network = network
        self.timeout_seconds = timeout_seconds
        # The command arrives as one shell string, so the shell is the entry point and the sandbox is
        # the boundary. An allowlist of commands would be a second, weaker boundary that the shell
        # can step around anyway.
        self.sandbox = BubblewrapWorkspaceSandbox(allowed_commands=frozenset({"sh"}))
        # Found on *this* process's PATH and handed to the sandbox, so the node can call the tool by
        # name. The sandbox does not inherit an environment, which is deliberate — a node should get
        # what it was given and not what happened to be lying around.
        found = _console_script("anchor-scholarly")
        self.tool_dirs = (str(Path(found).parent),) if found else ()
        # The literature tool is a console script in a virtual environment, so it needs that
        # environment and the package it imports to exist inside the sandbox — at their real paths,
        # because the interpreter and its scripts carry absolute ones. This is our own code, which is
        # the point: it is read-only and it is the only thing of ours the node can see. The
        # repository root is deliberately not bound, because the secrets file and the old state
        # live under it.
        self.readonly_binds = _tool_binds(found)

    def execute(self, action, cwd: str = "", *, timeout: int | None = None) -> dict:
        command = action.get("command", "")
        result = self.sandbox.run(SandboxSpec(
            workspace=self.tree, command=("sh", "-c", command),
            timeout_seconds=float(timeout or self.timeout_seconds), network=self.network,
            tool_dirs=self.tool_dirs, readonly_binds=self.readonly_binds))
        output = {"output": result.stdout + result.stderr, "returncode": result.returncode,
                  "exception_info": "the command timed out" if result.timed_out else ""}
        self._check_finished(output)
        return output


def build_agent(*, tree: Path, instructions: str, model_name: str, model_kwargs: dict,
                network: bool, timeout_seconds: float, max_steps: int,
                wall_time_limit_seconds: int):
    """A node's agent: their loop, their model client, our environment."""
    from minisweagent.agents.default import DefaultAgent
    from minisweagent.models.litellm_model import LitellmModel

    model = LitellmModel(
        model_name=model_name,
        model_kwargs=model_kwargs,
        # litellm has no price for every model, and mini raises rather than reporting an unknown
        # cost. Ignoring it costs the cost limit — a node is bounded by turns and wall-clock, which
        # are bounds we set ourselves and can reason about.
        cost_tracking="ignore_errors",
    )
    return DefaultAgent(
        model, SandboxEnvironment(tree=tree, network=network, timeout_seconds=timeout_seconds),
        system_template=system_prompt(instructions),
        instance_template=INSTANCE_TEMPLATE,
        step_limit=max_steps,
        wall_time_limit_seconds=wall_time_limit_seconds,
        # Eight, not their default of three. This model answers without a command often enough that
        # three consecutive such turns is not rare — measured, not assumed: the same configuration
        # and the same task succeeded once and then failed with `RepeatedFormatError`, which is the
        # loop working and the bound being too tight for the model it is running.
        max_consecutive_format_errors=8,
    )
