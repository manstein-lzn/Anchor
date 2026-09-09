#!/usr/bin/env bash
# Multi-file, two-node validation: revision lineage and evidence completeness.
#
# A coder fixes a bug across a package and runs the repository's existing test;
# a reviewer then reads the same workspace. The script asserts the lineage
# (coder produces a revision, reviewer consumes it), that the fixed code passes
# the pre-existing test in the read-only sandbox, and that every node's declared
# input records the workspace revision it consumed.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
API="${ANCHOR_API_URL:-http://127.0.0.1:8090}"
TOKEN_FILE="${ANCHOR_TOKEN_FILE:-$ROOT/.local/api-token}"
ANCHOR="$ROOT/.venv/bin/anchor"
PY="$ROOT/.venv/bin/python"
STAMP="$(date +%s)"
PROJECT_ID="lineage-${STAMP}"
WORKSPACE_ID="ws-lineage-${STAMP}"
GRAPH_ID="workspace-lineage"
TIMEOUT="${ANCHOR_VALIDATE_TIMEOUT:-600}"
KEEP="${KEEP:-0}"

TMP="$(mktemp -d)"
RUN=""
AUTH=()
cleanup() {
  # Never delete a repository out from under a run that is still executing:
  # stop it first, otherwise the workspace worktree loses its git directory.
  if [ -n "$RUN" ] && [ "${#AUTH[@]}" -gt 0 ]; then
    curl -sf -X POST "${AUTH[@]}" -d '{"reason":"validation script exiting"}' \
      "$API/api/runs/$RUN/stop" >/dev/null 2>&1 || true
  fi
  if [ "$KEEP" = "1" ]; then echo "kept: $TMP"; else rm -rf "$TMP"; fi
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
step() { echo; echo "== $*"; }
ok() { echo "  PASS  $*"; }
json() { "$PY" -c "import json,sys; d=json.load(sys.stdin); print($1)"; }

TOKEN="$(cat "$TOKEN_FILE")"
AUTH=(-H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json")
curl -sf "${AUTH[@]}" "$API/health/ready" >/dev/null || fail "API not ready"
ok "api ready"

step "1. repository with a package bug and an existing failing test"
REPO="$TMP/repo"
mkdir -p "$REPO/mathlib" "$REPO/tests"
git init -q "$REPO"
git -C "$REPO" config user.email validate@example.com
git -C "$REPO" config user.name Validate
cat > "$REPO/mathlib/calc.py" <<'PYFILE'
def add(a, b):
    """Return the sum of a and b."""
    return a - b
PYFILE
cat > "$REPO/mathlib/__init__.py" <<'PYFILE'
from .calc import add

__all__ = ["add"]
PYFILE
cat > "$REPO/tests/test_calc.py" <<'PYFILE'
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from mathlib import add

assert add(2, 3) == 5, add(2, 3)
assert add(-1, 1) == 0, add(-1, 1)
print("existing test passes")
PYFILE
cat > "$REPO/README.md" <<'MD'
# Lineage validation

`mathlib.add(a, b)` must return the sum. `tests/test_calc.py` currently fails.
MD
git -C "$REPO" add -A
git -C "$REPO" commit -qm init
BASE="$(git -C "$REPO" rev-parse HEAD)"
ok "base ${BASE:0:10}, 4 files"

step "2. project + workspace"
curl -sf -X POST "${AUTH[@]}" -d "{\"project_id\":\"$PROJECT_ID\",\"name\":\"Lineage\",\"root\":\"$REPO\"}" \
  "$API/api/projects" >/dev/null || fail "project registration failed"
curl -sf -X POST "${AUTH[@]}" \
  -d "{\"project_id\":\"$PROJECT_ID\",\"base_revision\":\"$BASE\",\"workspace_id\":\"$WORKSPACE_ID\"}" \
  "$API/api/workspaces" | json "d['state']" | grep -qx active || fail "workspace creation failed"
ok "workspace $WORKSPACE_ID"

step "3. two-node graph: coder -> reviewer"
cat > "$TMP/graph.json" <<JSON
{
  "graph_id": "$GRAPH_ID",
  "name": "Workspace lineage",
  "nodes": [
    { "id": "coder", "type": "agent", "name": "Fix the package", "agent_ref": "agents.coder",
      "metadata": { "workspace_id": "$WORKSPACE_ID" } },
    { "id": "reviewer", "type": "agent", "name": "Review the fix", "agent_ref": "agents.coder",
      "metadata": { "workspace_id": "$WORKSPACE_ID" } }
  ],
  "edges": [ { "source": "coder", "target": "reviewer" } ]
}
JSON
VERSION="$("$ANCHOR" graph install --file "$TMP/graph.json" | json "d['version']['graph_version_id']")"
TRIGGER="$("$ANCHOR" trigger add --version "$VERSION" | json "d['id']")"
RUN="$("$ANCHOR" run start --trigger "$TRIGGER" --idempotency-key "lineage-$STAMP" --objective \
"Fix mathlib/calc.py so add(a, b) returns the sum. Use workspace.list to find the files, workspace.read to inspect mathlib/calc.py and tests/test_calc.py, and workspace.write to correct mathlib/calc.py. Then run the existing test with workspace.exec using command ['python3','-B','tests/test_calc.py'] and confirm it prints 'existing test passes'. Finish with a one-sentence summary." \
  | json "d['run_id']")"
[ -n "$RUN" ] || fail "run admission failed"
ok "run $RUN"

DIGEST="$("$ANCHOR" run watch "$RUN" --timeout "$TIMEOUT" --interval 6)"
echo "$DIGEST" | json "'status='+d['status']+' counts='+str(d['node_status_counts'])"
echo "$DIGEST" | "$PY" -c "import json,sys; d=json.load(sys.stdin); assert d['status']=='completed', d" \
  || fail "run did not complete"
ok "run completed"

step "4. lineage and evidence checks"
"$PY" - "$API" "$TOKEN" "$WORKSPACE_ID" "$REPO" "$ROOT" "$RUN" <<'PYFILE'
import json, subprocess, sys, urllib.request
from pathlib import Path

api, token, workspace_id, repo, root, run_id = sys.argv[1:7]
sys.path.insert(0, str(Path(root) / "src"))

def get(path):
    request = urllib.request.Request(f"{api}{path}", headers={"Authorization": f"Bearer {token}"})
    with urllib.request.urlopen(request) as response:
        return json.load(response)

failures = []
def check(name, condition, detail=""):
    print(f"  {'PASS' if condition else 'FAIL'}  {name}{'' if condition else ' — ' + str(detail)}")
    if not condition:
        failures.append(name)

nodes = {item["node_id"]: item for item in get(f"/api/runs/{run_id}/nodes")}
coder, reviewer = nodes["coder"], nodes["reviewer"]
check("coder completed with a workspace revision",
      coder["status"] == "completed" and (coder.get("output_ref") or "").startswith(f"workspace://{workspace_id}@"),
      coder)
revision = (coder.get("output_ref") or "").split("@", 1)[1]

events = get(f"/api/runs/{run_id}/events?after=0&limit=200")
completed = [event for event in events if event["event_type"] == "node.completed"]
check("both nodes completed", len(completed) == 2, len(completed))
by_node = {event["payload"].get("node_id"): event for event in completed}
check("reviewer also recorded the revision it saw",
      by_node.get("reviewer", {}).get("payload", {}).get("workspace_revision") == revision,
      by_node.get("reviewer", {}).get("payload"))

# The declared input snapshot must record the revision the node consumed (I2/B1).
snapshots = get(f"/api/runs/{run_id}/contexts")
blob = json.dumps(snapshots, ensure_ascii=False)
check("a declared input snapshot records the workspace revision",
      revision in blob and workspace_id in blob,
      [item.get("input_hash") for item in snapshots])

kinds = [item["kind"] for item in get(f"/api/workspaces/{workspace_id}/operations")]
check("ledger records create, write and freeze",
      kinds[:1] == ["create"] and "write" in kinds and kinds[-1] == "freeze", kinds)

dirty = subprocess.run(["git", "-C", repo, "status", "--porcelain", "--untracked-files=no"],
                       capture_output=True, text=True, check=True).stdout.strip()
check("source working tree untouched", dirty == "", dirty)

from anchor.domain.content import workspace_ref
from anchor.runtime.sandbox import BubblewrapWorkspaceSandbox
from anchor.runtime.workspace import execute_in_workspace
from anchor.state.relational import RelationalStateStore

store = RelationalStateStore(f"sqlite:///{Path(root) / '.local' / 'api.sqlite'}")
result = execute_in_workspace(store, workspace_ref(workspace_id, revision),
                              ["python3", "-B", "tests/test_calc.py"],
                              sandbox=BubblewrapWorkspaceSandbox())
store.close()
check("the repository's existing test passes in the sandbox",
      result.ok and "existing test passes" in result.stdout,
      f"rc={result.returncode} out={result.stdout!r} err={result.stderr!r}")

sys.exit(1 if failures else 0)
PYFILE

echo
echo "LINEAGE VALIDATION PASSED"
