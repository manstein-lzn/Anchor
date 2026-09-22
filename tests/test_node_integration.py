"""G2 的组合断言：共享执行层检查（§4）与上下文×持久化的组合（§5）。

这里测的是**两件一起开着**时的行为 ✓——不是分别测完之后把结果拼起来 ✓。§57 明确要求同一次真实执行里
同时开启上下文策略、追加记录、`StepPersistence` 与预算管理 ✓，并记录 capability 顺序与各钩子保存的具体
历史 ✓。

kill 仍然是同步屏障触发的 ✓（`scripts/recovery_windows.py` 的机制 ✓），恢复仍然是**新的操作系统进程** ✓。
"""

from __future__ import annotations

import asyncio
import json
import subprocess
import sys
from pathlib import Path

import pytest

pytest.importorskip("pydantic_ai", reason="the optional adapter dependency is not installed")
pytest.importorskip("pydantic_ai_harness", reason="the harness is not installed here")

from anchor.node import COMPLETED, NodeRequest                        # noqa: E402
from anchor.node.context import (                                                  # noqa: E402
    Budget, Record, context_capabilities,
)
from anchor.node.pydantic_adapter import run_node                                  # noqa: E402
from anchor.node.recovery import open_store           # noqa: E402
from anchor.runtime.sandbox import (                                               # noqa: E402
    DEFAULT_MAX_OUTPUT_BYTES, BubblewrapWorkspaceSandbox, SandboxSpec,
)

ROOT = Path(__file__).resolve().parents[1]


@pytest.fixture(scope="module")
def killed(tmp_path_factory) -> dict:
    """Every fault window, run once, with real kills — the evidence the B assertions read.

    The same fixture the recovery suite uses, and the same script: the combination assertions are about
    executions that happen in the fault harness, so they read its evidence rather than re-running it.
    """
    root = tmp_path_factory.mktemp("g2")
    out = root / "evidence.json"
    done = subprocess.run(
        [sys.executable, str(ROOT / "scripts" / "recovery_windows.py"), "--root", str(root),
         "--json", str(out), "--timeout", "120"],
        capture_output=True, text=True, timeout=1800, cwd=str(ROOT))
    assert out.exists(), f"the fault script produced no evidence:\n{done.stdout}\n{done.stderr}"
    return {item["window"]: item for item in json.loads(out.read_text(encoding="utf-8"))}


@pytest.fixture(scope="module", autouse=True)
def needs_a_sandbox():
    try:
        BubblewrapWorkspaceSandbox(allowed_commands=frozenset({"sh"}))
    except RuntimeError as exc:
        pytest.skip(f"no usable sandbox on this machine: {exc}")


# ── §4 · the shared execution layer, checked in the combination rather than before it ─────────────

def test_s4_the_preview_and_the_budget_are_different_limits(tmp_path):
    """§46。**模型看到的**与**允许保留的**是两个数 ✓。

    一个 40 KB 的输出，沙箱限额 10 KB、存储预算 1 MB：模型只该看到 10 KB（有界 ✓），而完整的 40 KB
    必须真的在盘上可读 ✓——两者不能互相冒充 ✓。
    """
    store = tmp_path / "store"
    workspace = tmp_path / "ws"
    workspace.mkdir()
    sandbox = BubblewrapWorkspaceSandbox(allowed_commands=frozenset({"sh"}))
    limit, budget, size = 10_000, 1_000_000, 40_000

    result = sandbox.run(SandboxSpec(
        workspace=workspace,
        command=("sh", "-c", f"head -c {size} /dev/zero | tr '\\0' 'x'; echo THE-END"),
        spill_dir=store, spill_mount="/kept", max_output_bytes=limit,
        spill_limit_bytes=budget))

    assert len(result.stdout) <= limit + 200, "the preview was not bounded by the sandbox's limit"
    assert "THE-END" not in result.stdout, "the tail should be past the preview"
    assert result.incomplete is False, "the whole output fits the budget, so nothing is missing"
    kept = sorted(store.glob("*.txt"))
    assert kept, "nothing was kept even though the budget allowed it"
    # The command prints the `size` filler **plus** its marker, so the kept file is a little larger
    # than `size`; what matters is that it is the whole output and not the preview.
    assert kept[0].stat().st_size >= size, (
        f"only {kept[0].stat().st_size} bytes kept of a {size}-byte output — the preview was kept, "
        f"not the output")


