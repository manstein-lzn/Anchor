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

import json
from pathlib import Path

from minisweagent.agents.default import DefaultAgent
from minisweagent.environments.local import LocalEnvironment
from minisweagent.exceptions import FormatError, InterruptAgentFlow, Submitted

from anchor.runtime.execenv import NodeSandbox

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


class SandboxEnvironment(LocalEnvironment):
    """Their local environment, with bwrap between it and the machine, and the way out.

    A real subclass, not a copy: `_check_finished` is theirs, and it is where the completion sentinel
    is recognised. Overriding it is how a node's way out becomes a property of the graph rather than
    a sentence in a prompt.
    """

    def __init__(self, *, tree: Path, node_id: str, routes: tuple[str, ...] = (),
                 network: bool, timeout_seconds: float,
                 inputs: tuple[tuple[str, str], ...] = (), **kwargs) -> None:
        super().__init__(cwd=str(tree), **kwargs)
        self.tree = Path(tree)
        self.node_id = node_id
        # The nodes this one may hand to. Empty means it has a single way out and does not choose.
        self.routes = tuple(routes)
        self.route: str | None = None
        self.network = network
        self.timeout_seconds = timeout_seconds
        # What this node was given, as (where it lives, where it is visible). Read-only, and never
        # copied: a pointer to a predecessor's workspace, which is also why its history comes with it.
        self.inputs = tuple(inputs)
        # What a node's sandbox is wired with is decided in `runtime/execenv.py`, because a second
        # node runner needs the same wiring and must not restate it.
        self.wiring = NodeSandbox(tree=self.tree, node_id=node_id, network=network,
                                  timeout_seconds=timeout_seconds, routes=self.routes, inputs=self.inputs)
        self.sandbox = self.wiring.sandbox
        self.tool_dirs = self.wiring.dirs
        self.readonly_binds = self.wiring.binds
        self.wiring.require_working()

    def _require_working(self) -> None:
        """Run one trivial command with this node's real mounts, before the loop starts.

        Delegated, so that two runners cannot disagree about what a working sandbox is.
        """
        self.wiring.require_working()

    def execute(self, action, cwd: str = "", *, timeout: int | None = None) -> dict:
        command = action.get("command", "")
        ran = self.wiring.run(command, timeout=timeout)
        output = {"output": ran.output, "returncode": ran.returncode,
                  "exception_info": "the command timed out" if ran.timed_out else ""}
        self._check_finished(output)
        return output

    def _check_finished(self, output: dict) -> None:
        """Recognise the way out, which depends on how many ways out there are.

        A node with one edge out finishes the ordinary way: the edge follows from the graph and the
        node has nothing to decide, which is the rule the README states and `_task` writes into the
        prompt. A node with more than one cannot finish that way at all: the ordinary sentinel is not
        accepted, so the loop keeps going and the only exit is `anchor-route`, which names a target.
        Submitting and routing are then the same act, and a node cannot leave the graph without
        having chosen where it goes.

        This read `if not self.routes`, which meant only a node with *no* way out could finish the
        ordinary way — so every non-terminal node in every graph was told by its prompt to run
        `anchor-done` and then refused for doing it, one wasted turn each. A model that reads the
        correction recovers, which is why it survived: the pointer run's log shows `route: "b"` where
        its instruction said `anchor-done`.
        """
        text = output.get("output", "")
        first = next((line.strip() for line in text.lstrip().splitlines() if line.strip()), "")
        if len(self.routes) <= 1:
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


