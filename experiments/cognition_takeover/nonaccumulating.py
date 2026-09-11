#!/usr/bin/env python
"""Does non-accumulating retrieval make a small state pay for itself?

The behavioural test found the opposite of what everyone assumes: handing the successor a
small state and letting it fetch detail cost three to five times *more* than putting the
material in the prompt. The reason is mechanical — a tool loop re-sends its whole
conversation on every call, so fetched detail is paid for per turn, while detail written
into the initial prompt is paid for once.

That implies a requirement rather than a conclusion: **retrieved content must not
accumulate in the conversation**. This tests whether meeting it changes the answer.

Three ways to give the same successor the same ledger:

  A  the whole material in the prompt, no tool
  B  a small state plus a paging read — fetched pages accumulate in the loop
  C  the same small state plus a query that answers *inside a separate call* — the ledger
     enters that call, is used once, and only the answer returns to the loop

B and C share the same state, so the comparison is between retrieval designs and not
between states. C is what the archived project's Update step does, and what a compaction
implemented as a tool would do: the bulk is read where it is, and only a conclusion travels.

The sub-call keeps the ledger at the *front* of its prompt so repeated queries share a
cached prefix — if that is where the saving is, it should show up as cached tokens rather
than as fewer tokens.

    ANCHOR_DATABASE_URL=... .venv/bin/python experiments/cognition_takeover/nonaccumulating.py

Writes `nonaccumulating_results.json` beside this file.
"""

from __future__ import annotations

import asyncio
import json
import pathlib
import sys
import time
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[2]
HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / "src"))
sys.path.insert(0, str(HERE))

from anchor.runtime.academic import craft_errors, structure_errors  # noqa: E402
from anchor.runtime.capabilities import CapabilityRegistry  # noqa: E402
from anchor.runtime.config import load_runtime_config  # noqa: E402
from anchor.runtime.json_output import extract_json_object  # noqa: E402
from anchor.runtime.model_gateway import ToolFunction, build_model_gateway  # noqa: E402
from anchor.runtime.node_prompt import PromptParts, assemble_prompt  # noqa: E402
from anchor.runtime.secrets import (  # noqa: E402
    ChainedSecretProvider,
    EnvironmentSecretProvider,
    JsonFileSecretProvider,
)
from anchor.runtime.settings import AnchorSettings  # noqa: E402
from anchor.state.relational import RelationalStateStore  # noqa: E402

RUN_ID = "f4eb8ac7-3418-4e8d-8be5-8eaede46e02a"
WRITER_AGENT = "agents.academic.writer"
DELIVERABLE = [
    "有清晰的论证主线（不是文献罗列）",
    "对机制层面的演化给出解释（为什么变、什么权衡）",
    "包含一张对比表，能让读者一眼比较不同方法",
    "把相互冲突的证据摆出来，而不是只呈现一致结论",
    "结尾给出可操作的开放问题",
]
#: How much of an answer may travel back into the loop. This is the whole point: the
#: bulk stays outside. A cap makes the mechanism honest rather than aspirational.
ANSWER_CAP = 3000


def ledger_snapshot(store: RelationalStateStore) -> dict[str, Any]:
    row = next(item for item in store.list_node_runs(RUN_ID)
               if item.node_id == "write" and item.attempt == 0)
    stored = store.get_context_snapshot(row.id)
    if stored is None:
        raise SystemExit(f"no persisted snapshot for {row.id}")
    return dict(stored.snapshot)


def sections(snapshot: dict[str, Any]) -> dict[str, Any]:
    evidence = snapshot.get("evidence") or {}
    return {**evidence, "plan": snapshot.get("plan"),
            "request": snapshot.get("request")}


def paging_tool(snapshot: dict[str, Any]) -> ToolFunction:
    """B's retrieval: a page of the ledger enters the conversation and stays there."""
    available = sections(snapshot)

    async def call(arguments_json: str) -> str:
        try:
            args = json.loads(arguments_json or "{}")
        except json.JSONDecodeError:
            return "arguments must be JSON"
        section = str(args.get("section", ""))
        if section not in available:
            return f"unknown section {section!r}; available: {sorted(available)}"
        value = available[section]
        if isinstance(value, list):
            offset = max(0, int(args.get("offset", 0)))
            limit = min(40, max(1, int(args.get("limit", 10))))
            value = {"total": len(value), "offset": offset,
                     "items": value[offset:offset + limit]}
        return json.dumps(value, ensure_ascii=False)[:12000]

    return ToolFunction(
        name="read_ledger",
        description=(f"读取证据账本的某一节，可分页。section 取其一：{sorted(available)}。"),
        call=call)


