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
import json
from pathlib import Path

from minisweagent.agents.default import DefaultAgent
from minisweagent.environments.local import LocalEnvironment
from minisweagent.exceptions import FormatError, InterruptAgentFlow, Submitted

from anchor.runtime.sandbox import BubblewrapWorkspaceSandbox, SandboxSpec

RULES = """\
You are a research assistant that works inside one directory, and you act by calling the bash tool
you have been given.

Each response must contain at least one tool call. The system runs it, shows you the output, and you
call the next one. A response with no tool call is rejected and you will be asked again, so never
answer with a description of what you intend to do — do it.

The literature tools are on your PATH:

  anchor-scholarly search      --query "..." [--source crossref|arxiv|openalex] [--limit N] [--offset N]
  anchor-scholarly search-many --queries-file q.txt [--source …] [--limit N]
  anchor-scholarly read        --url "https://..." [--offset N] [--page-start N]
  anchor-scholarly read-many   --urls "u1,u2" [--offset N]
  anchor-scholarly citations   --identifier "doi:…|arxiv:…|openalex:…" [--direction cites|cited_by]
  anchor-scholarly sources     ask each source whether it will answer, and say which did

Start with `anchor-scholarly sources`. Which sources answer changes from hour to hour, and one call
tells you which to use — far better than discovering it one failure at a time. A source that refuses
is one to leave alone for now, not a dead end to work around: use the ones that answered.

`search-many` takes a file with one query per line (blank lines and lines starting with # are
ignored) and runs them all in one call. Use it for a list of queries rather than one call each.

A `read` returns the first part of a document and a `next_offset`. Pass it back to read the next
part: `read --url X --offset 24000`. A long paper takes several calls and that is expected — do not
call a paper read when you have only read the beginning of it.

`--source crossref` is the most precise for bibliographic text, `openalex` the broadest and the only
one that reliably returns abstracts, `arxiv` covers preprints and is often rate limited.

They print JSON on success and exit non-zero with a message on stderr on failure. Sources are rate
limited and sometimes refuse: that is information, not a dead end. Try another source, narrow the
query, or move on — and never claim a source was read when it was not.

You are finished only when you run one of the completion commands below. Saying that you are done is
not finishing, and neither is any other command that looks like it: the loop reads the exact output
of those two commands and nothing else.
"""