class OpEnvironment(SandboxEnvironment):
    """The same sandbox and the same mounts, with a program deciding instead of a model.

    Nothing here is a second mechanism. The op is given the same read-only pointers, runs in the same
    sandbox, leaves the same commit, and is recorded the same way; what changes is one thing, and this
    class is that one thing. A model can talk itself into believing it has finished — a command
    cannot, so its exit code is taken as the verdict and its output as what it says.
    """

    def _check_finished(self, output: dict) -> None:
        from minisweagent.exceptions import InterruptAgentFlow

        text = output.get("output", "")
        code = int(output.get("returncode") or 0)
        first = next((line.strip() for line in text.lstrip().splitlines() if line.strip()), "")

        def end(status: str, said: str) -> None:
            raise InterruptAgentFlow({"role": "exit", "content": said,
                                      "extra": {"exit_status": status, "submission": said}})

        if code == 127:
            # Not a failed check: a command that is not there. Saying which one beats a bare 127.
            end("CommandNotFound",
                f"the op's command is not on the sandbox PATH (exit 127):\n{text.strip()}")
            return
        if code != 0:
            # A failed pass, and named as one. It is not a budget exit, so a resume does not pick it
            # up and the run does not carry on as if the work had been done.
            end("Failed", text.strip() or f"the op's command exited {code}")
            return
        if first.startswith("ANCHOR_ROUTE:"):
            target = first.split(":", 1)[1].strip()
            if target not in self.routes:
                end("Failed", f"the op routed to {target!r}, which is not a way out of this node. "
                              f"Choose one of: {', '.join(self.routes)}")
                return
            self.route = target
            end("Submitted", "\n".join(text.lstrip().splitlines()[1:]).strip())
            return
        if len(self.routes) > 1:
            # Same rule as an agent's: a node that chooses where the graph goes has to choose.
            end("Failed", f"this node has more than one way out and did not route. Finish by running "
                          f"`anchor-route --to <{'|'.join(self.routes)}> --reason \"…\"`")
            return
        end("Submitted", text.strip())


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


def scripted_model(commands: list[str]):
    """A model that answers with the commands a script wrote down, in order.

    Only the model is replaced. The loop, the sandbox, the mounts, the commits and the record are all
    the real ones, so a run against this is evidence about the runtime rather than about a model — and
    it costs nothing, which matters when the alternative is paying a provider to find out whether an
    accounting change broke the scheduler. `DeterministicModel` is mini-swe-agent's own test double.
    """
    from minisweagent.models.test_models import DeterministicModel, make_output

    return DeterministicModel(
        outputs=[make_output(content="", actions=[{"command": command}]) for command in commands],
        cost_per_call=0.0)


def build_agent(*, tree: Path, node_id: str, instructions: str, routes: tuple[str, ...],
                network: bool, timeout_seconds: float, max_steps: int, wall_time_limit_seconds: int,
                model_name: str = "", model_kwargs: dict | None = None,
                inputs: tuple[tuple[str, str], ...] = (), trace: Path | None = None,
                script: list[str] | None = None, op: str | None = None):
    """A node's agent: their loop, their model client (or a scripted one or an op), our environment.

    An op is one command and no model at all, so it is the scripted model's own mechanism with a
    single written-down action. That is not a shortcut: the loop, the sandbox, the mounts, the commit
    and the record stay exactly what they are for an agent node, which is what makes `op` a second
    kind of node rather than a second way of running one.
    """
    if op is not None:
        script = [op]
    if script is not None:
        model = scripted_model(script)
    else:
        from minisweagent.models.litellm_model import LitellmModel

        model = LitellmModel(
            model_name=model_name,
            model_kwargs=model_kwargs or {},
            # litellm has no price for every model, and mini raises rather than reporting an unknown
            # cost. Ignoring it costs the cost limit — a node is bounded by turns and wall-clock, which
            # are bounds we set ourselves and can reason about.
            cost_tracking="ignore_errors",
        )
    environment = (OpEnvironment(tree=tree, node_id=node_id, routes=routes, network=network,
                                 timeout_seconds=timeout_seconds, inputs=inputs)
                   if op is not None else
                   SandboxEnvironment(tree=tree, node_id=node_id, routes=routes, network=network,
                                      timeout_seconds=timeout_seconds, inputs=inputs))
    return TracingAgent(
        model, environment,
        # One per pass, beside the workspace rather than in it. The workspace is reused across passes
        # and the conversation is not: two passes appended to one file would replay as a conversation
        # with two beginnings, which is not the one either of them had.
        trace=trace or Path(tree).parent / f"{Path(tree).name}.trace.jsonl",
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