def ledger_text(snapshot: dict[str, Any]) -> str:
    return json.dumps(sections(snapshot), ensure_ascii=False)


def subcall_tool(gateway: Any, snapshot: dict[str, Any]) -> tuple[ToolFunction, list[dict]]:
    """C's retrieval: the ledger enters a separate call; only the answer comes back.

    The ledger goes at the front on purpose. Every query therefore shares a cached
    prefix, which is where any saving would come from.
    """
    ledger = ledger_text(snapshot)
    log: list[dict] = []

    async def call(arguments_json: str) -> str:
        try:
            args = json.loads(arguments_json or "{}")
        except json.JSONDecodeError:
            return "arguments must be JSON"
        question = str(args.get("question", "")).strip()
        if not question:
            return "a question is required"
        response = await gateway.generate(
            prompt=(f"{ledger}\n\n"
                    "=== 以上是证据账本 ===\n\n"
                    f"问题：{question}\n\n"
                    "只依据账本回答这个问题。可以引用具体的引用编号、数字与结论。"
                    "不要复述整节内容，只给出回答所需的内容。"),
            system_prompt="你从账本中检索并作答。只输出回答。")
        answer = response.text.strip()[:ANSWER_CAP]
        log.append({"question": question[:200], "answer_chars": len(answer),
                    "input_tokens": response.input_tokens,
                    "cached_tokens": response.cache_read_tokens,
                    "output_tokens": response.output_tokens})
        return answer or "(账本中没有相关内容)"

    return ToolFunction(
        name="query_ledger",
        description=("就证据账本提一个问题，得到答案。账本很大，你不需要、也无法"
                     "把整本账本读进上下文——提问即可。问题要具体。"),
        call=call), log


async def write_paper(gateway: Any, system: str, text: str, tools: list[ToolFunction],
                      *, label: str) -> dict[str, Any]:
    prompt = (
        "你是一个新接手的 agent。下面是你所得到的全部内容。\n\n"
        "=== 你得到的内容 ===\n"
        f"{text}\n"
        "=== 内容结束 ===\n\n"
        "现在请**写出论文**。不要只描述你打算怎么写——直接产出成品。\n\n"
        '返回且只返回一个 JSON 对象：{"thesis": "一句话中心论断", '
        '"manuscript": "Markdown 正文"}。')
    started = time.monotonic()
    if tools:
        response = await gateway.generate_with_tools(
            prompt=prompt, system_prompt=system, tools=tools)
    else:
        response = await gateway.generate(prompt=prompt, system_prompt=system)
    elapsed = time.monotonic() - started
    try:
        payload = extract_json_object(response.text)
    except Exception:  # noqa: BLE001 - a malformed answer is a result, not a crash
        payload = {}
    return {"condition": label, "seconds": round(elapsed, 1),
            "manuscript": str(payload.get("manuscript") or ""),
            "thesis": str(payload.get("thesis") or ""),
            "input_tokens": response.input_tokens,
            "output_tokens": response.output_tokens,
            "cached_tokens": response.cache_read_tokens}


async def judge(gateway: Any, manuscript: str) -> dict[str, Any]:
    if not manuscript:
        return {"met": 0, "of": len(DELIVERABLE), "verdicts": {}}
    prompt = (
        "下面是一篇论文的正文。逐条判断它是否满足下列要求。\n\n"
        "=== 正文 ===\n"
        f"{manuscript[:60000]}\n"
        "=== 正文结束 ===\n\n"
        "=== 要求 ===\n" + "\n".join(f"{i+1}. {r}" for i, r in enumerate(DELIVERABLE)) +
        "\n=== 要求结束 ===\n\n"
        '返回 JSON：{"1": {"met": true|false, "why": "..."}, ...}。只返回 JSON。')
    response = await gateway.generate(
        prompt=prompt, system_prompt="你严格按给定要求判断，不评价文笔。")
    text = response.text.strip()
    start, end = text.find("{"), text.rfind("}")
    try:
        parsed = json.loads(text[start:end + 1]) if start != -1 else {}
    except json.JSONDecodeError:
        parsed = {}
    met = sum(1 for k, v in parsed.items()
              if k.isdigit() and isinstance(v, dict) and v.get("met"))
    return {"met": met, "of": len(DELIVERABLE), "verdicts": parsed}


