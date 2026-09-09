#!/usr/bin/env bash
# End-to-end validation of the content plane against a real model.
#
# Creates a throwaway repository with a bug, asks a real agent to fix it inside a
# workspace, and verifies the full chain: the node's output is an immutable
# workspace revision, the fix is correct when executed in the read-only sandbox,
# the audit ledger is complete, and the source repository is untouched.
#
# Requires the dev services to be running. Exits non-zero on any failure.
#   KEEP=1 ./scripts/validate_workspace.sh   # keep the temp repo for debugging
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
API="${ANCHOR_API_URL:-http://127.0.0.1:8090}"
TOKEN_FILE="${ANCHOR_TOKEN_FILE:-$ROOT/.local/api-token}"
ANCHOR="$ROOT/.venv/bin/anchor"
PY="$ROOT/.venv/bin/python"
STAMP="$(date +%s)"
PROJECT_ID="validate-${STAMP}"
WORKSPACE_ID="ws-validate-${STAMP}"
GRAPH_ID="workspace-validation"
TIMEOUT="${ANCHOR_VALIDATE_TIMEOUT:-600}"
KEEP="${KEEP:-0}"

TMP="$(mktemp -d)"
cleanup() {
  if [ "$KEEP" = "1" ]; then echo "kept: $TMP"; else rm -rf "$TMP"; fi
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
step() { echo; echo "== $*"; }
ok() { echo "  PASS  $*"; }
json() { "$PY" -c "import json,sys; d=json.load(sys.stdin); print($1)"; }

[ -f "$TOKEN_FILE" ] || fail "missing token file: $TOKEN_FILE"
TOKEN="$(cat "$TOKEN_FILE")"
AUTH=(-H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json")

step "0. API readiness"
READY="$(curl -sf "${AUTH[@]}" "$API/health/ready" || true)"
[ -n "$READY" ] || fail "API is not ready at $API"
echo "$READY" | "$PY" -c "import json,sys; d=json.load(sys.stdin); assert d['status']=='ready', d" \
  || fail "readiness payload is not ready"
ok "api ready, worker connected"

step "1. throwaway repository with a bug"
REPO="$TMP/repo"
git init -q "$REPO"
git -C "$REPO" config user.email validate@example.com
git -C "$REPO" config user.name Validate
cat > "$REPO/calc.py" <<'PYFILE'
def add(a, b):
    return a - b
PYFILE
cat > "$REPO/README.md" <<'MD'
# Validate

`add(a, b)` in calc.py should return the sum.
MD
git -C "$REPO" add -A
git -C "$REPO" commit -qm init
BASE="$(git -C "$REPO" rev-parse HEAD)"
ok "base ${BASE:0:10}"

step "2. register project and fork a workspace"
curl -sf -X POST "${AUTH[@]}" -d "{\"project_id\":\"$PROJECT_ID\",\"name\":\"Validate\",\"root\":\"$REPO\"}" \
  "$API/api/projects" | json "d['project_id']" >/dev/null || fail "project registration failed"
ok "project $PROJECT_ID"

curl -sf -X POST "${AUTH[@]}" \
  -d "{\"project_id\":\"$PROJECT_ID\",\"base_revision\":\"$BASE\",\"workspace_id\":\"$WORKSPACE_ID\"}" \
  "$API/api/workspaces" | json "d['state']" | grep -qx active || fail "workspace creation failed"
ok "workspace $WORKSPACE_ID active"

step "3. install and run the graph"
cat > "$TMP/graph.json" <<JSON
{
  "graph_id": "$GRAPH_ID",
  "name": "Workspace validation",
  "nodes": [
    { "id": "coder", "type": "agent", "name": "Fix the bug", "agent_ref": "agents.coder",
      "metadata": { "workspace_id": "$WORKSPACE_ID" } }
  ],
  "edges": []
}
JSON
VERSION="$("$ANCHOR" graph install --file "$TMP/graph.json" | json "d['version']['graph_version_id']")"
[ -n "$VERSION" ] || fail "graph install failed"
TRIGGER="$("$ANCHOR" trigger add --version "$VERSION" | json "d['id']")"
[ -n "$TRIGGER" ] || fail "trigger registration failed"
RUN="$("$ANCHOR" run start --trigger "$TRIGGER" --idempotency-key "validate-$STAMP" --objective \
"Fix the bug in calc.py so that add(a, b) returns the sum. Use workspace.read to read it and workspace.write to write the corrected file. Then create test_calc.py containing assertions add(2,3)==5 and add(-1,1)==0. Run the test with workspace.exec using command ['python3','-B','-c',\"import calc; assert calc.add(2,3)==5; assert calc.add(-1,1)==0; print('tests pass')\"]. Finish with a one-sentence summary." \
  | json "d['run_id']")"
[ -n "$RUN" ] || fail "run admission failed"
ok "run $RUN"

DIGEST="$("$ANCHOR" run watch "$RUN" --timeout "$TIMEOUT" --interval 5)"
echo "$DIGEST" | json "'status='+d['status']+' terminal='+str(d['terminal'])"
echo "$DIGEST" | "$PY" -c "import json,sys; d=json.load(sys.stdin); assert d['status']=='completed', d" \
  || fail "run did not complete: $DIGEST"
ok "run completed"

step "4. verify the content chain"
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

node = next(item for item in get(f"/api/runs/{run_id}/nodes") if item["node_id"] == "coder")
output_ref = node.get("output_ref") or ""
check("node output is a workspace revision", output_ref.startswith(f"workspace://{workspace_id}@"), output_ref)
revision = output_ref.split("@", 1)[1] if "@" in output_ref else ""

workspace = get(f"/api/workspaces/{workspace_id}")
check("workspace frozen at that revision",
      workspace["state"] == "frozen" and workspace["current_revision"] == revision, workspace)

kinds = [item["kind"] for item in get(f"/api/workspaces/{workspace_id}/operations")]
check("ledger records create, write and freeze",
      kinds[:1] == ["create"] and "write" in kinds and kinds[-1] == "freeze", kinds)

events = get(f"/api/runs/{run_id}/events?after=0&limit=200")
completed = [event for event in events if event["event_type"] == "node.completed"]
check("completion event carries revision and model text",
      bool(completed) and completed[-1]["payload"].get("workspace_revision") == revision
      and str(completed[-1]["payload"].get("response_ref", "")).startswith("artifact://sha256/"),
      completed[-1]["payload"] if completed else None)

dirty = subprocess.run(["git", "-C", repo, "status", "--porcelain", "--untracked-files=no"],
                       capture_output=True, text=True, check=True).stdout.strip()
check("source working tree untouched", dirty == "", dirty)

from anchor.domain.content import workspace_ref
from anchor.runtime.sandbox import BubblewrapWorkspaceSandbox
from anchor.runtime.workspace import execute_in_workspace
from anchor.state.relational import RelationalStateStore

store = RelationalStateStore(f"sqlite:///{Path(root) / '.local' / 'api.sqlite'}")
result = execute_in_workspace(
    store, workspace_ref(workspace_id, revision),
    ["python3", "-B", "-c",
     "import calc; assert calc.add(2,3)==5; assert calc.add(-1,1)==0; print('tests pass')"],
    sandbox=BubblewrapWorkspaceSandbox())
store.close()
check("fixed code passes the assertions in the sandbox", result.ok and "tests pass" in result.stdout,
      f"rc={result.returncode} out={result.stdout!r} err={result.stderr!r}")

sys.exit(1 if failures else 0)
PYFILE

echo
echo "VALIDATION PASSED"