INSTANCE_TEMPLATE = """\
{{task}}

You work in a directory that already contains everything the previous step produced.

After the completion command, on the following lines, write your summary. It is the only thing that
leaves this step, so it must say what you did and what you could not do.
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
    """Their local environment, with bwrap between it and the machine, and the way out.

    A real subclass, not a copy: `_check_finished` is theirs, and it is where the completion sentinel
    is recognised. Overriding it is how a node's way out becomes a property of the graph rather than
    a sentence in a prompt.
    """

    def __init__(self, *, tree: Path, node_id: str, routes: tuple[str, ...] = (),
                 network: bool, timeout_seconds: float, **kwargs) -> None:
        super().__init__(cwd=str(tree), **kwargs)
        self.tree = Path(tree)
        self.node_id = node_id
        # The nodes this one may hand to. Empty means it has a single way out and does not choose.
        self.routes = tuple(routes)
        self.route: str | None = None
        self.network = network
        self.timeout_seconds = timeout_seconds
        # The command arrives as one shell string, so the shell is the entry point and the sandbox is
        # the boundary. An allowlist of commands would be a second, weaker boundary that the shell
        # can step around anyway.
        self.sandbox = BubblewrapWorkspaceSandbox(allowed_commands=frozenset({"sh"}))
        found = _console_script("anchor-scholarly")
        self.tool_dirs = (str(Path(found).parent),) if found else ()
        self.readonly_binds = _tool_binds(found)

    def execute(self, action, cwd: str = "", *, timeout: int | None = None) -> dict:
        command = action.get("command", "")
        result = self.sandbox.run(SandboxSpec(
            workspace=self.tree, command=("sh", "-c", command),
            timeout_seconds=float(timeout or self.timeout_seconds), network=self.network,
            tool_dirs=self.tool_dirs, readonly_binds=self.readonly_binds,
            env=(("ANCHOR_NODE", self.node_id), ("ANCHOR_ROUTES", ",".join(self.routes)))))
        output = {"output": result.stdout + result.stderr, "returncode": result.returncode,
                  "exception_info": "the command timed out" if result.timed_out else ""}
        self._check_finished(output)
        return output

    def _check_finished(self, output: dict) -> None:
        """Recognise the way out, which depends on how many ways out there are.

        A node with one edge out finishes the ordinary way. A node with more than one cannot finish
        that way at all: the ordinary sentinel is not accepted, so the loop keeps going and the only
        exit is `anchor-route`, which names a target. Submitting and routing are then the same act,
        and a node cannot leave the graph without having chosen where it goes — which is the failure
        a separate route command would have had.
        """
        text = output.get("output", "")
        first = next((line.strip() for line in text.lstrip().splitlines() if line.strip()), "")
        if not self.routes:
            super()._check_finished(output)
            return
        if not first.startswith("ANCHOR_ROUTE:"):
            if first == "COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT":
                # Told, not merely refused: the next turn should be a correction, not a guess.
                output["output"] = (
                    f"{text}\n\n[This node does not finish that way. It has more than one way out, "
                    f"so it must name one: `anchor-route --to <{'|'.join(self.routes)}>`. The "
                    f"ordinary completion command is not accepted here.]")
            return
        target = first.split(":", 1)[1].strip()
        if target not in self.routes:
            output["output"] = (
                f"{text}\n\n[{target!r} is not a way out of this node. Choose one of: "
                f"{', '.join(self.routes)}]")
            return
        self.route = target
        submission = "\n".join(text.lstrip().splitlines()[1:]).strip()
        raise Submitted({"role": "exit", "content": submission,
                         "extra": {"exit_status": "Submitted", "submission": submission,
                                   "route": target}})


class TracingAgent(DefaultAgent):
    """Their loop, with every message written to the trace as it arrives, and a way back in.

    The trace holds the messages *whole*, not a summary of them. A readable projection would be
    nicer to read and would throw away the structure a resume needs — the tool calls, their
    arguments, the results — so the record is the thing itself and reading it is a separate problem.

    It sits beside the node's directory, never inside it. Inside it is a file the agent can read, and
    one did: it found its own conversation, concluded "nothing prior exists except the trace file
    itself", and reasoned about that instead of its task. The directory holds the deliverable; the
    trace is for whoever is watching.

    `resume` is the one place a loop of theirs is written out here rather than called, because
    `run()` starts by clearing `self.messages` and there is no parameter that says otherwise.
    """

    def __init__(self, *args, trace: Path, **kwargs) -> None:
        super().__init__(*args, **kwargs)
        # An instance attribute, not a class one: two nodes could otherwise be pointed at each
        # other's trace, which is the kind of bug that only shows up when something runs twice.
        self._trace = Path(trace)

    def add_messages(self, *messages):
        added = super().add_messages(*messages)
        with self._trace.open("a", encoding="utf-8") as handle:
            for message in added:
                handle.write(json.dumps(message, ensure_ascii=False, default=str) + "\n")
            handle.flush()
        return added

    def resume(self, messages: list[dict]) -> dict:
        """Continue a conversation that a previous process left unfinished.

        Mirrors the body of `run()` deliberately, minus the part that resets `self.messages` and
        minus the parts that only make sense at a beginning. `Submitted` arrives as an
        `InterruptAgentFlow`, exactly as it does there, and the exit message it carries is what ends
        the loop.
        """
        # `role: exit` is mini-swe-agent's own record of why a loop stopped — the submission on the
        # way out, or `LimitsExceeded` on the way to one — and not something the model said. Sending
        # it back is what the provider refuses (`unknown variant 'exit'`), and it is stale besides:
        # this attempt is continuing, so the reason the last one stopped is not part of the
        # conversation. Dropped rather than rewritten, so nothing is put in the model's mouth.
        self.messages = list(messages)
        while self.messages and self.messages[-1].get("role") == "exit":
            self.messages.pop()
        while True:
            try:
                self.step()
                self.n_consecutive_format_errors = 0
            except FormatError as exc:
                self.n_consecutive_format_errors += 1
                if 0 < self.config.max_consecutive_format_errors <= self.n_consecutive_format_errors:
                    self.add_messages(*exc.messages, {
                        "role": "exit", "content": "RepeatedFormatError",
                        "extra": {"exit_status": "RepeatedFormatError", "submission": ""}})
                else:
                    self.add_messages(*exc.messages)
            except InterruptAgentFlow as exc:
                self.add_messages(*exc.messages)
            if self.messages and self.messages[-1].get("role") == "exit":
                break
        return self.messages[-1].get("extra", {})


def build_agent(*, tree: Path, node_id: str, instructions: str, routes: tuple[str, ...],
                model_name: str, model_kwargs: dict, network: bool, timeout_seconds: float,
                max_steps: int, wall_time_limit_seconds: int):
    """A node's agent: their loop, their model client, our environment."""
    from minisweagent.models.litellm_model import LitellmModel

    model = LitellmModel(
        model_name=model_name,
        model_kwargs=model_kwargs,
        # litellm has no price for every model, and mini raises rather than reporting an unknown
        # cost. Ignoring it costs the cost limit — a node is bounded by turns and wall-clock, which
        # are bounds we set ourselves and can reason about.
        cost_tracking="ignore_errors",
    )
    return TracingAgent(
        model, SandboxEnvironment(tree=tree, node_id=node_id, routes=routes, network=network,
                                  timeout_seconds=timeout_seconds),
        trace=Path(tree).parent / f"{Path(tree).name}.trace.jsonl",
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