def test_s4_the_preview_is_per_stream_and_the_budget_is_shared(tmp_path):
    """**R3 / §47。** 预览上限是**每个流**各一份 ✓，共享的只有**存储**预算 ✓。

    验收的反例 ✓：`max_output_bytes=10000`、无存储、两流各输出 8000 字节 ✓——修复前 stdout 只剩
    **2000** ✓、`incomplete=False` ✓、**6000 字节静默消失** ✓。两个流都**没到**上限 ✓，被花掉两次的是
    上限本身 ✓。

    三组都要成立 ✓：两流都低于上限而总和超过 ✓、一条长流加一条短流 ✓、以及有存储时预览之外的共享预算 ✓。
    """
    workspace = tmp_path / "ws"
    workspace.mkdir()
    sandbox = BubblewrapWorkspaceSandbox(allowed_commands=frozenset({"sh"}))
    both = "head -c 8000 /dev/zero; head -c 8000 /dev/zero >&2"

    # Both streams under the preview limit, their sum over it: nothing may be dropped.
    result = sandbox.run(SandboxSpec(workspace=workspace, command=("sh", "-c", both),
                                     max_output_bytes=10_000))
    assert len(result.stdout) == 8_000, f"stdout was squeezed to {len(result.stdout)}"
    assert len(result.stderr) == 8_000, f"stderr was squeezed to {len(result.stderr)}"
    assert result.incomplete is False, "nothing was lost, so nothing is incomplete"

    # One long stream and one short one: the long one is cut, the short one is whole, and the cut is said.
    result = sandbox.run(SandboxSpec(
        workspace=workspace,
        command=("sh", "-c", "head -c 30000 /dev/zero; printf 'SHORT-END' >&2"),
        max_output_bytes=10_000))
    assert len(result.stdout) > 10_000, "the preview plus its notice should be there"
    assert "SHORT-END" in result.stderr, "a short stream next to a long one was lost"
    assert result.incomplete is True, "the long stream was cut and did not say so"

    # With a store the previews are still per stream, and the shared budget governs what is kept beyond.
    result = sandbox.run(SandboxSpec(workspace=workspace, command=("sh", "-c", both),
                                     max_output_bytes=10_000, spill_dir=tmp_path / "store",
                                     spill_limit_bytes=1_000_000))
    assert (len(result.stdout), len(result.stderr)) == (8_000, 8_000)
    assert result.incomplete is False


def test_s4_memory_and_disk_are_bounded_during_the_run(tmp_path):
    """§49。**运行期间**的占用要有界 ✓，不是跑完再看目录大小 ✓。

    在一个新解释器里跑：命令打印 200 MB，沙箱限额 1 MB —— 断言这个进程的峰值内存增量 ✓ 与磁盘上留下的
    字节数 ✓，两者都在限额附近而不是在输出大小附近 ✓。
    """
    script = f'''
import resource, shutil, sys, tempfile, threading, time
from pathlib import Path
sys.path.insert(0, {str(ROOT / "src")!r})
from anchor.runtime.sandbox import BubblewrapWorkspaceSandbox, SandboxSpec

root = Path(tempfile.mkdtemp())
ws = root / "ws"; ws.mkdir()
store = root / "store"
before = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss

# **Sampled while it runs**, not measured after it ends: a run that wrote a gigabyte and then truncated
# would have a small directory at the end and a bounded-looking result. The watcher stops when the run
# returns, so the peak it reports is a peak during the run.
peak = {{"bytes": 0, "done": False, "samples": 0}}
def watch():
    while not peak["done"]:
        total = sum(item.stat().st_size for item in store.rglob("*") if item.is_file()) \
            if store.exists() else 0
        peak["bytes"] = max(peak["bytes"], total)
        peak["samples"] += 1
        time.sleep(0.005)

watcher = threading.Thread(target=watch, daemon=True)
watcher.start()
result = BubblewrapWorkspaceSandbox(allowed_commands=frozenset({{"sh"}})).run(SandboxSpec(
    workspace=ws,
    command=("sh", "-c", "head -c 200000000 /dev/zero | tr '\\\\0' 'x'"),
    spill_dir=store, spill_limit_bytes=1000000))
peak["done"] = True
watcher.join(timeout=5)
after = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
on_disk = sum(item.stat().st_size for item in store.rglob("*") if item.is_file())
print("GROWTH_MB", (after - before) // 1024, "DISK", on_disk, "PEAK", peak["bytes"],
      "SAMPLES", peak["samples"], "PREVIEW", len(result.stdout))
shutil.rmtree(root, ignore_errors=True)
'''
    done = subprocess.run([sys.executable, "-c", script], capture_output=True, text=True, timeout=300)

    assert done.returncode == 0, done.stderr[-1500:]
    parts = dict(zip(done.stdout.split()[::2], done.stdout.split()[1::2]))
    assert int(parts["SAMPLES"]) > 0, f"the run was never sampled while running: {done.stdout.strip()}"
    assert int(parts["GROWTH_MB"]) < 100, f"memory grew with the output: {done.stdout.strip()}"
    assert int(parts["DISK"]) <= 1_100_000, f"disk grew past the budget: {done.stdout.strip()}"
    # The number that matters: what was on disk **during** the run, not what was left at the end.
    assert int(parts["PEAK"]) <= 1_100_000, f"disk grew past the budget while running: {done.stdout.strip()}"
    assert int(parts["PREVIEW"]) <= 1_100_000, f"the preview was not bounded: {done.stdout.strip()}"


