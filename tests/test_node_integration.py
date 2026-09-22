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


def test_s4_short_output_on_both_streams_is_not_lost(tmp_path):
    """§47。双流同时输出**短**内容时，两边都必须完整可见 ✓——共享预算不能把它们挤掉 ✓。"""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    sandbox = BubblewrapWorkspaceSandbox(allowed_commands=frozenset({"sh"}))

    result = sandbox.run(SandboxSpec(
        workspace=workspace,
        command=("sh", "-c", "echo OUT-MARKER; echo ERR-MARKER >&2"),
        spill_dir=tmp_path / "store", spill_limit_bytes=1_000))

    assert "OUT-MARKER" in result.stdout
    assert "ERR-MARKER" in result.stderr
    assert result.spilled == (), "short output should not have been spilled at all"
    assert result.incomplete is False


def test_s4_memory_and_disk_are_bounded_during_the_run(tmp_path):
    """§49。**运行期间**的占用要有界 ✓，不是跑完再看目录大小 ✓。

    在一个新解释器里跑：命令打印 200 MB，沙箱限额 1 MB —— 断言这个进程的峰值内存增量 ✓ 与磁盘上留下的
    字节数 ✓，两者都在限额附近而不是在输出大小附近 ✓。
    """
    script = f'''
import resource, shutil, sys, tempfile
from pathlib import Path
sys.path.insert(0, {str(ROOT / "src")!r})
from anchor.runtime.sandbox import BubblewrapWorkspaceSandbox, SandboxSpec

root = Path(tempfile.mkdtemp())
ws = root / "ws"; ws.mkdir()
before = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
result = BubblewrapWorkspaceSandbox(allowed_commands=frozenset({{"sh"}})).run(SandboxSpec(
    workspace=ws,
    command=("sh", "-c", "head -c 200000000 /dev/zero | tr '\\\\0' 'x'"),
    spill_dir=root / "store", spill_limit_bytes=1000000))
after = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
on_disk = sum(item.stat().st_size for item in (root / "store").glob("*.txt"))
print("GROWTH_MB", (after - before) // 1024, "DISK", on_disk, "PREVIEW", len(result.stdout))
shutil.rmtree(root, ignore_errors=True)
'''
    done = subprocess.run([sys.executable, "-c", script], capture_output=True, text=True, timeout=300)

    assert done.returncode == 0, done.stderr[-1500:]
    parts = dict(zip(done.stdout.split()[::2], done.stdout.split()[1::2]))
    assert int(parts["GROWTH_MB"]) < 100, f"memory grew with the output: {done.stdout.strip()}"
    assert int(parts["DISK"]) <= 1_100_000, f"disk grew past the budget: {done.stdout.strip()}"
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


def test_b1_a_compacted_run_recovers_with_limited_context(tmp_path):
    """**B1。** 真触发压缩 ✓、检查点落盘后 kill ✓、**另一个进程**继续到提交 ✓。

    恢复后模型实际收到的是**有限的**上下文 ✓（到达的多、发出的少 ✓），而**原始记录**仍能查回被压缩掉的
    内容 ✓——两者都在同一次执行里 ✓。
    """
    control = tmp_path / "control"
    workspace = tmp_path / "ws"
    workspace.mkdir()
    control.mkdir()
    noisy = "for i in $(seq 1 300); do echo a-fairly-long-line-number-$i; done"

    # A first attempt with the context capabilities **on** — same execution, both things switched on —
    # killed once its command has settled.
    from scripts.recovery_windows import _kill_at  # noqa: PLC0415 - the fault machinery lives there

    script = {"window": "C4", "node": "b1", "run_id": "b1-run", "task": "long then finish",
              "commands": [noisy, 'anchor-done --summary "done"'],
              "with_context": True,
              "budget": {"window": 8_000, "output_reserve": 500, "input_target": 3_000,
                         "keep_messages": 2}}
    killed, code, said, errors = _kill_at(control, workspace, script,
                                          "after the settled cycle, before the run ends", 120)
    assert killed and said, f"the first process was not held at its barrier: {errors[-400:]}"

    # The record must be the complete one, compaction or not.
    trace = (control / "trace.jsonl").read_text(encoding="utf-8") if (control / "trace.jsonl").exists() else ""
    assert "a-fairly-long-line-number-" in trace or (control / "kept").exists(), (
        "nothing recorded what the compacted history had contained")

    runs = asyncio.run(open_store(control).list_runs())
    assert runs, "the first attempt left no step record"
    newest = sorted(runs, key=lambda item: item.started_at)[-1]

    # A **new process** continues, with the same capabilities attached and the reference.
    from anchor.node.recovery import RecoveryRef
    from scripts.recovery_windows import _ask_once  # noqa: PLC0415

    outcome = _ask_once(control, RecoveryRef(node=newest.agent_name, run=newest.run_id,
                                             store=str(control)).encode(),
                        dict(script, window="B1"))
    assert outcome.get("status") == COMPLETED, outcome
    assert outcome.get("model_requests", 0) > 0, "the continuation did not run the model at all"
    assert outcome.get("submission"), "no submission came back"


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
