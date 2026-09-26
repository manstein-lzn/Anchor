"""Persistent text conversation for the first Anchor Pilot walking skeleton."""

from __future__ import annotations

import asyncio
import dataclasses
import hashlib
import json
import inspect
import threading
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Any

from pydantic_ai import (Agent, CancellationToken, CallDeferred, DeferredToolRequests,
                         DeferredToolResults, RunContext)
from pydantic_ai.messages import ModelMessage, ModelRequest, ModelResponse, TextPart, UserPromptPart

from pydantic_ai_harness.step_persistence import StepPersistence

from anchor.node.model_bridge import model_for
from anchor.runtime.secrets import ChainedSecretProvider, EnvironmentSecretProvider, JsonFileSecretProvider
from anchor.session import Session, SessionStore

INSTRUCTIONS = """You are Anchor Pilot, the user's assistant for working with Anchor.
Help clarify goals, inspect Anchor resources, construct graphs when requested, start and control runs,
and report concrete evidence. Use the Anchor tools for facts and actions; never claim an action happened
unless its tool returned success. Answer in the user's language, clearly and practically.

Doing what the user asked is authorization: when the request names the Graph, Run or change, call the
tool and report the result. Ask the user — with session_ask — only when the request does not say which
object or what change. Deleting a Graph is the one call with its own confirmation step: call
graph_delete and let that step ask, instead of asking the same question again yourself.

A tool result may come back marked interrupted. That means an earlier process stopped before the tool
returned and the outcome is unknown — not that nothing happened. Check the actual state first (the Graph
file, the Run record, the node files), say what you found, and do not repeat the interrupted call
unchanged or report it as success.

When you mention a Graph, a Run or a file from a Run, link it so the user can open it in the workspace:
[the graph](#anchor/graph/<graph>), [the run](#anchor/run/<run>),
[the file](#anchor/artifact/<run>/<node>/<path>). Use the exact identifiers the tools returned, and
keep the link text short."""


@dataclass(frozen=True)
class PilotDeps:
    """The narrow control-plane dependency exposed to Pilot tools."""

    scheduler: Any
    session_id: str


def _payload(body: str, status: int) -> dict[str, Any]:
    try:
        value = json.loads(body)
    except ValueError:
        value = {"error": body}
    return {**value, "http_status": status} if isinstance(value, dict) else {
        "value": value, "http_status": status
    }


def _resource_snapshot(scheduler: Any, target: str) -> tuple[str | None, Any]:
    lookup = getattr(scheduler, "workspace", None)
    workspace = lookup(target) if lookup else None
    if workspace is None:
        return None, None
    path = workspace / "graph.json"
    try:
        raw = path.read_bytes()
    except FileNotFoundError:
        return None, None
    return hashlib.sha256(raw).hexdigest(), json.loads(raw)


_SIDE_EFFECTS = threading.Lock()


def _recorded(ctx: RunContext[PilotDeps], action: str, target: str, mutate: Any,
              verify: Any = None) -> dict[str, Any]:
    """Run a confirmed side effect exactly once per approved tool call.

    The framework owns the human gate; this ledger only closes the crash window between the side
    effect and the saved result. A replay of the same tool call returns the recorded result.
    """
    key = ctx.tool_call_id or action
    sessions = ctx.deps.scheduler.sessions
    # Deferred calls resolve concurrently, so the precondition check, the effect and the record are
    # one critical section. ponytail: one process-wide lock; per-resource locks if throughput matters.
    with _SIDE_EFFECTS:
        if verify is not None:
            problem = verify()
            if problem is not None:
                return problem
        state, recorded = sessions.begin_operation(ctx.deps.session_id, action, key)
        if state == "completed":
            return recorded if isinstance(recorded, dict) else {"result": recorded}
        if state == "uncertain":
            return {"error": "operation outcome is uncertain; it was not replayed",
                    "action": action, "target": target, "uncertain": True}
        body, status = mutate()
        result = _payload(body, status)
        try:
            sessions.finish_operation(ctx.deps.session_id, key, result)
        except (KeyError, ValueError, OSError) as exc:
            result["record_error"] = str(exc)
        return result