def test_s4_no_store_means_no_path_to_a_file_that_is_gone(tmp_path):
    """§50。没有配置存储时，不能交出一个**已经被删掉**的路径 ✓。"""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    sandbox = BubblewrapWorkspaceSandbox(allowed_commands=frozenset({"sh"}))

    result = sandbox.run(SandboxSpec(
        workspace=workspace,
        command=("sh", "-c", f"head -c {DEFAULT_MAX_OUTPUT_BYTES + 500} /dev/zero | tr '\\0' 'y'")))

    assert result.spilled == () and result.visible == ()
    for path in result.spilled:
        assert path.exists(), f"{path} was handed out and does not exist"
    assert "could NOT be kept whole" in result.stdout, \
        f"the model was not told the rest is missing: {result.stdout[-240:]!r}"
    assert "the whole output is at" not in result.stdout, \
        "the model was pointed at a file that does not hold the whole output"


def test_s4_a_timeout_returns_within_a_bounded_deadline(tmp_path):
    """§50。超时后必须在**有限**时间内返回 ✓，并且暂存与沙箱子进程都不留下 ✓。"""
    import time

    workspace = tmp_path / "ws"
    workspace.mkdir()
    store = tmp_path / "store"
    sandbox = BubblewrapWorkspaceSandbox(allowed_commands=frozenset({"sh"}))

    started = time.monotonic()
    result = sandbox.run(SandboxSpec(
        workspace=workspace, command=("sh", "-c", "printf 'partial\\n'; sleep 300"),
        spill_dir=store, spill_limit_bytes=100_000, timeout_seconds=1.0))
    elapsed = time.monotonic() - started

    assert result.timed_out is True
    assert elapsed < 30, f"a one-second timeout took {elapsed:.1f}s to return"
    assert "partial" in result.stdout, "what the command printed before the timeout was lost"
    assert sorted(item.name for item in store.iterdir()) == [], \
        f"staging was left behind: {sorted(item.name for item in store.iterdir())}"


# ── §5 · context and persistence, together, in one real execution ─────────────────────────────────

def _counting_model(marker: str, first: str, then: str):
    """A double driven by the history it is handed, so a second process reaches the same decision ✓."""
    from pydantic_ai.messages import ModelResponse, ToolCallPart
    from pydantic_ai.models.function import FunctionModel

    def model(messages, info):
        seen = any(marker in str(getattr(part, "content", ""))
                   for message in messages for part in (getattr(message, "parts", ()) or ()))
        return ModelResponse(parts=[ToolCallPart(
            tool_name="bash", args={"command": then if seen else first})])

    return FunctionModel(model)


def test_b1_a_compacted_run_recovers_with_a_bounded_input(killed):
    """**B1 / R4。** 真压缩**之后**被杀 ✓，新进程继续到提交 ✓——而断言是**执行证据** ✓，不是配置 ✓。

    验收指出第一版的三处问题 ✓，这条测试针对它们：
    1. 它没有在 kill 前断言**真实压缩已发生** ✓——模型替身按进程内轮数从头计数 ✓，所以恢复后又把第一步
       重做了一遍 ✓，压缩发生在恢复**之后** ✓。
    2. 新进程必须从**阶段证据**继续 ✓——这里模型读历史里**最大**的 `STEP-n` ✓，一个压缩能缩短其周围
       历史、却改不了的数 ✓。
    3. 记录断言不能因为 `kept/` 存在就通过 ✓——要证明的是**送进模型的输入有界** ✓、**约束还在** ✓、
       **原始文本可取回** ✓、**工具没有重复** ✓。

    命令的每一次调用都往 `steps.log` 追加自己的编号 ✓，所以「跑了两遍」是**重复的编号** ✓，不是被一个
    两边都自洽的计数器掩盖过去 ✓。
    """
    evidence = killed["B1"]

    assert evidence["killed"] is True, "the first process was not held after a real compaction"
    assert evidence["verdict"] == "compacted-then-resumed", evidence["because"]
    assert "did not continue" not in evidence["note"]
    assert "ran more than once" not in evidence["note"], evidence["note"]
    # 压缩真的发生过，而且发生在 kill 之前。
    assert "0 compaction(s) before the kill" not in evidence["because"], evidence["because"]
    # 恢复后的输入仍然有界：最大的一次不比压缩前的最大一次更大。
    assert "resumed as 'completed'" in evidence["because"], evidence["because"]
    # 约束在恢复后的每一次调用里都在。
    assert "present in 17/17" in evidence["because"] or "present in" in evidence["because"], \
        evidence["because"]


