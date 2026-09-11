#!/usr/bin/env python
"""Cognition handoff: can a fresh agent continue from a state, where raw material fails?

A real run is cut at a point — immediately before its first writing attempt — and a
fresh agent is asked the same seven questions about the task. Three conditions:

  A  raw material: the write node's real declared input, verbatim — the evidence
     ledger, the plan, the request. What the graph handed the agent that did the work.
  S  a structured summary: goal, what was done, findings, conflicts, problems, next
     step. What a competent compaction produces today.
  B  a cognition state: contract, situation, experience, intent, and an index of where
     detail lives. The archived project's shape.

S and B are authored from the same persisted facts. The comparison that matters is
S against B: a state is only worth building if the discipline it adds — attention
levels, failures carried as retry conditions, an index of what was dropped — beats a
plain good summary. A is the floor, and it is also the interesting one, because the
raw ledger demonstrably *contains* the facts while stating none of the conclusions.

All three conditions receive the same operational instructions, because those live in
the agent's system prompt in production and withholding them would ask all three a
question none of them can answer.

Grading asks whether each expected fact is *determinable* from an answer, not whether
it is phrased the way the checklist phrases it. An earlier run graded literally and
marked a correct paraphrase as absent, which measured my wording rather than the
agent's knowledge.

    ANCHOR_DATABASE_URL=... .venv/bin/python experiments/cognition_takeover/run.py

Costs a handful of model calls. Writes `results.json` beside this file.
"""

from __future__ import annotations

import asyncio
import json
import os
import pathlib
import sys
import time
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[2]
HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / "src"))
sys.path.insert(0, str(HERE))

from anchor.runtime.capabilities import CapabilityRegistry  # noqa: E402
from anchor.runtime.config import load_runtime_config  # noqa: E402
from anchor.runtime.model_gateway import build_model_gateway  # noqa: E402
from anchor.runtime.node_prompt import PromptParts, assemble_prompt  # noqa: E402
from anchor.runtime.secrets import (  # noqa: E402
    ChainedSecretProvider,
    EnvironmentSecretProvider,
    JsonFileSecretProvider,
)
from anchor.runtime.settings import AnchorSettings  # noqa: E402
from anchor.state.relational import RelationalStateStore  # noqa: E402
from conditions import EXPECTED, QUESTIONS  # noqa: E402

RUN_ID = "f4eb8ac7-3418-4e8d-8be5-8eaede46e02a"
WRITER_AGENT = "agents.academic.writer"


def material(store: RelationalStateStore) -> dict[str, Any]:
    """The real declared input of the write node's first attempt.

    Read through the store's public interface rather than SQL, so this cannot drift
    from how the runtime itself reads a snapshot.
    """
    row = next(item for item in store.list_node_runs(RUN_ID)
               if item.node_id == "write" and item.attempt == 0)
    stored = store.get_context_snapshot(row.id)
    if stored is None:
        raise SystemExit(f"no persisted snapshot for {row.id}")
    return dict(stored.snapshot)


def instructions(config: Any) -> str:
    """The agent's own instructions — the system prompt, in every condition.

    These carry the operational constraints (citation style, paragraph limits, what
    the body may not contain). They are not in the input snapshot, so a comparison
    that omitted them would score all three conditions on a question none can answer.
    """
    registry = CapabilityRegistry(models=config.models, agents=config.agents,
                                  tools=config.tools, verifiers=config.verifiers)
    return registry.validate_agent(WRITER_AGENT).instructions


#: Prompts for the two authored conditions. Both are produced by the same model from
#: the same material, so the comparison is between schemas, not between who wrote them.
#: An earlier version used a hand-written summary, which was 996 characters against the
#: state's 1,814 — that compared an amount of content, not a shape.
SUMMARY_PROMPT = """下面是一个进行中任务的完整材料。请把它压缩成一份给**接任者**的进展总结。

接任者没有看过这些材料，只有你的总结。用你作为一名资深工程师会用的方式总结：
目标、已经完成什么、发现了什么、遇到什么问题、下一步做什么。写多少由你判断，
但要让接任者能接着干下去。

只输出总结本身，不要前言。

=== 材料 ===
{material}
=== 材料结束 ===
"""

STATE_PROMPT = """下面是一个进行中任务的完整材料。请把它压缩成一份给**接任者**的认知状态。

接任者没有看过这些材料，只有你写的这份状态。按下面五个部分写，每部分都可省略（若无内容）：

## Contract
目标；读者/服务对象；明确的范围之外；交付要求。
## Situation
当前什么是已确立的；什么是尚不确定或相互冲突的；什么是受阻的。
## Experience
哪些失败路径不可重复；在什么条件下才允许重试（写成条件式，不是事件叙述）。
## Intent
当前指令是什么；下一个具体动作是什么；不是什么。
## Knowledge Index
你省略掉的细节在原文的哪里、以什么形式存在，可以怎样精确取回。

写多少由你判断。只输出这份状态本身，不要前言。

=== 材料 ===
{material}
=== 材料结束 ===
"""


