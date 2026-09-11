#!/usr/bin/env python
"""Behavioural test: does passing the handoff quiz predict writing a better paper?

The handoff experiment measured whether a fresh agent can answer seven questions about a
task given a state, a summary, or the raw material. But those seven questions define the
state's own schema, so a state organised by the criterion satisfying the criterion is
partly circular: it cannot show the criterion is the right one.

This closes that loop. The same three conditions are handed to a fresh agent, which is
asked to *do the next thing* — write the paper — and the output is judged against the
deliverable's stated requirements, which are a different standard from the handoff
questions. If a condition that scores well on the quiz also produces a better paper, the
quiz is a usable proxy. If it does not, the criterion is measuring the wrong thing.

Dereferencing matters here and is why the handoff experiment could not settle this. A
state that says "the detail is in the ledger" is useless to an agent that cannot read
the ledger, so every condition gets the same ledger-managed read tool. Whether a small
state can be *acted on* is the question; whether an agent can use a tool is not.

Judgement is in two parts: the deterministic delivery checks, which are exact and free,
and a blind read of whether the paper does what the request asked for.

    ANCHOR_DATABASE_URL=... .venv/bin/python experiments/cognition_takeover/behavioral.py

Costs a few dozen model calls. Writes `behavioral_results.json` beside this file.
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

#: What the request asked the paper to do. Distinct from the handoff questions: these
#: are properties of the artefact, not of the successor's knowledge about the task.
DELIVERABLE = [
    "有清晰的论证主线（不是文献罗列）",
    "对机制层面的演化给出解释（为什么变、什么权衡）",
    "包含一张对比表，能让读者一眼比较不同方法",
    "把相互冲突的证据摆出来，而不是只呈现一致结论",
    "结尾给出可操作的开放问题",
]

SECTIONS = ("sources", "evidence_notes", "tensions", "unresolved", "coverage",
            "search_log", "plan", "scope")


def ledger_snapshot(store: RelationalStateStore) -> dict[str, Any]:
    row = next(item for item in store.list_node_runs(RUN_ID)
               if item.node_id == "write" and item.attempt == 0)
    stored = store.get_context_snapshot(row.id)
    if stored is None:
        raise SystemExit(f"no persisted snapshot for {row.id}")
    return dict(stored.snapshot)


def read_tool(snapshot: dict[str, Any]) -> ToolFunction:
    """A ledger-managed read: page through one section of the persisted ledger.

    Deliberately small. It exists so that every condition can reach the detail, which is
    what makes the comparison about the state rather than about the tool loop.
    """
    evidence = snapshot.get("evidence") or {}
    available = {**evidence, "plan": snapshot.get("plan"),
                 "scope": (snapshot.get("request") or {}).get("scope"),
                 "request": snapshot.get("request")}

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
            window = value[offset:offset + limit]
            return json.dumps({"section": section, "total": len(value),
                               "offset": offset, "items": window},
                              ensure_ascii=False)[:12000]
        return json.dumps(value, ensure_ascii=False)[:12000]

    return ToolFunction(
        name="read_ledger",
        description=("读取证据账本的某一节，可分页。section 取其一："
                     f"{sorted(available)}；offset/limit 用于列表分页。"),
        call=call)


async def write_paper(gateway: Any, system: str, text: str,
                      snapshot: dict[str, Any]) -> dict[str, Any]:
    prompt = (
        "你是一个新接手的 agent。下面是你所得到的全部内容。\n\n"
        "=== 你得到的内容 ===\n"
        f"{text}\n"
        "=== 内容结束 ===\n\n"
        "现在请**写出论文**。你可以用 read_ledger 工具按节分页读取证据账本，"
        "取回你需要的细节。不要只描述你打算怎么写——直接产出成品。\n\n"
        "返回且只返回一个 JSON 对象：{\"thesis\": \"一句话中心论断\", "
        "\"manuscript\": \"Markdown 正文\"}。")
    started = time.monotonic()
    response = await gateway.generate_with_tools(
        prompt=prompt, system_prompt=system,
        tools=[read_tool(snapshot)])
    elapsed = time.monotonic() - started
    try:
        payload = extract_json_object(response.text)
    except Exception:  # noqa: BLE001 - a malformed answer is a result, not a crash
        payload = {}
    manuscript = str(payload.get("manuscript") or "")
    return {"seconds": round(elapsed, 1), "prompt_chars": len(prompt),
            "manuscript": manuscript, "thesis": str(payload.get("thesis") or ""),
            "response_chars": len(response.text),
            "input_tokens": response.input_tokens, "output_tokens": response.output_tokens,
            "raw": response.text[:500] if not manuscript else ""}


def deterministic(manuscript: str, thesis: str) -> dict[str, Any]:
    """Exact and free: the same checks the real pipeline runs. No judge, no noise."""
    if not manuscript:
        return {"structure_errors": ["no manuscript produced"],
                "craft_errors": [], "has_table": False, "sections": []}
    return {
        "structure_errors": structure_errors(manuscript),
        "craft_errors": craft_errors(manuscript),
        "has_table": "|" in manuscript and "---" in manuscript,
        "sections": [line.strip("# ").strip() for line in manuscript.splitlines()
                     if line.startswith("## ")],
        "chars": len(manuscript),
    }


async def judge(gateway: Any, manuscript: str, thesis: str) -> dict[str, Any]:
    """Blind: the judge sees the paper, never which condition produced it."""
    if not manuscript:
        return {"requirements": {r: False for r in DELIVERABLE}, "note": "empty"}
    prompt = (
        "下面是一篇论文的正文。请逐条判断它是否满足下列要求。\n\n"
        "=== 正文 ===\n"
        f"{manuscript[:60000]}\n"
        "=== 正文结束 ===\n\n"
        "=== 要求 ===\n"
        + "\n".join(f"{i+1}. {r}" for i, r in enumerate(DELIVERABLE)) +
        "\n=== 要求结束 ===\n\n"
        "逐条判断「满足」或「不满足」，并给出一句话理由。"
        '返回 JSON：{"1": {"met": true|false, "why": "..."}, ..., '
        '"overall": "<这段正文最突出的问题是什么，一句话>"}。只返回 JSON。')
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
    return {"verdicts": parsed, "met": met, "of": len(DELIVERABLE)}


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

        raw = assemble_prompt(PromptParts(
            objective="编译器优化中的代价模型（cost model）近年的发展",
            node_name="Write the reader-facing paper", snapshot=snapshot))
        # S and B are the ones this experiment already derived; reuse them so the only
        # thing that changes between the quiz and this test is what is being measured.
        arms = {"A": raw,
                "S": (HERE / "derived_S.md").read_text(encoding="utf-8"),
                "B": (HERE / "derived_B.md").read_text(encoding="utf-8")}

        results: dict[str, Any] = {"run_id": RUN_ID, "deliverable": DELIVERABLE}
        for label, text in arms.items():
            print(f"\n── {label}：写作（材料 {len(text):,} 字符）──")
            paper = await write_paper(gateway, system, text, snapshot)
            checks = deterministic(paper["manuscript"], paper["thesis"])
            print(f"   正文 {len(paper['manuscript']):,} 字符，{paper['seconds']} 秒，"
                  f"结构错 {len(checks['structure_errors'])}，行文错 {len(checks['craft_errors'])}")
            print(f"── {label}：评判 ──")
            verdict = await judge(gateway, paper["manuscript"], paper["thesis"])
            print(f"   满足 {verdict.get('met')}/{verdict.get('of')} 项要求")
            results[label] = {"material_chars": len(text), "paper": paper,
                              "deterministic": checks, "judgement": verdict}

        (HERE / "behavioral_results.json").write_text(
            json.dumps(results, ensure_ascii=False, indent=2), encoding="utf-8")

        print("\n=== 汇总 ===")
        for label in arms:
            r = results[label]
            j = r["judgement"]
            print(f"  {label}: 要求 {j.get('met')}/{j.get('of')} | "
                  f"正文 {len(r['paper']['manuscript']):>6,} 字符 | "
                  f"结构错 {len(r['deterministic']['structure_errors'])} "
                  f"行文错 {len(r['deterministic']['craft_errors'])} | "
                  f"表 {r['deterministic'].get('has_table')}")
        return 0
    finally:
        await gateway.close()
        store.close()


if __name__ == "__main__":
    raise SystemExit(asyncio.run(main()))