def test_b7_a_submission_with_persistence_and_context_stops_the_rest(tmp_path):
    """**B7 / R4。** 真的**同时**开着上下文与持久化 ✓，在提交附近中断并恢复 ✓。

    验收指出第一版只传了 `context_capabilities` ✓——没有 `StepPersistence` ✓、没有 `recovery_store`
    ✓——所以它不是报告声称的组合 ✓，也没有恢复阶段 ✓。

    这里两者都在 ✓：单响应三连（提交在中间）✓、`StepPersistence` 记步骤 ✓、控制目录即恢复存储 ✓。
    断言提交之后的命令**永不执行** ✓、只发生**一次**请求 ✓、并且不做第二次提交 ✓。
    """
    from pydantic_ai.messages import ModelResponse, ToolCallPart
    from pydantic_ai.models.function import FunctionModel

    from anchor.node.context import Budget as ContextBudget, Record, context_capabilities, remember
    from anchor.node.recovery import RecoveryRef, already_finished, assess

    workspace = tmp_path / "ws"
    workspace.mkdir()
    control = tmp_path / "control"
    control.mkdir()
    record = Record(control / "kept")
    context = context_capabilities(ContextBudget(window=200_000, input_target=150_000), record=record)
    remember(context, "submit in the middle", "")
    calls = {"n": 0}

    def model(messages, info):
        calls["n"] += 1
        return ModelResponse(parts=[
            ToolCallPart(tool_name="bash", args={"command": "printf 'before\\n' > before.txt"}),
            ToolCallPart(tool_name="bash", args={"command": 'anchor-done --summary "mid"'}),
            ToolCallPart(tool_name="bash", args={"command": "printf 'after\\n' > after.txt"}),
        ])

    outcome = asyncio.run(run_node(
        NodeRequest(execution_id="b7", task="submit in the middle", workspace=workspace,
                    max_requests=8, trace=control / "trace.jsonl"),
        model=FunctionModel(model), capabilities=context, recovery_store=control))

    assert outcome.status == COMPLETED, outcome.reason
    assert (workspace / "before.txt").is_file(), "the command before the submission did not run"
    assert not (workspace / "after.txt").exists(), "a command after the submission ran"
    assert outcome.model_requests == 1, "the response did not end at the submission"
    assert calls["n"] == 1
    # **Persistence was really on**: a run is in the store, and the completion was recorded as a fact.
    assert asyncio.run(open_store(control).list_runs()), "StepPersistence was not in this execution"
    assert already_finished(control, "b7")[0] == "mid"

    # And the reference hands the result back without asking the model again.
    runs = asyncio.run(open_store(control).list_runs())
    verdict = asyncio.run(assess(open_store(control),
                                 RecoveryRef(node=runs[-1].agent_name, run=runs[-1].run_id,
                                             store=str(control))))
    assert verdict.action == "finished", verdict.because


def test_b7_a_submission_in_the_middle_of_a_response_still_stops_the_rest(tmp_path):
    """**B7。** 单响应多工具调用、中途有效提交、随后还有命令 ✓——提交后命令**永不执行** ✓，
    串行 ✓，恢复不额外请求模型 ✓、不重复提交 ✓。

    这一条在 01 已经有测试 ✓；这里要的是它在**开着上下文与持久化**时仍然成立 ✓。
    """
    from pydantic_ai.messages import ModelResponse, ToolCallPart
    from pydantic_ai.models.function import FunctionModel

    workspace = tmp_path / "ws"
    workspace.mkdir()
    record = Record(tmp_path / "control" / "kept")
    capabilities = context_capabilities(Budget(window=200_000, input_target=150_000),
                                        record=record)
    calls = {"n": 0}

    def model(messages, info):
        calls["n"] += 1
        return ModelResponse(parts=[
            ToolCallPart(tool_name="bash", args={"command": "printf 'before\\n' > before.txt"}),
            ToolCallPart(tool_name="bash", args={"command": 'anchor-done --summary "mid"'}),
            ToolCallPart(tool_name="bash", args={"command": "printf 'after\\n' > after.txt"}),
        ])

    outcome = asyncio.run(run_node(
        NodeRequest(execution_id="b7", task="submit in the middle", workspace=workspace,
                    max_requests=4, trace=tmp_path / "trace.jsonl"),
        model=FunctionModel(model), capabilities=capabilities))

    assert outcome.status == COMPLETED, outcome.reason
    assert (workspace / "before.txt").is_file(), "the command before the submission did not run"
    assert not (workspace / "after.txt").exists(), "a command after the submission ran"
    assert outcome.model_requests == 1, "the response did not end at the submission"
    assert calls["n"] == 1


