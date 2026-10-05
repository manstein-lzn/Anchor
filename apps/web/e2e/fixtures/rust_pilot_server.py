"""Controlled model only; the Pilot, Harness, HTTP and Rust execution stay real."""

import argparse
import json
from pathlib import Path

from pydantic_ai import Agent, DeferredToolRequests
from pydantic_ai.messages import ToolReturnPart, UserPromptPart
from pydantic_ai.models.function import DeltaToolCall, FunctionModel

from anchor import pilot
from anchor.serve import serve


def agent(config, scheduler=None, session_id=""):  # noqa: C901 - explicit test model sequence
    async def stream(messages, info):  # noqa: C901 - only the provider is synthetic
        mode = ""
        current = {}
        history = {}
        for message in messages:
            for part in message.parts:
                if isinstance(part, UserPromptPart) and isinstance(part.content, str):
                    for token in ("启动并暂停", "继续运行", "重开后读取产物"):
                        if token in part.content:
                            mode, current = token, {}
                if isinstance(part, ToolReturnPart):
                    current[part.tool_name] = part.content
                    history[part.tool_name] = part.content
        plans = {
            "启动并暂停": ["graph_list", "graph_read", "graph_validate", "graph_run", "run_pause"],
            "继续运行": ["session_wait", "run_resume"],
            "重开后读取产物": ["session_wait", "artifact_read"],
        }
        for result in current.values():
            if isinstance(result, dict) and result.get("http_status", 200) >= 400:
                raise RuntimeError(f"Fixture tool failed: {result}")
        for tool in plans[mode]:
            if tool in current:
                continue
            arguments = {}
            if tool in {"graph_read", "graph_run"}:
                arguments = {"graph": "work"}
            elif tool == "graph_validate":
                arguments = {"definition": current["graph_read"]["definition"]}
            elif tool in {"run_pause", "run_resume", "artifact_read"}:
                run = history["graph_run"]["run"]
                arguments = {"run": run}
                if tool == "artifact_read":
                    arguments.update(node="last", path="result.txt")
            with (scheduler.root / "fixture-model-calls.jsonl").open("a") as trace:
                trace.write(json.dumps({"mode": mode, "tool": tool, "arguments": arguments}) + "\n")
            yield {0: DeltaToolCall(name=tool, json_args=json.dumps(arguments),
                                   tool_call_id=f"fixture-{mode}-{tool}")}
            return
        run = history["graph_run"]["run"]
        if mode == "启动并暂停":
            assert current["graph_validate"]["valid"]
            yield f"已发起并请求暂停 [本次运行](#anchor/run/{run})。"
        elif mode == "继续运行":
            yield f"已继续 [本次运行](#anchor/run/{run})。"
        else:
            assert current["artifact_read"]["text"] == "pilot-rust-marker"
            yield f"重开后已核查原产物：[结果文件](#anchor/artifact/{run}/last/result.txt)。"

    built = Agent(FunctionModel(stream_function=stream), output_type=[str, DeferredToolRequests])
    pilot._register_tools(built)
    return built


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--port", type=int, required=True)
    args = parser.parse_args()
    pilot._agent = agent
    serve(args.root, args.config, "127.0.0.1", args.port)