def approval_precondition(scheduler: Any, tool_name: str, args: Any) -> dict[str, Any]:
    """The resource state a deferred call was proposed against, captured while the user decides."""
    values = args if isinstance(args, dict) else {}
    graph = values.get("graph") or values.get("name")
    if tool_name not in {"graph_create", "graph_update", "graph_delete", "graph_run"}:
        return {}
    if not isinstance(graph, str) or not graph:
        return {}
    version, current = _resource_snapshot(scheduler, graph)
    return {"graph": graph, "expected_sha256": version, "existed": current is not None}


def _stale(ctx: RunContext[PilotDeps], tool_name: str, graph: str) -> dict[str, Any] | None:
    """Refuse a confirmed call whose target changed while the user was deciding."""
    precondition = ctx.deps.scheduler.sessions.approval_precondition(
        ctx.deps.session_id, ctx.tool_call_id)
    if not precondition or precondition.get("graph") != graph:
        return None
    version, current = _resource_snapshot(ctx.deps.scheduler, graph)
    if tool_name == "graph_create":
        return {"error": f"Graph already exists: {graph}"} if current is not None else None
    if version != precondition.get("expected_sha256"):
        return {"error": "Graph changed after confirmation; review and confirm again",
                "changed": True}
    return None


def _register_tools(agent: Agent[PilotDeps, str]) -> None:  # noqa: C901 - explicit control surface
    """Expose Anchor's control plane as structured tools, not prompt conventions."""

    @agent.tool
    def graph_list(ctx: RunContext[PilotDeps]) -> list[dict[str, Any]]:
        """List saved Graph assets and their current run, if any."""
        scheduler = ctx.deps.scheduler
        result = []
        for workspace in scheduler.workspaces():
            try:
                definition = json.loads((workspace / "graph.json").read_text(encoding="utf-8"))
            except (OSError, ValueError) as exc:
                result.append({"graph": workspace.name, "error": str(exc)})
                continue
            result.append({
                "graph": workspace.name,
                "objective": definition.get("objective", ""),
                "nodes": [item.get("id") for item in definition.get("nodes", [])
                           if isinstance(item, dict)],
                "running": scheduler.running.get(workspace.name),
            })
        return result

    @agent.tool
    def graph_read(ctx: RunContext[PilotDeps], graph: str) -> dict[str, Any]:
        """Read one Graph's canonical JSON definition."""
        workspace = ctx.deps.scheduler.workspace(graph)
        if workspace is None:
            return {"error": f"no such graph: {graph}"}
        try:
            return {"graph": graph, "definition": json.loads(
                (workspace / "graph.json").read_text(encoding="utf-8"))}
        except (OSError, ValueError) as exc:
            return {"error": str(exc)}

    @agent.tool
    def graph_validate(ctx: RunContext[PilotDeps], definition: dict[str, Any]) -> dict[str, Any]:
        """Validate Graph JSON and Plugin references without saving it."""
        try:
            parsed = __import__("anchor.simple.graph", fromlist=["parse"]).parse(definition)
            for node in parsed.nodes.values():
                ctx.deps.scheduler.library.attach(node.plugins)
            return {"valid": True, "nodes": list(parsed.nodes),
                    "entry": parsed.entry()}
        except (ValueError, OSError, TypeError) as exc:
            return {"valid": False, "error": str(exc)}

    @agent.tool
    def plugin_list(ctx: RunContext[PilotDeps]) -> list[dict[str, Any]]:
        """List registered Plugins and availability."""
        return ctx.deps.scheduler.library.catalog()

    @agent.tool
    def plugin_read(ctx: RunContext[PilotDeps], plugin: str) -> dict[str, Any]:
        """Read a Plugin manifest and its progressive-disclosure instructions."""
        try:
            return ctx.deps.scheduler.library.detail(plugin)
        except (ValueError, OSError) as exc:
            return {"error": str(exc)}

    @agent.tool
    def run_list(ctx: RunContext[PilotDeps]) -> list[dict[str, Any]]:
        """List Graph Runs, newest first."""
        return ctx.deps.scheduler.runs()

    @agent.tool
    def run_status(ctx: RunContext[PilotDeps], run: str) -> dict[str, Any]:
        """Read one Run state, traces, Plugin bindings, and node directories."""
        value = ctx.deps.scheduler.run("", run)
        if value is not None:
            return value
        graph = next((name for name, current in ctx.deps.scheduler.running.items() if current == run), None)
        return ({"run": run, "graph": graph, "status": "starting", "running": True}
                if graph is not None else {"error": f"no such run: {run}"})

    @agent.tool
    def artifact_read(ctx: RunContext[PilotDeps], run: str, node: str, path: str) -> dict[str, Any]:
        """Read a text artifact from a node workspace; binary files are reported, not decoded."""
        body, status = ctx.deps.scheduler.read_file(run, node, path)
        return _payload(body, status)

    @agent.tool
    def graph_run(ctx: RunContext[PilotDeps], graph: str, objective: str | None = None) -> dict[str, Any]:
        """Start a Graph Run for a request the user already made, linked to this Pilot Session."""

        def mutate() -> tuple[str, int]:
            body, status = ctx.deps.scheduler.trigger(graph, objective)
            result = _payload(body, status)
            run = result.get("run")
            if status == 202 and isinstance(run, str):
                try:
                    # The run directory is created by the worker thread, so association must not race it.
                    ctx.deps.scheduler.sessions.attach_run(ctx.deps.session_id, run)
                    ctx.deps.scheduler.sessions.append(ctx.deps.session_id, "run.started",
                                                       {"run": run, "graph": graph})
                    result["session"] = ctx.deps.session_id
                except (KeyError, ValueError, OSError) as exc:
                    result["association_error"] = str(exc)
            return json.dumps(result, ensure_ascii=False), status

        return _recorded(ctx, "graph.run", graph, mutate,
                         lambda: _stale(ctx, "graph_run", graph))

    def _run_control(ctx: RunContext[PilotDeps], run: str, action: str) -> dict[str, Any]:
        def mutate() -> tuple[str, int]:
            body, status = ctx.deps.scheduler.control_run(run, action)
            result = _payload(body, status)
            if status == 202:
                try:
                    ctx.deps.scheduler.sessions.append(ctx.deps.session_id, f"run.{action}.asked",
                                                       {"run": run})
                except (KeyError, ValueError, OSError):
                    pass
            return json.dumps(result, ensure_ascii=False), status

        return _recorded(ctx, f"run.{action}", run, mutate)

    @agent.tool
    def run_pause(ctx: RunContext[PilotDeps], run: str) -> dict[str, Any]:
        """Ask a running Graph Run to pause at the next node boundary."""
        return _run_control(ctx, run, "pause")

    @agent.tool
    def run_resume(ctx: RunContext[PilotDeps], run: str) -> dict[str, Any]:
        """Resume a paused or interrupted Graph Run."""
        return _run_control(ctx, run, "resume")

    @agent.tool
    def run_stop(ctx: RunContext[PilotDeps], run: str) -> dict[str, Any]:
        """Stop a Graph Run, cancelling its active node when supported."""
        return _run_control(ctx, run, "stop")

    @agent.tool
    def session_wait(ctx: RunContext[PilotDeps]) -> dict[str, Any]:
        """Return the current Session and associated Run snapshots."""
        try:
            session = ctx.deps.scheduler.sessions.get(ctx.deps.session_id)
        except (KeyError, ValueError) as exc:
            return {"error": str(exc)}
        runs = []
        for run in session.run_ids:
            snapshot = ctx.deps.scheduler.run("", run)
            if snapshot is None:
                graph = next((name for name, current in ctx.deps.scheduler.running.items()
                              if current == run), None)
                snapshot = ({"run": run, "graph": graph, "status": "starting",
                             "running": True} if graph is not None else None)
            if snapshot is not None:
                runs.append(snapshot)
        return {"session": session.model_dump(mode="json"),
                "runs": runs}

    @agent.tool
    def session_ask(ctx: RunContext[PilotDeps], question: str) -> dict[str, Any]:
        """Pause this conversation until the user answers one concrete question.

        The run ends here rather than continuing with tools the answer has not justified yet; the
        user's reply comes back as this call's result.
        """
        if not question.strip():
            return {"error": "question must not be empty"}
        raise CallDeferred(metadata={"question": question.strip()})

    @agent.tool
    def graph_create(ctx: RunContext[PilotDeps], name: str,
                     definition: dict[str, Any]) -> dict[str, Any]:
        """Create a saved Graph the user asked for; the request is the authorization."""
        return _recorded(ctx, "graph.create", name,
                         lambda: ctx.deps.scheduler.create(name, definition),
                         lambda: _stale(ctx, "graph_create", name))

    @agent.tool
    def graph_update(ctx: RunContext[PilotDeps], graph: str,
                     definition: dict[str, Any]) -> dict[str, Any]:
        """Replace a saved Graph the user asked for; the request is the authorization."""
        return _recorded(ctx, "graph.update", graph,
                         lambda: ctx.deps.scheduler.save(graph, definition),
                         lambda: _stale(ctx, "graph_update", graph))

    @agent.tool(requires_approval=True)
    def graph_delete(ctx: RunContext[PilotDeps], graph: str) -> dict[str, Any]:
        """Delete a Graph workspace after explicit confirmation."""
        return _recorded(ctx, "graph.delete", graph,
                         lambda: ctx.deps.scheduler.delete_graph(graph),
                         lambda: _stale(ctx, "graph_delete", graph))


