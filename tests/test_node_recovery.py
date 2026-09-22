"""第三包的验收矩阵：崩溃边界、恢复判定，以及「不确定」是一等结果。

**这里的 kill 是真的。** C1–C4 与 C9 由 `scripts/recovery_windows.py` 在子进程里跑：节点到达窗口后
写一个字节到管道并阻塞，父进程读到那一个字节才 `SIGKILL` ✓——不是 sleep 猜时刻 ✓。因此这些测试断言
的是**子进程的退出码、计数器的前后值、以及账本** ✓，不是代码里写了什么 ✓。
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

from anchor.node.recovery import (                                              # noqa: E402
    Budget, InvalidReference, RecoveryRef, assess, budget_path,
    continued_messages, load_budget, open_store, save_budget,
)
from anchor.runtime.sandbox import BubblewrapWorkspaceSandbox                   # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "recovery_windows.py"


@pytest.fixture(scope="module", autouse=True)
def needs_a_sandbox():
    try:
        BubblewrapWorkspaceSandbox(allowed_commands=frozenset({"sh"}))
    except RuntimeError as exc:
        pytest.skip(f"no usable sandbox on this machine: {exc}")


@pytest.fixture(scope="module")
def killed(tmp_path_factory) -> dict:
    """Every window, run once, with real kills — the evidence the assertions below are about.

    Module-scoped because a kill takes about half a second and there is no reason to repeat it for each
    assertion. The JSON it writes is the same evidence a person would read.
    """
    root = tmp_path_factory.mktemp("recovery")
    out = root / "evidence.json"
    done = subprocess.run(
        [sys.executable, str(SCRIPT), "--root", str(root), "--json", str(out), "--timeout", "90"],
        capture_output=True, text=True, timeout=900, cwd=str(ROOT))
    assert out.exists(), f"the fault script produced no evidence:\n{done.stdout}\n{done.stderr}"
    return {item["window"]: item for item in json.loads(out.read_text(encoding="utf-8"))}


# ── the reference ────────────────────────────────────────────────────────────────────────────────

def test_a_reference_survives_the_round_trip(tmp_path):
    """The token is the only thing the Graph carries (§19) — it has to come back exactly."""
    original = RecoveryRef(node="exec-7", run="run-abc", store=str(tmp_path),
                           budget=Budget(requests_used=3, requests_allowed=8))

    again = RecoveryRef.decode(original.encode())

    assert again == original
    assert again.budget.remaining == 5


def test_a_reference_that_does_not_check_out_is_refused_by_name(tmp_path):
    """**C7。** 截断、篡改、不是本项目的、以及版本不符——都要**有解释地**拒绝。

    被强行修好的 token 会指向调用方没打算恢复的那次运行 ✓——恢复错了运行比说「恢复不了」更糟 ✓。
    """
    good = RecoveryRef(node="n", run="r", store=str(tmp_path)).encode()
    for label, token in {
        "truncated": good[: len(good) // 2],
        "edited": good[:-4] + "AAAA",
        "not ours": "sp-something-else",
        "empty": "",
    }.items():
        with pytest.raises(InvalidReference):
            RecoveryRef.decode(token)

    # 版本不符：字段不稳定，猜字段就是指向错的运行。
    import base64
    payload = json.dumps({"node": "n", "run": "r", "store": str(tmp_path), "version": 99})
    token = "anchor1." + base64.urlsafe_b64encode(payload.encode()).decode()
    with pytest.raises(InvalidReference, match="version"):
        RecoveryRef.decode(token)


def test_an_unknown_run_is_invalid_and_not_a_fresh_start(tmp_path):
    """**C7。** 引用有效但 store 里没有这次运行：明确失败，**不能偷偷从头开始一个全新任务**。"""
    store = open_store(tmp_path)
    ref = RecoveryRef(node="n", run="run-that-never-existed", store=str(tmp_path))

    verdict = asyncio.run(assess(store, ref))

    assert verdict.action == "invalid"
    assert "nothing to recover from" in verdict.because


# ── the budget ───────────────────────────────────────────────────────────────────────────────────

def test_the_budget_survives_a_restart_and_is_not_handed_back(tmp_path):
    """**C8。** 预算不能因为进程重启而清零。

    框架的文档明确说它不恢复 retry counter 与 capability state ✓——所以这一份由 Anchor 自己保存 ✓。
    """
    save_budget(tmp_path, Budget(requests_used=6, requests_allowed=8))

    assert load_budget(tmp_path).remaining == 2
    assert load_budget(tmp_path).requests_used == 6

    # 没有文件 = 还没被重启过 ✓；损坏的文件**不能**被当成满额 ✓。
    assert load_budget(tmp_path / "elsewhere").requests_allowed == 0
    budget_path(tmp_path).write_text("{not json", encoding="utf-8")
    with pytest.raises(InvalidReference, match="unreadable"):
        load_budget(tmp_path)


def test_a_budget_is_written_atomically(tmp_path):
    """杀掉在写文件的中间，不能留下一个「什么都没花」的预算文件。"""
    save_budget(tmp_path, Budget(requests_used=1, requests_allowed=8))
    save_budget(tmp_path, Budget(requests_used=2, requests_allowed=8))

    assert load_budget(tmp_path).requests_used == 2
    # 写用的临时文件不留下——留下的会在下一次被当成真身读走。
    assert list(tmp_path.glob("*.writing")) == []


# ── the windows, with real kills ─────────────────────────────────────────────────────────────────

def test_every_kill_window_really_reached_its_barrier(killed):
    """**先证明杀对了地方。** 屏障没到就没有证明任何东西——而一个把 EOF 当成信号的父进程会把四个窗口
    都报成成功 ✓（这件事真的发生过 ✓）。"""
    for name in ("C1", "C2", "C3", "C4", "C9"):
        evidence = killed[name]
        assert evidence["killed"] is True, f"{name} was not killed"
        assert evidence["exit_code"] == -9, f"{name} exited {evidence['exit_code']}, not SIGKILL"
        assert evidence["barrier"], f"{name} never reached its barrier — it proved nothing"


def test_c1_and_c2_never_ran_the_command(killed):
    """**C1/C2。** 计数器必须是 0：命令确实没有执行 ✓。

    两者的**判定不同**，而且这个区别是实测出来的 ✓——取决于屏障注册在 `StepPersistence` 的哪一侧 ✓：

    - **C1** 在 `after_model_request` ✓。框架的 `model_request_completed` 已经写下 ✓，没有任何工具调用
      开始 ✓ → `replayable` ✓（计划期望的 K1 ✓）。
    - **C2** 在 `before_tool_execute` ✓。账本里有 `started`、没有终态 ✓ → `uncertain` ✓——**不重放** ✓，
      不是「看起来只读就重放」✗。
    """
    assert killed["C1"]["counter_after"] == 0, "C1 ran the command"
    assert killed["C1"]["verdict"] == "replayable", killed["C1"]["because"]
    assert "model_request_completed" in killed["C1"]["events"], killed["C1"]["events"]

    assert killed["C2"]["counter_after"] == 0, "C2 ran the command"
    assert killed["C2"]["verdict"] == "uncertain", killed["C2"]["because"]
    assert any(status == "started" for _, _, status in killed["C2"]["effects"]), killed["C2"]


def test_a1_a_second_process_finishes_the_work_without_repeating_anything(killed):
    """**A1。** 进程一完成副作用后被 SIGKILL ✓；进程二只拿到恢复引用 ✓，用**同一执行实现**继续到
    **有效提交** ✓。

    计数器仍是 1 ✓、剩余动作确实执行 ✓、调用数可查 ✓。**只读取 messages 不算恢复成功** ✗——所以这里
    断言的是一个提交，不是一个历史。
    """
    evidence = killed["A1"]

    assert evidence["killed"] is True, "the first process was not killed at C4"
    assert evidence["barrier"], "the first process never reached its barrier"
    assert evidence["counter_after"] == 1, (
        f"the command ran {evidence['counter_after']} times — recovery repeated a settled side effect")
    assert evidence["verdict"] == "completed", evidence["because"]
    assert "finished after resuming" in evidence["because"], evidence["because"]
    assert "1 model request" in evidence["because"], evidence["because"]


def test_a2_the_old_snapshot_does_not_cover_a_later_side_effect(killed):
    """**A2。** 已有**旧的** complete 快照，而之后发生了一个未被它覆盖的副作用 ✓——必须报 `uncertain` ✓，
    **不能**从旧快照继续并重跑那条命令 ✓。

    这是本包从 03 手里接过的那条：03 说这个窗口不存在 ✗，源码与应用都表明它存在 ✓。
    """
    evidence = killed["C5"]

    assert evidence["killed"] is True, "C5 was not killed at its barrier"
    assert evidence["barrier"], "C5 never reached its barrier"
    assert evidence["counter_after"] == 2, "both commands should have run"
    assert "complete" in evidence["snapshot"], "no older complete snapshot was present"
    assert evidence["verdict"] == "uncertain", (
        f"continuing from the old snapshot would repeat a settled command: {evidence['because']}")
    assert "not covered" in evidence["because"], evidence["because"]


def test_c3_ran_the_command_and_still_refuses_to_replay(killed):
    """**C3。** 副作用发生了（counter 0→1）✓，而账本只知道它 `started` ✓——所以答案是 `uncertain` ✓，
    **不是**「已完成、可以继续」✗，也不是重放 ✗。"""
    evidence = killed["C3"]

    assert evidence["counter_after"] == 1, "the effect did not happen, so this window proves nothing"
    assert evidence["verdict"] == "uncertain"
    assert any(status == "started" for _, _, status in evidence["effects"]), evidence["effects"]


def test_c4_settles_and_can_be_continued_without_redoing_the_work(killed):
    """**C4。** 每个调用都有终态、且有一个 complete 快照 ✓——可以从那里继续，不重做 ✓。"""
    evidence = killed["C4"]

    assert evidence["counter_after"] == 1
    assert evidence["verdict"] == "continuable", evidence["because"]
    assert "complete" in evidence["snapshot"]
    assert all(status != "started" for _, _, status in evidence["effects"]), evidence["effects"]


def test_c5_the_window_between_the_terminal_record_and_the_snapshot_does_not_exist(killed):
    """**C5。** 在可达的钩子上探测：有终态记录时快照**也已经**在 ✓——所以这个窗口不存在 ✓。

    不是说它不存在 ✓，是**测得**它不存在 ✓。
    """
    evidence = killed["C5"]

    assert evidence["barrier"], "the probe never fired, so nothing was measured"
    assert "tool_call_completed" in evidence["events"], evidence["events"]
    # 同一时刻快照已经在——这正是「没有这个窗口」的意思。
    assert "complete" in evidence["snapshot"], (
        f"a window with a terminal record and no snapshot may exist after all: {evidence}")


def test_c9_the_sandbox_does_not_outlive_the_host(killed):
    """**C9。** 宿主被杀时命令还在跑 ✓——答案由**事后观察**给出，不是由「宿主死了」推断的 ✓。

    并且留下来的进程被记下来又清掉 ✓（§44 要求测试清理自己启动的东西 ✓）。
    """
    evidence = killed["C9"]

    assert evidence["barrier"], "the command never announced itself from inside the sandbox"
    assert "did NOT continue" in evidence["note"] or "DID continue" in evidence["note"]
    assert "0 process(es)" in evidence["note"], (
        f"the test left processes behind: {evidence['note']}")
    # 账本本身分不出「命令停了」与「命令还在跑」——所以它只能说 uncertain ✓。
    assert evidence["verdict"] == "uncertain"


def test_c6_the_same_reference_twice_does_not_confirm_anything_twice(killed):
    """**C6。** 同一个恢复引用重复评估：历史不被覆盖 ✓，副作用不被重复确认 ✓。"""
    assert killed["C6"]["verdict"] == "no-repeat", killed["C6"]["because"]


def test_c7_every_bad_reference_is_explained(killed):
    """**C7。** 四种坏引用，四种都不是「静默从头开始」✓。"""
    assert killed["C7"]["verdict"] == "explicit", killed["C7"]["because"]


def test_c8_the_allowance_is_carried_across_recoveries(killed):
    """**C8。** 两次恢复之后，额度是 6/8 而不是满额 ✓。"""
    assert killed["C8"]["verdict"] == "carried", killed["C8"]["because"]
    assert killed["C8"]["budget"] == "6/8"


# ── continuing, and what it must not do ──────────────────────────────────────────────────────────

def test_continuing_fetches_history_and_runs_nothing(tmp_path, killed):
    """从 C4 的产物继续：拿得到历史 ✓，而且**不因为「拿历史」就跑任何命令** ✓。"""
    control = Path(killed["C4"]["control"])
    store = open_store(control)
    # **Derived from the store**, which is where the attempt exists — not from a file written before the
    # attempt started, which could name a run that never happened.
    runs = asyncio.run(store.list_runs())
    assert runs, "the C4 window recorded no run"
    newest = sorted(runs, key=lambda item: item.started_at)[-1]
    ref = RecoveryRef(node=newest.agent_name, run=newest.run_id, store=str(control))
    before = _counter(control.parent / "workspace")

    messages = asyncio.run(continued_messages(store, ref))

    assert messages, "no history came back"
    assert _counter(control.parent / "workspace") == before, "fetching history ran something"


def test_a_run_with_no_complete_snapshot_refuses_to_continue(tmp_path):
    """没有 complete 快照时继续 = 拒绝 ✓，而不是拿一个 interrupted 的凑合 ✓——那个可能会重放一个
    待定的调用 ✓。"""
    store = open_store(tmp_path)
    ref = RecoveryRef(node="n", run="no-such-run", store=str(tmp_path))

    with pytest.raises(InvalidReference):
        asyncio.run(continued_messages(store, ref))


def _counter(workspace: Path) -> int:
    try:
        return int((workspace / "counter.txt").read_text(encoding="utf-8").strip() or 0)
    except (OSError, ValueError):
        return 0


def test_a3_the_real_recovery_entry_over_three_kinds_of_reference(killed):
    """**A3。** 顺序调用**真实恢复入口**三次 ✓，覆盖三类引用 ✓。

    - `uncertain` → 什么都不做 ✓（0 次模型请求 ✓）
    - `continuable` → 确实继续到提交 ✓
    - `finished` → 把已有的结果交回 ✓（0 次模型请求 ✓）——**已确认的提交不能重跑** ✓

    这不是把 `assess` 调两次 ✓：每一步都是一个新进程跑节点的入口 ✓，所以量到的是调用方真正会得到的
    东西 ✓。它抓出两个真 bug ✓：引用曾指向**上一次**的 run ✗（下一次调用会去评估错的那次 ✓）；从历史
    取回的提交与实时路径报出的**不一致** ✗。
    """
    evidence = killed["A3"]

    assert evidence["verdict"] == "no-repeat", evidence["because"]
    assert "uncertain -> uncertain with 0 model request(s)" in evidence["because"], evidence["because"]
    assert "continuable -> completed" in evidence["because"], evidence["because"]
    assert "finished -> completed with 0 model request(s)" in evidence["because"], evidence["because"]
    assert evidence["counter_after"] == 1, "the command ran more than once across three recoveries"


def test_a4_every_bad_reference_and_broken_file_is_refused_with_a_reason(killed):
    """**A4。** 能解码的引用不等于身份成立 ✓；损坏的文件也不能把异常抛出入口 ✓。

    覆盖：错 store ✓、错 node ✓（**绑定检查** ✓——同一个 store 装着每个节点的每次尝试 ✓，不匹配的引用
    会恢复别人的工作 ✓）、未知 run ✓、版本不符 ✓、负预算 ✓、字段类型错 ✓、以及**真实损坏**的
    `run.json` / 快照 / `events.jsonl` / 预算文件 ✓。**每一次都零命令执行** ✓。

    它抓出两处真泄漏 ✓：损坏的快照与 events 会抛 `ValidationError` ✗ 并逃出 `run_node` ✗——而入口承诺
    的是返回一个状态 ✓。
    """
    evidence = killed["A4"]

    assert evidence["verdict"] == "explicit", evidence["because"]
    assert evidence["counter_after"] == 0, "a refused reference ran a command"
    for expected in ("wrong store -> failed", "wrong node -> failed", "unknown run -> failed",
                     "future version -> refused", "negative budget -> refused",
                     "budget as text -> refused", "corrupt run.json -> invalid",
                     "corrupt 0.json -> invalid", "corrupt events.jsonl -> invalid",
                     "corrupt budget -> refused"):
        assert expected in evidence["because"], f"{expected!r} missing: {evidence['because']}"
    # 引用里的预算不能把控制目录已经花掉的额度还回来。
    assert "remaining 2" in evidence["because"], evidence["because"]


def test_a5_the_budget_agrees_and_stops_when_spent(killed):
    """**A5。** 真请求、真中断、真重启，直到额度**耗尽** ✓——两份独立账目逐轮相符 ✓，而耗尽后**零请求** ✓。

    断言的不是本测试预先选定的数字 ✓（§39 禁止 ✓），而是**两份独立账目的一致** ✓：模型在请求真正到达的
    地方自己记一份 ✓（跨进程 ✓），持久化预算记另一份 ✓——**并且**耗尽后的那一次不得再发请求 ✓。

    第一版只 kill 一次、遇到 completed 就 break ✓，所以"耗尽"从未被走到 ✓；一个 8/8 的预算会变成 9/8 ✓。
    """
    evidence = killed["A5"]

    assert evidence["verdict"] == "agrees-and-stops", evidence["because"]
    assert evidence["because"].count("killed=True") >= 2, "fewer than two real kills: " + evidence["because"]
    assert "budget_exhausted' with 0 request(s)" in evidence["because"], evidence["because"]
    assert evidence["budget"].split("/")[0] == evidence["budget"].split("/")[1], (
        f"the allowance was not spent to the end: {evidence['budget']}")


def test_r2_a_spent_allowance_makes_no_request_at_all(tmp_path):
    """**R2。** 额度为 8/8 时，恢复**一次请求都不能发** ✓，也不能把额度变大 ✓。

    验收的真实反例 ✓：C4 kill 留下可继续快照 ✓，引用与控制目录都设为 8/8 ✓，新进程调用真实恢复入口 ——
    修复前模型端新增 **1** 次调用 ✓、结果 `completed` ✓、预算文件变成 **9/8** ✓。
    """
    from scripts.recovery_windows import _ask_once, _kill_at  # noqa: PLC0415

    from anchor.node.recovery import Budget

    control, workspace = tmp_path / "control", tmp_path / "workspace"
    control.mkdir()
    workspace.mkdir()
    script = {"window": "C4", "node": "r2", "run_id": "r2", "task": "t",
              "history_driven": True, "marker": "EFFECT-", "first": "echo EFFECT-1",
              "then": 'anchor-done --summary "done"'}
    killed, _, said, errors = _kill_at(control, workspace, script,
                                       "after 2 settled request(s), before the run ends", 60)
    assert killed and said, errors[-400:]

    # Spent to the last request, in both places the two accounts live.
    save_budget(control, Budget(requests_used=8, requests_allowed=8))
    log = control / "model-calls.log"
    before = len(log.read_text(encoding="utf-8").splitlines()) if log.exists() else 0
    runs = asyncio.run(open_store(control).list_runs())
    newest = sorted(runs, key=lambda item: item.started_at)[-1]
    token = RecoveryRef(node=newest.agent_name, run=newest.run_id, store=str(control),
                        budget=Budget(requests_used=8, requests_allowed=8)).encode()

    outcome = _ask_once(control, token, script)
    after = len(log.read_text(encoding="utf-8").splitlines()) if log.exists() else 0
    persisted = load_budget(control)

    assert after == before, f"a spent allowance made {after - before} request(s)"
    assert outcome.get("model_requests") == 0, outcome
    assert outcome.get("status") == "budget_exhausted", outcome
    # **Not 9/8.** The file is the record of what was spent, and nothing was.
    assert persisted.requests_used == 8, f"the budget became {persisted}"
    assert persisted.requests_allowed <= 8, f"the allowance was raised: {persisted}"


