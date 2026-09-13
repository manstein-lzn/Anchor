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

import shutil
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
        found = shutil.which("anchor-scholarly")
        self.tool_dirs = (str(Path(found).parent),) if found else ()

    def execute(self, action, cwd: str = "", *, timeout: int | None = None) -> dict:
        command = action.get("command", "")
        result = self.sandbox.run(SandboxSpec(
            workspace=self.tree, command=("sh", "-c", command),
            timeout_seconds=float(timeout or self.timeout_seconds), network=self.network,
            tool_dirs=self.tool_dirs))
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