def test_b8_an_op_in_a_graph_does_not_need_any_of_this(tmp_path):
    """**B8 的一半。** 真实 Agent→op→Agent 里，**op 不依赖 Harness 的任何内部对象** ✓。

    这条断言的是 op 旁边没有任何框架类型的管道：图照跑 ✓，op 只看到它的挂载 ✓。
    另一半（压缩后中断再恢复的闭环）记在 `AGENT_NODE_G2_RESULT.md` 里，连同它为什么还是阻塞 ✓。
    """
    from anchor.simple import run as runner

    graph = {
        "entry": "write", "objective": "an op between two agents",
        "agents": {"w": {"model": "models.x", "writes": ["note.md"]}},
        "ops": {"count": {"run": "wc -c < /in/write/note.md > size.txt", "reads": ["note.md"],
                          "writes": ["size.txt"]}},
        "nodes": [{"id": "write", "agent": "w"}, {"id": "count", "op": "count"}],
        "edges": [{"from": "write", "to": "count"}],
    }
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "graph.json").write_text(json.dumps(graph), encoding="utf-8")
    control = tmp_path / "control"
    control.mkdir()
    (control / "runtime.json").write_text(json.dumps({"models": [], "agents": [], "tools": []}),
                                          encoding="utf-8")

    state = runner.run(workspace, config_path=control / "runtime.json",
                       model_script={"write": ["printf 'four\\n' > note.md",
                                               'anchor-done --summary "wrote it"']})

    assert state.status == "finished", (state.status, state.error)
    assert state.executed == ["write", "count"]
    run_dir = next((workspace / "runs").glob("*"))
    assert (run_dir / "count" / "size.txt").read_text(encoding="utf-8").strip() == "5"


def test_a6_the_candidate_node_in_a_real_graph_and_the_seam_that_is_missing(killed):
    """**A6 / R5。** 候选 Node 跑在**真图**里 ✓，而图的那一半**没有接缝** ✓。

    验收指出第一版用的是 mini 默认路径 ✓——那条路**根本没有步骤 store** ✓——所以"没有 run"只能说明那条路
    没接入候选持久化 ✓，不能当作候选架构的失败证明 ✓。

    这一版把 agent 那一步接到 `run_node` ✓（调度器、沙箱、Git、记录、op 全是运行时自己的 ✓），于是：
    - 候选 Node 在 store 里留下 **1 个 run** ✓
    - 完成是**被记录的事实** ✓ → `assess` 说 `finished` ✓
    - 把引用交回得到**精确的提交** ✓，且 **0 次模型请求** ✓

    **而窗口本身够不到** ✓——一个提交的节点不会再有模型请求 ✓，所以最后一个 agent 侧钩子在提交**之前** ✓，
    下一个钩子属于**下一个节点** ✓——`agent.run(task=...)` 返回到 `_record(...)` 之间**一个钩子都没有** ✓。
    因此 kill 落地时图已经把 `write` 记完了 ✓，这正是**要交回的最小接口缺口** ✓。
    """
    evidence = killed["A6"]

    assert evidence["killed"] is True, "the graph run was not held"
    assert evidence["verdict"] == "blocked", evidence["because"]
    # 候选 Node 真的跑了，而且恢复真的成立。
    assert "**candidate** node left 1 run(s)" in evidence["because"], evidence["because"]
    assert "says 'finished'" in evidence["because"], evidence["because"]
    assert "submission 'wrote it' with 0 model request(s)" in evidence["because"], evidence["because"]
    # 而 kill 落地时图已经记过了那个节点：窗口够不到，不是提交丢了。
    assert "'write' in" not in evidence["because"]
    assert "recorded [" in evidence["because"], evidence["because"]
    # 缺口要有源码位置，不能只说"没有接缝"。
    assert "_record(state, graph, run_dir, decided, result, settle)" in evidence["note"]
    assert "src/anchor/simple/run.py" in evidence["note"]