def _compaction(raw: dict[str, Any], profile: dict[str, Any]) -> list[Any]:
    """The harness capability that keeps a long conversation inside the model's window.

    Nothing Anchor-specific is built here: the framework trims, or summarises when a summariser model
    is configured. It compacts the history the run continues from, and the receipt saying how much was
    dropped stays in that history, so the model knows its memory before that point is secondhand. The
    dropped messages remain in that run's earlier snapshots but are no longer what a later turn reads.

    Configuration, all optional:

    ```json
    "pilot_compaction": {"max_messages": 200, "keep_messages": 40,
                         "max_fraction": 0.6, "summarizer_model": "models.deepseek"}
    ```
    """
    from pydantic_ai_harness.compaction import SlidingWindowCompaction, SummarizingCompaction
    settings = raw.get("pilot_compaction") or {}
    if settings.get("enabled") is False:
        return []
    keep = int(settings.get("keep_messages", 40))
    window = int(profile.get("context_window") or 0)
    fraction = float(settings.get("max_fraction", 0.6)) if window else None
    max_messages = settings.get("max_messages", 200)
    strategies: list[Any] = []
    summarizer_ref = settings.get("summarizer_model")
    if summarizer_ref:
        profiles = {item["ref"]: item for item in raw.get("models", [])}
        source = profiles.get(summarizer_ref)
        if source is None:
            raise ValueError(f"pilot_compaction.summarizer_model is not a configured model: {summarizer_ref}")
        secret_file = raw.get("secret_file")
        providers = [EnvironmentSecretProvider()]
        if secret_file:
            providers.append(JsonFileSecretProvider(secret_file))
        secret = ChainedSecretProvider(*providers).get(source["secret_ref"])
        strategies.append(SummarizingCompaction(model=model_for(source, secret=secret),
                                                max_messages=max_messages, max_fraction=fraction,
                                                keep_messages=keep, context_window=window or None,
                                                preserve_first_user_message=True, receipts=True))
    strategies.append(SlidingWindowCompaction(max_messages=max_messages, max_fraction=fraction,
                                              keep_messages=keep, context_window=window or None,
                                              preserve_first_user_message=True, receipts=True))
    return strategies


