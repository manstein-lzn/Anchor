"""Reading a node's workspace over HTTP, and the one thing that must never work.

A file name arrives in a URL. Where it is allowed to point is decided in `_inside`, which **resolves**
rather than looking for `..` in the string — and these tests are the reason for that choice. `a/../../b`,
a percent-encoded `..`, a doubled slash and a symlink all reach elsewhere by different spellings, and a
check that reads the text of the name catches none of them reliably. The symlink cases matter most: the
sandbox deliberately preserves symlinks in a node's workspace, so one pointing out of it is a file that
exists and is reachable, and nothing about its name says so.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from anchor.serve import Scheduler


def _scheduler(tmp_path: Path) -> tuple[Scheduler, str]:
    """A run on disk with one node whose workspace has something in it — and nothing runs."""
    workspace = tmp_path / "workspaces" / "demo"
    node = workspace / "runs" / "r1" / "notes"
    (node / "deep").mkdir(parents=True)
    (node / "summary.md").write_text("# a summary\n", encoding="utf-8")
    (node / "deep" / "data.tsv").write_text("a\tb\n", encoding="utf-8")
    (node / "blob.bin").write_bytes(b"\xff\xfe\x00\x01not text")
    (node / ".git").mkdir()
    (node / ".git" / "config").write_text("secret-ish\n", encoding="utf-8")
    (workspace / "runs" / "r1" / "run.json").write_text(json.dumps({"status": "finished"}))
    (workspace / "graph.json").write_text('{"nodes": []}', encoding="utf-8")
    config = tmp_path / "runtime.json"
    config.write_text('{"models": []}', encoding="utf-8")
    return Scheduler(tmp_path, config), "r1"


def _files(scheduler: Scheduler, node: str = "notes") -> dict:
    body, status = scheduler.files("r1", node)
    assert status == 200, body
    return json.loads(body)


def _read(scheduler: Scheduler, name: str, node: str = "notes") -> tuple[dict, int]:
    body, status = scheduler.read_file("r1", node, name)
    return json.loads(body), status


def test_it_lists_what_the_node_left(tmp_path):
    scheduler, _ = _scheduler(tmp_path)
    listed = {item["path"]: item["size"] for item in _files(scheduler)["files"]}

    assert listed["summary.md"] == len("# a summary\n")
    assert listed["deep/data.tsv"] == len("a\tb\n")


def test_it_does_not_list_the_history(tmp_path):
    """`.git` is how the work is recorded, not part of what the node produced — and it is the largest
    thing in most workspaces."""
    scheduler, _ = _scheduler(tmp_path)

    assert not any(".git" in item["path"] for item in _files(scheduler)["files"])


def test_it_reads_a_text_file_and_says_when_it_is_not_text(tmp_path):
    scheduler, _ = _scheduler(tmp_path)

    text, status = _read(scheduler, "summary.md")
    assert status == 200 and text["binary"] is False and text["text"] == "# a summary\n"

    binary, status = _read(scheduler, "blob.bin")
    assert status == 200 and binary["binary"] is True and binary["text"] == ""


@pytest.mark.parametrize("name", [
    "../run.json",                          # one level up, to the run's own record
    "../../../../etc/passwd",               # all the way out
    "../../../demo/graph.json",             # the graph definition, one directory over
    "....//....//etc/passwd",               # a traversal a naive strip would recompose
    "/etc/passwd",                          # absolute
    "deep/../../../etc/passwd",             # out through a directory that exists
    "..",
])
def test_it_refuses_a_name_that_leaves_the_workspace(tmp_path, name):
    scheduler, _ = _scheduler(tmp_path)

    body, status = _read(scheduler, name)

    assert status == 404, f"{name!r} was read"
    assert scheduler.locate("r1", "notes", name) is None


def test_it_refuses_a_symlink_that_points_out(tmp_path):
    """The one a string check cannot catch.

    The sandbox preserves symlinks in a workspace, so a node can leave one behind, and its name says
    nothing about where it goes. Resolving is what makes this a refusal rather than a read of whatever
    the link happens to name.
    """
    scheduler, _ = _scheduler(tmp_path)
    node = tmp_path / "workspaces" / "demo" / "runs" / "r1" / "notes"
    (node / "absolute-link").symlink_to("/etc/passwd")
    (node / "relative-link").symlink_to("../../../graph.json")

    for name in ("absolute-link", "relative-link"):
        body, status = _read(scheduler, name)
        assert status == 404, f"{name} was followed out of the workspace"
        assert scheduler.locate("r1", "notes", name) is None


def test_it_keeps_a_symlink_that_stays_inside(tmp_path):
    """Refusing those is not the same as refusing all of them: a link to a neighbour is a file in the
    workspace, and reading it is not a boundary crossing."""
    scheduler, _ = _scheduler(tmp_path)
    node = tmp_path / "workspaces" / "demo" / "runs" / "r1" / "notes"
    (node / "shortcut").symlink_to("summary.md")

    body, status = _read(scheduler, "shortcut")

    assert status == 200 and body["text"] == "# a summary\n"


def test_the_node_name_is_checked_too(tmp_path):
    """The other half of the URL. A node name that climbs out reaches the run's directory and every
    other node's workspace from there."""
    scheduler, _ = _scheduler(tmp_path)

    for node in ("..", "../..", "notes/../..", "/etc", "notes/../../r1"):
        body, status = scheduler.files("r1", node)
        assert status == 404, f"node {node!r} was listed: {body}"