async def derive(gateway: Any, template: str, material_text: str) -> str:
    response = await gateway.generate(
        prompt=template.format(material=material_text),
        system_prompt="你为接任者写交接材料。只输出成品。")
    return response.text.strip()


async def conditions(gateway: Any, config: Any, store: RelationalStateStore) -> dict[str, str]:
    snap = material(store)
    raw = assemble_prompt(PromptParts(
        objective="编译器优化中的代价模型（cost model）近年的发展",
        node_name="Write the reader-facing paper", snapshot=snap))
    return {
        "A": raw,
        "S": await derive(gateway, SUMMARY_PROMPT, raw),
        "B": await derive(gateway, STATE_PROMPT, raw),
    }


async def ask(gateway: Any, system: str, text: str) -> dict[str, Any]:
    """One fresh agent, one condition. No tools, no memory, no prior turns."""
    prompt = (
        "你是一个新接手的 agent，对这项任务的过去一无所知。"
        "下面是你所得到的全部内容。\n\n"
        "=== 你得到的内容 ===\n"
        f"{text}\n"
        "=== 内容结束 ===\n\n"
        f"{QUESTIONS}")
    started = time.monotonic()
    response = await gateway.generate(prompt=prompt, system_prompt=system)
    return {"seconds": round(time.monotonic() - started, 1), "prompt_chars": len(prompt),
            "answer": response.text, "input_tokens": response.input_tokens,
            "output_tokens": response.output_tokens}


async def grade(gateway: Any, answer: str) -> tuple[dict[str, Any], int, int]:
    """Blind: the grader sees the answer and the facts, never which condition made it."""
    checklist = json.dumps(
        {str(n): {"question": item["question"], "facts": item["facts"]}
         for n, item in EXPECTED.items()}, ensure_ascii=False, indent=1)
    prompt = (
        "下面是一份对某个任务状态的回答，以及一份事实清单。\n\n"
        "=== 回答 ===\n"
        f"{answer}\n"
        "=== 回答结束 ===\n\n"
        "=== 事实清单 ===\n"
        f"{checklist}\n"
        "=== 清单结束 ===\n\n"
        "对清单中的每一条事实，判断**读者能否从这份回答中得到它**。\n"
        "可以用不同措辞，也可以是由回答中的信息直接推得的；"
        "凡是无法得到、或回答明确说无法确定的，判为 false。\n"
        "注意：带不确定限定**不算**已得到（例如把 24 说成「约 26」不算得到 24）。\n\n"
        '返回 JSON：{"1": {"facts": [{"fact": "<事实原文>", "stated": true|false}, ...]}, '
        '"2": {...}}\n只返回 JSON。')
    response = await gateway.generate(
        prompt=prompt, system_prompt="你只判断信息可得性，不评价质量。")
    text = response.text.strip()
    start, end = text.find("{"), text.rfind("}")
    parsed = json.loads(text[start:end + 1]) if start != -1 else {}
    stated = total = 0
    for entry in (parsed.values() if isinstance(parsed, dict) else []):
        facts = entry.get("facts") if isinstance(entry, dict) else entry
        for fact in facts or []:
            if not isinstance(fact, dict):
                continue
            total += 1
            stated += 1 if fact.get("stated") else 0
    return parsed, stated, total


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
        system = instructions(config)
        arms = await conditions(gateway, config, store)
        print(f"instructions  {len(system):,} 字符（三个条件相同）\n")
        for label, text in arms.items():
            print(f"  {label}: {len(text):,} 字符")

        for label in ("S", "B"):
            (HERE / f"derived_{label}.md").write_text(arms[label], encoding="utf-8")
        results: dict[str, Any] = {"run_id": RUN_ID, "instructions_chars": len(system),
                                   "sizes": {k: len(v) for k, v in arms.items()}}
        repeat = int(os.environ.get("TAKEOVER_REPEAT", "1"))
        for label, text in arms.items():
            runs = []
            for i in range(repeat):
                print(f"\n── {label} 第 {i+1}/{repeat} 次：提问 ──")
                answer = await ask(gateway, system, text)
                graded, stated, total = await grade(gateway, answer["answer"])
                print(f"   {len(answer['answer'])} 字符，{answer['seconds']} 秒 "
                      f"→ {stated}/{total}")
                runs.append({**answer, "grading": graded, "stated": stated,
                             "expected": total})
            scores = [r["stated"] for r in runs]
            results[label] = {"material_chars": len(text), "runs": runs,
                              "scores": scores,
                              "mean": round(sum(scores) / len(scores), 1),
                              "expected": runs[0]["expected"]}

        (HERE / "results.json").write_text(
            json.dumps(results, ensure_ascii=False, indent=2), encoding="utf-8")
        print("\n=== 汇总 ===")
        for label, text in arms.items():
            r = results[label]
            print(f"  {label}: {r['scores']} 均值 {r['mean']}/{r['expected']} | "
                  f"材料 {r['material_chars']:>7,} 字符")
        return 0
    finally:
        await gateway.close()
        store.close()


if __name__ == "__main__":
    raise SystemExit(asyncio.run(main()))