def _agent(config_path: Path, scheduler: Any = None, session_id: str = "") -> Agent[Any, str]:
    raw = json.loads(config_path.read_text(encoding="utf-8"))
    profiles = {item["ref"]: item for item in raw.get("models", [])}
    ref = raw.get("pilot_model") or next(iter(profiles), None)
    profile = profiles.get(ref)
    if profile is None:
        raise ValueError("runtime config must define at least one model for Anchor Pilot")
    secret_file = raw.get("secret_file")
    providers = [EnvironmentSecretProvider()]
    if secret_file:
        providers.append(JsonFileSecretProvider(secret_file))
    secret = ChainedSecretProvider(*providers).get(profile["secret_ref"])
    agent: Agent[Any, Any] = Agent(model_for(profile, secret=secret),
                                   output_type=[str, DeferredToolRequests],
                                   instructions=INSTRUCTIONS,
                                   capabilities=_compaction(raw, profile))
    if scheduler is not None:
        _register_tools(agent)
    return agent


def _entries(message: ModelRequest | ModelResponse) -> list[dict[str, str]]:
    """One conversation view of one framework message.

    A `ModelRequest` can hold several user prompts — pydantic-ai merges consecutive requests, and a
    prompt whose turn failed is followed by the next one — so each part becomes its own message. The
    assistant's text parts are one reply and stay together.
    """
    if isinstance(message, ModelRequest):
        contents = [part.content for part in message.parts if isinstance(part, UserPromptPart)]
        return [{"role": "user", "text": content.strip()}
                for content in contents if isinstance(content, str) and content.strip()]
    reply = "\n".join(part.content for part in message.parts
                      if isinstance(part, TextPart) and isinstance(part.content, str)).strip()
    return [{"role": "assistant", "text": reply}] if reply else []


