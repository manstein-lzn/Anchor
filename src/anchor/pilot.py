"""Persistent text conversation for the first Anchor Pilot walking skeleton."""

from __future__ import annotations

import asyncio
import hashlib
import json
import inspect
import threading
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from pydantic_ai import (Agent, CancellationToken, CallDeferred, DeferredToolRequests,
                         DeferredToolResults, RunContext)
from pydantic_ai.messages import ModelRequest, ModelResponse, TextPart, UserPromptPart

from anchor.node.model_bridge import model_for
from anchor.runtime.secrets import ChainedSecretProvider, EnvironmentSecretProvider, JsonFileSecretProvider
from anchor.session import Session, SessionStore

INSTRUCTIONS = """You are Anchor Pilot, the user's assistant for working with Anchor.
Help clarify goals, inspect Anchor resources, construct graphs when requested, start and control runs,
and report concrete evidence. Use the Anchor tools for facts and actions; never claim an action happened
unless its tool returned success. Ask the user before destructive or high-impact changes. Answer in the
user's language, clearly and practically.

Starting a Graph Run, controlling a Run, and creating, replacing or deleting a Graph all require the
user's confirmation. Call the tool; if it returns confirmation_required, say briefly what will happen
and wait. After the user confirms, call the same tool again with exactly the same arguments."""


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

    @agent.tool(requires_approval=True)
    def graph_run(ctx: RunContext[PilotDeps], graph: str, objective: str | None = None) -> dict[str, Any]:
        """Start a Graph Run after explicit confirmation, associated with this Pilot Session."""

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

    @agent.tool(requires_approval=True)
    def run_pause(ctx: RunContext[PilotDeps], run: str) -> dict[str, Any]:
        """Ask a running Graph Run to pause at the next node boundary."""
        return _run_control(ctx, run, "pause")

    @agent.tool(requires_approval=True)
    def run_resume(ctx: RunContext[PilotDeps], run: str) -> dict[str, Any]:
        """Resume a paused or interrupted Graph Run."""
        return _run_control(ctx, run, "resume")

    @agent.tool(requires_approval=True)
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

    @agent.tool(requires_approval=True)
    def graph_create(ctx: RunContext[PilotDeps], name: str,
                     definition: dict[str, Any]) -> dict[str, Any]:
        """Create a saved Graph after explicit confirmation."""
        return _recorded(ctx, "graph.create", name,
                         lambda: ctx.deps.scheduler.create(name, definition),
                         lambda: _stale(ctx, "graph_create", name))

    @agent.tool(requires_approval=True)
    def graph_update(ctx: RunContext[PilotDeps], graph: str,
                     definition: dict[str, Any]) -> dict[str, Any]:
        """Replace a saved Graph after explicit confirmation."""
        return _recorded(ctx, "graph.update", graph,
                         lambda: ctx.deps.scheduler.save(graph, definition),
                         lambda: _stale(ctx, "graph_update", graph))

    @agent.tool(requires_approval=True)
    def graph_delete(ctx: RunContext[PilotDeps], graph: str) -> dict[str, Any]:
        """Delete a Graph workspace after explicit confirmation."""
        return _recorded(ctx, "graph.delete", graph,
                         lambda: ctx.deps.scheduler.delete_graph(graph),
                         lambda: _stale(ctx, "graph_delete", graph))


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
                                   instructions=INSTRUCTIONS)
    if scheduler is not None:
        _register_tools(agent)
    return agent


def _text(message: ModelRequest | ModelResponse) -> tuple[str, str] | None:
    if isinstance(message, ModelRequest):
        parts = [part.content for part in message.parts if isinstance(part, UserPromptPart)]
        role = "user"
    else:
        parts = [part.content for part in message.parts if isinstance(part, TextPart)]
        role = "assistant"
    content = "\n".join(part if isinstance(part, str) else "" for part in parts).strip()
    return (role, content) if content else None


def history(store: SessionStore, session: Session) -> list[dict[str, str]]:
    saved = asyncio.run(store.conversation_store().get(conversation_id=session.conversation_id))
    return [{"role": role, "text": content}
            for message in saved.messages if (item := _text(message))
            for role, content in [item]]


def respond(store: SessionStore, config_path: Path, session: Session, prompt: str | None,
            cancellation_token: CancellationToken | None = None, scheduler: Any = None,
            turn_id: str | None = None, emit: Any = None,
            deferred: DeferredToolResults | None = None) -> str | DeferredToolRequests:
    conversation_store = store.conversation_store()
    saved = asyncio.run(conversation_store.get(conversation_id=session.conversation_id))
    messages = saved.messages
    summary = saved.summary
    if prompt is not None:
        messages = [*messages, ModelRequest(parts=[UserPromptPart(content=prompt)])]
        # Persist the user's turn before the provider call. A failed call leaves recoverable input
        # rather than a message that vanished with the HTTP request.
        summary = asyncio.run(conversation_store.save(summary=summary, messages=messages))
    elif deferred is None and (not messages or not isinstance(messages[-1], ModelRequest) or not any(
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
              "conversation_id": session.conversation_id}
    if deferred is not None:
        kwargs["deferred_tool_results"] = deferred
    if turn_id is not None:
        kwargs["run_id"] = turn_id
    if deps is not None and "deps" in inspect.signature(agent.run).parameters:
        kwargs["deps"] = deps
    if emit is None:
        result = asyncio.run(agent.run(None, **kwargs))
    else:
        result = asyncio.run(_stream(agent, kwargs, store, emit, turn_id))
    if cancellation_token is not None and cancellation_token.cancelled:
        raise InterruptedError("Pilot response was stopped")
    output = result.output
    messages = result.all_messages()
    asyncio.run(conversation_store.save(summary=summary, messages=messages))
    # A deferred output is a real pause: the model run ended waiting on the user, not on a tool result.
    return output if isinstance(output, DeferredToolRequests) else output.strip()


async def _stream(agent: Agent, kwargs: dict, store: SessionStore, emit: Any, turn_id: str):
    from pydantic_ai.run import AgentRunResultEvent
    from pydantic_ai.ui.vercel_ai import VercelAIEventStream
    from pydantic_ai_harness.step_persistence import SqliteStepStore, StepPersistence

    # Framework records tool intent before execution. UI chunks alone are not a replay safety ledger.
    kwargs["capabilities"] = [StepPersistence(
        store=SqliteStepStore(database=store.root / "state" / "pilot-steps.sqlite"), agent_name="pilot")]
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
