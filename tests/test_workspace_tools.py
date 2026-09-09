"""Native workspace tools: an agent reads and writes the workspace it is bound to.

The tools run in the kernel and mutate through the workspace manager, so every
write is committed and audited. A node that does not declare a workspace cannot
use them.
"""

import asyncio
import json
import subprocess
from uuid import uuid4

import pytest

from anchor.domain.admission import RunRequest
from anchor.domain.graph import GraphDefinition, GraphNode, GraphVersion, Trigger
from anchor.domain.project import Project
from anchor.runtime.agent_tools import AgentToolLoop
from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.capabilities import AgentCapability, CapabilityRegistry, CapabilityRegistryError
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.model_gateway import ModelResponse
from anchor.runtime.receiver import DurableExecutionReceiver
from anchor.runtime.sandbox import SubprocessWorkspaceSandbox
from anchor.runtime.workspace_tools import WorkspaceToolset
from anchor.runtime.workspaces import WorkspaceManager, WorkspaceError
from conftest import make_store


def make_repo(tmp_path):
    root = tmp_path / "repo"
    root.mkdir()
    (root / "readme.md").write_text("base\n", encoding="utf-8")
    for args in (["init", "-q"], ["config", "user.email", "t@example.com"],
                 ["config", "user.name", "Test"], ["add", "-A"], ["commit", "-qm", "init"]):
        subprocess.run(["git", "-C", str(root), *args], check=True, capture_output=True)
    sha = subprocess.run(["git", "-C", str(root), "rev-parse", "HEAD"],
                         check=True, capture_output=True, text=True).stdout.strip()
    return root, sha


def setup(tmp_path, *, workspace_metadata=True):
    store = make_store(tmp_path, "wtools.sqlite")
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    root, sha = make_repo(tmp_path)
    store.create_project(Project(project_id="proj-1", name="P", root=str(root)))
    manager = WorkspaceManager(store, root=tmp_path / "worktrees")
    manager.create(project_id="proj-1", base_revision=sha, workspace_id="ws-1")

    metadata = {"workspace_id": "ws-1"} if workspace_metadata else {}
    version = store.publish_graph(GraphVersion.publish(GraphDefinition(
        graph_id="ws-tools", name="WS tools",
        nodes=[GraphNode(id="a", type="agent", name="A", agent_ref="agents.reader",
                         metadata=metadata)]), 1))
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id,
                                           type="manual"))
    store.admit_run(RunRequest(trigger_id=trigger.id, idempotency_key=f"k-{uuid4().hex}",
                               objective="ws", inputs={}))
    asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
    lease = store.claim_ready_agent_node("worker", uuid4())
    return store, artifacts, manager, root, lease




@pytest.fixture
def bound(tmp_path):
    store, artifacts, manager, root, lease = setup(tmp_path)
    toolset = WorkspaceToolset(store, manager, sandbox=SubprocessWorkspaceSandbox())
    try:
        yield store, artifacts, manager, root, lease, toolset
    finally:
        store.close()


def test_read_returns_the_current_revision_content(bound):
    store, _, _, _, lease, toolset = bound
    assert toolset.execute(lease=lease, tool_ref="workspace.read",
                           arguments={"path": "readme.md"}) == "base\n"


def test_write_commits_and_is_then_readable(bound):
    store, _, _, _, lease, toolset = bound
    written = json.loads(toolset.execute(lease=lease, tool_ref="workspace.write",
                                         arguments={"path": "src/app.py",
                                                    "content": "print('hi')\n"}))
    assert written["path"] == "src/app.py" and written["revision"]
    assert toolset.execute(lease=lease, tool_ref="workspace.read",
                           arguments={"path": "src/app.py"}) == "print('hi')\n"
    assert store.get_workspace("ws-1").current_revision == written["revision"]
    assert [item.kind.value for item in store.list_workspace_operations("ws-1")] == [
        "create", "write"]


def test_list_and_exec_run_against_the_pinned_revision(bound):
    _, _, _, _, lease, toolset = bound
    toolset.execute(lease=lease, tool_ref="workspace.write",
                    arguments={"path": "src/app.py", "content": "x\n"})
    listing = json.loads(toolset.execute(lease=lease, tool_ref="workspace.list",
                                         arguments={"prefix": "src/"}))
    assert listing["paths"] == ["src/app.py"]

    executed = json.loads(toolset.execute(lease=lease, tool_ref="workspace.exec",
                                          arguments={"command": ["cat", "readme.md"]}))
    assert executed["returncode"] == 0 and executed["stdout"] == "base\n"


def test_a_node_without_a_workspace_is_refused(tmp_path):
    store, artifacts, manager, root, lease = setup(tmp_path, workspace_metadata=False)
    try:
        toolset = WorkspaceToolset(store, manager, sandbox=SubprocessWorkspaceSandbox())
        with pytest.raises(WorkspaceError, match="does not declare metadata.workspace_id"):
            toolset.execute(lease=lease, tool_ref="workspace.read",
                            arguments={"path": "readme.md"})
    finally:
        store.close()


class _StubTools:
    """Native-only loop: the gateway is never reached."""

    def __init__(self, store):
        self.store = store

        class _Registry:
            @staticmethod
            def tool(ref):
                raise CapabilityRegistryError(ref)
        self.registry = _Registry()


class _ScriptedGateway:
    def __init__(self, calls):
        self.calls = calls

    async def generate_with_tools(self, *, prompt, system_prompt="", tools):
        by_name = {tool.name: tool for tool in tools}
        results = [await by_name[name].call(json.dumps(args)) for name, args in self.calls]
        return ModelResponse(text=json.dumps(results), provider="script", model="script")

    async def generate(self, *, prompt, system_prompt=""):  # pragma: no cover
        return ModelResponse(text="", provider="script", model="script")


def test_agent_tool_loop_routes_workspace_tools_natively(bound):
    store, artifacts, manager, _, lease, toolset = bound
    agent = AgentCapability(ref="agents.reader", model_ref="models.test",
                            tool_refs=["workspace.write", "workspace.read"])
    loop = AgentToolLoop(_StubTools(store), artifacts, native=toolset)
    gateway = _ScriptedGateway([
        ("workspace.write", {"path": "src/out.py", "content": "print('out')\n"}),
        ("workspace.read", {"path": "src/out.py"}),
    ])
    response = asyncio.run(loop.run(gateway, lease=lease, agent=agent, prompt="edit"))
    results = json.loads(response.text)
    assert json.loads(results[0])["revision"]
    assert results[1] == "print('out')\n"
    # The write went through the workspace ledger, not the tool-operation ledger.
    assert store.list_tool_operations(lease.run_id) == []