def history(store: SessionStore, session: Session) -> list[dict[str, str]]:
    saved = asyncio.run(store.conversation_store().get(conversation_id=session.conversation_id))
    return [entry for message in saved.messages for entry in _entries(message)]


#: Where the framework keeps this Pilot's own file record: `run.json`, `events.jsonl`,
#: `tool_effects.jsonl`, `snapshots/*.json` and `media/*` per run. Native format, never rewritten here.
PILOT_STEPS = Path("state") / "pilot-steps"


def step_store(root: Path):
    """The harness file store for one Anchor data root."""
    from pydantic_ai_harness.step_persistence import FileStepStore
    return FileStepStore(directory=Path(root) / PILOT_STEPS)


def _close_unfinished(messages: list[ModelMessage]) -> list[ModelMessage]:
    """Let PydanticAI close missing results, including when resuming without a new prompt."""
    if not messages:
        return messages
    tail = messages[-1]
    if isinstance(tail, ModelResponse) and tail.tool_calls:
        return [*messages, ModelRequest(parts=[], state="interrupted")]
    if isinstance(tail, ModelRequest):
        return [*messages[:-1], dataclasses.replace(tail, state="interrupted")]
    return messages


def attempt_history(store: SessionStore, session: Session,
                    saved_at: datetime) -> list[ModelMessage] | None:
    """Load a framework snapshot newer than the saved conversation head.

    A turn that was killed or stopped never reaches the save the conversation store gets at the end of
    a normal turn, so the file record is the only place its messages are. Compare native timestamps:
    compaction can make a newer snapshot shorter than the saved conversation.

    The file record outlives the Session it belongs to — the harness store has no delete, and Anchor
    does not delete its files — so a Session created *after* a record was written is a different
    conversation wearing a reused id, and must not inherit it.

    Reading it is best effort. The store is files on disk that another process may have been writing
    when it died, and one unreadable `run.json` anywhere in the store would otherwise fail every
    Session's next message; such a record is reported on stdout and the saved conversation is used
    instead.
    """
    steps = step_store(store.root)
    try:
        runs = [run for run in asyncio.run(steps.list_runs(conversation_id=session.conversation_id))
                if run.started_at >= session.created_at]
    except (ValueError, OSError) as exc:
        print(json.dumps({"pilot_record_unreadable": str(store.root), "error": str(exc)}), flush=True)
        return None
    if not runs:
        return None
    try:
        snapshot = asyncio.run(steps.latest_snapshot(run_id=runs[-1].run_id, include_interrupted=True))
    except LookupError:
        # The process died before the first frontier snapshot; events remain diagnostic only.
        return None
    except (ValueError, OSError) as exc:
        print(json.dumps({"pilot_record_unreadable": runs[-1].run_id, "error": str(exc)}), flush=True)
        return None
    if snapshot is None or snapshot.timestamp <= saved_at:
        return None
    return _close_unfinished(list(snapshot.messages))


