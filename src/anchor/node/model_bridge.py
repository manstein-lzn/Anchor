"""Turn the graph's model configuration into the model the node runtime is handed.

The scheduler has always built a model out of `runtime.json` — a profile with a provider, a base URL
and a secret reference — and handed it to whatever loop ran the node. That loop was mini's
`LitellmModel`. After ADR-062 it is a PydanticAI model, and this is where the same configuration
becomes one. Nothing about the profiles changes: the same `ref` resolves the same endpoint and the same
secret, so a graph does not have to be edited because the loop behind it was replaced.

**Why a bridge and not a dependency on litellm.** PydanticAI reaches providers through its own model
classes. Keeping litellm underneath would mean carrying a client for a loop that no longer exists, to
call endpoints PydanticAI already knows how to call. The one thing worth keeping is the *shape* of the
configuration, and that is what this preserves.

**Nothing here is imported until it is needed, and that is load-bearing.** `run.py` imports this
module on the default path, so a module-scope `pydantic_ai` import here would make the whole graph
runtime require the harness the moment it was loaded — the invariant A1–A13 hold, and hold by running
a graph in an interpreter where the framework is not importable at all.

**`ANCHOR_MODEL_SCRIPT` is honoured here, and that is the point of the `scripted` branch.** The
provider-free test path replaces the model and nothing else — the loop, the sandbox, the mounts, the
commits and the record stay real. A scripted model is a `FunctionModel` that answers with the commands
the script wrote down, one request at a time, and it is how the whole suite runs without paying anyone.
"""

from __future__ import annotations

from typing import Any


def _scripted(commands: list[str]) -> Any:
    """A model that answers with written-down commands, in order, and refuses to invent a new one.

    One command per request rather than all of them at once: a node's pass is a sequence of commands
    separated by observations, and a script that fired its whole list in one response would test
    neither the loop nor the boundary after a submission. Past the end of the script the model says
    nothing useful, which is how a test proves the loop stopped instead of being asked again.
    """
    from pydantic_ai.messages import ModelResponse, ToolCallPart
    from pydantic_ai.models.function import FunctionModel

    # Test fixture normalization only: model_script predates structured output. The production Agent
    # runtime never parses these strings or exposes a sentinel protocol.
    served = {"count": 0}

    def answer(messages: list[Any], info: Any) -> ModelResponse:
        at = served["count"]
        served["count"] += 1
        # A scripted shell step may choose a route conditionally. Read the actual tool observation,
        # rather than guessing from the command text (which can contain several branches).
        for message in reversed(messages):
            for part in getattr(message, "parts", ()) or ():
                if getattr(part, "part_kind", "") != "tool-return":
                    continue
                content = str(getattr(part, "content", "") or "")
                observed = content
                if "<output>" in observed and "</output>" in observed:
                    observed = observed.split("<output>", 1)[1].split("</output>", 1)[0]
                lines = observed.splitlines()
                for index, line in enumerate(lines):
                    if line.startswith("ANCHOR_ROUTE: "):
                        summary = "\n".join(lines[index + 1:]).strip() or "routed"
                        return ModelResponse(parts=[ToolCallPart(
                            tool_name="final_result",
                            args={"summary": summary, "route": line.split(":", 1)[1].strip()})])
        if at >= len(commands):
            return ModelResponse(parts=[ToolCallPart(tool_name="final_result",
                                                     args={"summary": "completed"})])
        command = commands[at]
        if command.startswith("anchor-done --summary"):
            summary = command.split("--summary", 1)[1].strip().strip("'\"")
            return ModelResponse(parts=[ToolCallPart(tool_name="final_result",
                                                     args={"summary": summary})])
        if command.startswith("anchor-route --to"):
            fields = command.split()
            route = fields[fields.index("--to") + 1]
            return ModelResponse(parts=[ToolCallPart(tool_name="final_result",
                                                     args={"summary": "routed", "route": route})])
        return ModelResponse(parts=[ToolCallPart(tool_name="bash", args={"command": command})])

    return FunctionModel(answer)


def scripted_models(script: dict[str, list[str]] | None) -> dict[str, Any]:
    """One scripted model per node id, for a whole run.

    Per node and not per run: two nodes in one graph are two conversations, and a single counter shared
    between them would serve node B's first command to node A's second request.
    """
    if not script:
        return {}
    return {node_id: _scripted(list(commands)) for node_id, commands in script.items()}


def model_for(profile: dict[str, Any], *, secret: str) -> Any:
    """One provider profile as a PydanticAI model.

    The profile's own fields decide the wire, and the secret is passed in already resolved so this
    function never reads one itself: where a credential comes from is `runtime/secrets.py`'s business,
    and a second reader is a second place it can be logged.
    """
    from pydantic_ai.models.openai import OpenAIChatModel
    from pydantic_ai.providers.openai import OpenAIProvider

    model_name = str(profile.get("model") or "")
    if not model_name:
        raise ValueError(f"model profile {profile.get('ref')!r} names no model")
    base_url = profile.get("base_url")
    provider = OpenAIProvider(base_url=base_url, api_key=secret) if base_url else OpenAIProvider(
        api_key=secret)
    # A base URL means an OpenAI-compatible endpoint that is not OpenAI's, in which case the graph's
    # `model` field is the name the endpoint knows. Without one, the name is passed through and the
    # provider resolves it.
    return OpenAIChatModel(model_name if base_url else _qualified(model_name), provider=provider)


def _qualified(name: str) -> str:
    """`openai/gpt-4o` is litellm's spelling; the provider here is already known, so it is dropped.

    Kept as a function rather than done inline because the two spellings coexisting is exactly the kind
    of thing that reads as a typo later. An unqualified name is passed through unchanged, which is what
    every profile in this repository already uses.
    """
    return name.split("/", 1)[1] if name.startswith("openai/") else name