def test_a_run_that_is_not_there_is_not_a_500(tmp_path):
    scheduler, _ = _scheduler(tmp_path)

    assert scheduler.files("nope", "notes")[1] == 404
    assert scheduler.read_file("nope", "notes", "summary.md")[1] == 404
    assert scheduler.files("r1", "no-such-node")[1] == 404
    assert scheduler.read_file("r1", "notes", "no-such-file")[1] == 404


def test_deleting_a_run_removes_its_complete_history(tmp_path):
    scheduler, _ = _scheduler(tmp_path)
    run_dir = tmp_path / "workspaces" / "demo" / "runs" / "r1"

    response, status = scheduler.delete_run("r1")

    assert status == 200 and json.loads(response) == {"run": "r1", "deleted": True}
    assert not run_dir.exists()
    assert scheduler.run_dir("r1") is None


def test_deleting_a_missing_run_returns_not_found(tmp_path):
    scheduler, _ = _scheduler(tmp_path)

    response, status = scheduler.delete_run("no-such-run")

    assert status == 404 and "no such run" in response


def test_deleting_a_running_run_is_refused(tmp_path):
    scheduler, _ = _scheduler(tmp_path)
    scheduler.running["demo"] = "r1"
    run_dir = tmp_path / "workspaces" / "demo" / "runs" / "r1"

    response, status = scheduler.delete_run("r1")

    assert status == 409 and "still running" in response
    assert run_dir.exists()


def test_pydantic_trace_is_readable_as_calls_and_results(tmp_path):
    scheduler, _ = _scheduler(tmp_path)
    trace = tmp_path / "workspaces" / "demo" / "runs" / "r1" / "notes.trace.jsonl"
    records = [
        {"kind": "request", "parts": [{"part_kind": "user-prompt", "content": "research"}]},
        {"kind": "response", "parts": [
            {"part_kind": "text", "content": "Checking evidence"},
            {"part_kind": "tool-call", "tool_name": "bash", "args": '{"command":"ls notes"}'}]},
        {"kind": "request", "parts": [{"part_kind": "tool-return", "tool_name": "bash",
                                       "content": "<returncode>0</returncode>\n<output>\npaper.md\n</output>"}]},
        {"role": "exit", "content": "finished", "extra": {}},
    ]
    trace.write_text("".join(json.dumps(item) + "\n" for item in records), encoding="utf-8")

    messages = scheduler.run("demo", "r1")["traces"]["notes"]

    assert [item["role"] for item in messages] == ["user", "assistant", "tool", "exit"]
    assert messages[1]["text"] == "Checking evidence"
    assert messages[1]["commands"] == ["ls notes"]
    assert messages[2]["text"] == "paper.md"
    assert messages[2]["exit_status"] == "succeeded"