def respond(store: SessionStore, config_path: Path, session: Session, prompt: str | None,
            cancellation_token: CancellationToken | None = None, scheduler: Any = None,
            turn_id: str | None = None, emit: Any = None,
            deferred: DeferredToolResults | None = None) -> str | DeferredToolRequests:
    conversation_store = store.conversation_store()
    saved = asyncio.run(conversation_store.get(conversation_id=session.conversation_id))
    messages = saved.messages
    summary = saved.summary
    # Deferred answers must complete the saved live call, not close it as interrupted.
    attempt = attempt_history(store, session, summary.updated_at) if deferred is None else None
    if attempt is not None:
        messages = attempt
    if prompt is not None:
        # A process killed mid-run leaves the attempt in the framework's file record and not in the
        # saved conversation, which still ends at the prompt that started it. Carrying that attempt in
        # is what lets the model see what it already tried and that the result never came back.
        messages = [*messages, ModelRequest(parts=[UserPromptPart(content=prompt)],
                                           state="interrupted" if attempt is not None else "complete")]
        # Persist the user's turn before the provider call. A failed call leaves recoverable input
        # rather than a message that vanished with the HTTP request.
        summary = asyncio.run(conversation_store.save(summary=summary, messages=messages))
    elif deferred is None and attempt is None and (not messages or not isinstance(messages[-1], ModelRequest) or not any(
        isinstance(part, UserPromptPart) for part in messages[-1].parts
    )):
        raise ValueError("there is no unanswered Pilot turn to resume")
    deps = PilotDeps(scheduler, session.id) if scheduler is not None else None
    # Keep the tiny test/provider seam usable for callers that replace `_agent(config_path)`.
    if scheduler is not None and ("scheduler" in inspect.signature(_agent).parameters or
                                 any(p.kind is inspect.Parameter.VAR_KEYWORD
                                     for p in inspect.signature(_agent).parameters.values())):
        agent = _agent(config_path, scheduler=scheduler, session_id=session.id)
    else:
        agent = _agent(config_path)
    kwargs = {"message_history": messages, "cancellation_token": cancellation_token,
              "conversation_id": session.conversation_id,
              # Framework records tool intent before execution; UI chunks are not a safety ledger.
              "capabilities": [StepPersistence(
                  store=step_store(store.root), agent_name="pilot",
                  # A process killed inside a tool never reaches the settled boundary that writes a
                  # complete snapshot; the frontier checkpoint is what the next process can read.
                  capture_frontier=True)]}
    if deferred is not None:
        kwargs["deferred_tool_results"] = deferred
    if turn_id is not None:
        kwargs["run_id"] = turn_id
    if deps is not None and "deps" in inspect.signature(agent.run).parameters:
        kwargs["deps"] = deps
    if emit is None:
        result = asyncio.run(agent.run(None, **kwargs))
    else:
        result = asyncio.run(_stream(agent, kwargs, emit, turn_id))
    if cancellation_token is not None and cancellation_token.cancelled:
        raise InterruptedError("Pilot response was stopped")
    output = result.output
    messages = result.all_messages()
    asyncio.run(conversation_store.save(summary=summary, messages=messages))
    # A deferred output is a real pause: the model run ended waiting on the user, not on a tool result.
    return output if isinstance(output, DeferredToolRequests) else output.strip()


async def _stream(agent: Agent, kwargs: dict, emit: Any, turn_id: str):
    from pydantic_ai.run import AgentRunResultEvent
    from pydantic_ai.ui.vercel_ai import VercelAIEventStream
    result = None
    failure = None

    async def events():
        nonlocal result, failure
        try:
            async with agent.run_stream_events(None, **kwargs) as stream:
                async for event in stream:
                    if isinstance(event, AgentRunResultEvent):
                        result = event.result
                    yield event
        except Exception as exc:  # noqa: BLE001 - re-raise after encoder emits the failure event
            failure = exc
            raise

    encoder = VercelAIEventStream(sdk_version=6, server_message_id=turn_id)
    async for chunk in encoder.transform_stream(events()):
        encoded = chunk.encode(6)
        if encoded != "[DONE]":
            emit(json.loads(encoded))
    if failure is not None:
        raise failure
    if result is None:
        raise RuntimeError("Pilot stream ended without a result")
    return result