async def main() -> int:
    settings = AnchorSettings()
    store = RelationalStateStore(settings.require_database_url())
    config = load_runtime_config(settings.runtime_config)
    providers: list[Any] = [EnvironmentSecretProvider()]
    if config.secret_file:
        providers.append(JsonFileSecretProvider(config.secret_file))
    profile = next(m for m in config.models if m.ref == "models.academic")
    gateway = build_model_gateway(profile, ChainedSecretProvider(*providers))

    try:
        snapshot = ledger_snapshot(store)
        registry = CapabilityRegistry(models=config.models, agents=config.agents,
                                      tools=config.tools, verifiers=config.verifiers)
        system = registry.validate_agent(WRITER_AGENT).instructions
        state = (HERE / "derived_B.md").read_text(encoding="utf-8")
        raw = assemble_prompt(PromptParts(
            objective="编译器优化中的代价模型（cost model）近年的发展",
            node_name="Write the reader-facing paper", snapshot=snapshot))

        query_tool, subcall_log = subcall_tool(gateway, snapshot)
        arms: list[tuple[str, str, list[ToolFunction]]] = [
            ("A", raw, []),
            ("B", state, [paging_tool(snapshot)]),
            ("C", state, [query_tool]),
        ]

        results: dict[str, Any] = {"run_id": RUN_ID, "state_chars": len(state),
                                   "raw_chars": len(raw)}
        for label, text, tools in arms:
            print(f"\n── {label}：写作（材料 {len(text):,} 字符，"
                  f"{'无工具' if not tools else tools[0].name}）──")
            before = len(subcall_log)
            paper = await write_paper(gateway, system, text, tools, label=label)
            calls = subcall_log[before:]
            extra = sum(c["input_tokens"] + c["output_tokens"] for c in calls)
            total = paper["input_tokens"] + paper["output_tokens"] + extra
            print(f"   正文 {len(paper['manuscript']):,} 字符，{paper['seconds']} 秒")
            print(f"   主循环 {paper['input_tokens']:,}+{paper['output_tokens']:,}"
                  f"  |  子调用 {len(calls)} 次 {extra:,}  |  合计 {total:,}")
            verdict = await judge(gateway, paper["manuscript"])
            checks = {"structure_errors": structure_errors(paper["manuscript"]),
                      "craft_errors": craft_errors(paper["manuscript"])}
            print(f"   要求 {verdict['met']}/{verdict['of']}")
            results[label] = {"material_chars": len(text), "paper": paper,
                              "subcalls": calls, "subcall_tokens": extra,
                              "total_tokens": total, "judgement": verdict,
                              "deterministic": checks}

        (HERE / "nonaccumulating_results.json").write_text(
            json.dumps(results, ensure_ascii=False, indent=2), encoding="utf-8")

        print("\n=== 汇总（total 含子调用）===")
        base = results["A"]["total_tokens"]
        for label, _, _ in arms:
            r = results[label]
            pct = (r["total_tokens"] / base - 1) * 100
            print(f"  {label}: 要求 {r['judgement']['met']}/{r['judgement']['of']} | "
                  f"正文 {len(r['paper']['manuscript']):>6,} | "
                  f"total {r['total_tokens']:>9,} tok ({pct:+5.0f}%) | "
                  f"子调用 {len(r['subcalls'])} | "
                  f"结构/行文错 {len(r['deterministic']['structure_errors'])}/"
                  f"{len(r['deterministic']['craft_errors'])}")
        return 0
    finally:
        await gateway.close()
        store.close()


if __name__ == "__main__":
    raise SystemExit(asyncio.run(main()))
