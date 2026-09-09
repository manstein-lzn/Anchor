#!/usr/bin/env bash
# Parallel branches: two agents edit independent forks, a join merges them.
#
# Validates that concurrent writers never share a tree, that the join's
# require_clean merge combines disjoint changes, and that the audit ledger
# records the fork and merge lineage.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
API="${ANCHOR_API_URL:-http://127.0.0.1:8090}"
TOKEN_FILE="${ANCHOR_TOKEN_FILE:-$ROOT/.local/api-token}"
ANCHOR="$ROOT/.venv/bin/anchor"
PY="$ROOT/.venv/bin/python"
STAMP="$(date +%s)"
PROJECT_ID="parallel-${STAMP}"
JOIN_WS="ws-join-${STAMP}"
A_WS="ws-a-${STAMP}"
B_WS="ws-b-${STAMP}"
GRAPH_ID="workspace-parallel"
TIMEOUT="${ANCHOR_VALIDATE_TIMEOUT:-700}"
KEEP="${KEEP:-0}"

TMP="$(mktemp -d)"
RUN=""
AUTH=()
cleanup() {
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

step "1. base repository and three workspaces (one target, two forks)"
REPO="$TMP/repo"
git init -q "$REPO"
git -C "$REPO" config user.email validate@example.com
git -C "$REPO" config user.name Validate
printf '# Parallel validation\n' > "$REPO/README.md"
git -C "$REPO" add -A
git -C "$REPO" commit -qm init
BASE="$(git -C "$REPO" rev-parse HEAD)"

curl -sf -X POST "${AUTH[@]}" -d "{\"project_id\":\"$PROJECT_ID\",\"name\":\"Parallel\",\"root\":\"$REPO\"}" \
  "$API/api/projects" >/dev/null || fail "project registration failed"
curl -sf -X POST "${AUTH[@]}" \
  -d "{\"project_id\":\"$PROJECT_ID\",\"base_revision\":\"$BASE\",\"workspace_id\":\"$JOIN_WS\"}" \
  "$API/api/workspaces" >/dev/null || fail "join workspace failed"
curl -sf -X POST "${AUTH[@]}" -d "{\"new_workspace_id\":\"$A_WS\"}" \
  "$API/api/workspaces/$JOIN_WS/fork" >/dev/null || fail "fork A failed"
curl -sf -X POST "${AUTH[@]}" -d "{\"new_workspace_id\":\"$B_WS\"}" \
  "$API/api/workspaces/$JOIN_WS/fork" >/dev/null || fail "fork B failed"
ok "workspaces $JOIN_WS + $A_WS + $B_WS"

step "2. graph: parallel -> two coders -> join(anchor.join_merge)"
cat > "$TMP/graph.json" <<JSON
{
  "graph_id": "$GRAPH_ID",
  "name": "Parallel branches",
  "nodes": [
    { "id": "fan", "type": "parallel", "name": "Fan out" },
    { "id": "coder_a", "type": "agent", "name": "Write a", "agent_ref": "agents.coder_a",
      "metadata": { "workspace_id": "$A_WS" } },
    { "id": "coder_b", "type": "agent", "name": "Write b", "agent_ref": "agents.coder_b",
      "metadata": { "workspace_id": "$B_WS" } },
    { "id": "join", "type": "join", "name": "Merge branches",
      "metadata": { "workspace_id": "$JOIN_WS", "behavior_ref": "anchor.join_merge" } }
  ],
  "edges": [
    { "source": "fan", "target": "coder_a" },
    { "source": "fan", "target": "coder_b" },
    { "source": "coder_a", "target": "join", "input_mapping": { "branch_a": "outputs.coder_a" } },
    { "source": "coder_b", "target": "join", "input_mapping": { "branch_b": "outputs.coder_b" } }
  ]
}
JSON
VERSION="$("$ANCHOR" graph install --file "$TMP/graph.json" | json "d['version']['graph_version_id']")"
[ -n "$VERSION" ] || fail "graph install failed"
TRIGGER="$("$ANCHOR" trigger add --version "$VERSION" | json "d['id']")"
RUN="$("$ANCHOR" run start --trigger "$TRIGGER" --idempotency-key "parallel-$STAMP" --objective \
"Create exactly the one file your instructions require, using workspace.write. Do not touch any other file. Finish with a one-sentence summary." \
  | json "d['run_id']")"
[ -n "$RUN" ] || fail "run admission failed"
ok "run $RUN"

DIGEST="$("$ANCHOR" run watch "$RUN" --timeout "$TIMEOUT" --interval 6)"
echo "$DIGEST" | json "'status='+d['status']+' counts='+str(d['node_status_counts'])"
echo "$DIGEST" | "$PY" -c "import json,sys; d=json.load(sys.stdin); assert d['status']=='completed', d" \
  || fail "run did not complete"
ok "run completed"

step "3. branch isolation, merge result and lineage"
"$PY" - "$API" "$TOKEN" "$JOIN_WS" "$A_WS" "$B_WS" "$REPO" "$ROOT" "$RUN" <<'PYFILE'
import json, sys, urllib.request
from pathlib import Path

api, token, join_ws, a_ws, b_ws, repo, root, run_id = sys.argv[1:9]
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
check("all four nodes completed",
      all(nodes[name]["status"] == "completed" for name in ("fan", "coder_a", "coder_b", "join")),
      {k: v["status"] for k, v in nodes.items()})

a_ref, b_ref = nodes["coder_a"].get("output_ref"), nodes["coder_b"].get("output_ref")
check("branches produced different revisions", a_ref and b_ref and a_ref != b_ref, (a_ref, b_ref))

join_ref = nodes["join"].get("output_ref") or ""
check("join output is a workspace revision", join_ref.startswith(f"workspace://{join_ws}@"), join_ref)

from anchor.runtime.workspace import GitWorkspaceBackend
from anchor.state.relational import RelationalStateStore

store = RelationalStateStore(f"sqlite:///{Path(root) / '.local' / 'api.sqlite'}")
backend = GitWorkspaceBackend(repo)
revision = join_ref.split("@", 1)[1] if "@" in join_ref else ""
paths = backend.list_paths(revision) if revision else []
check("merged revision contains both branch files",
      any(p.endswith("a.txt") for p in paths) and any(p.endswith("b.txt") for p in paths), paths)
check("base file survives the merge", "README.md" in paths, paths)

kinds = [item["kind"] for item in get(f"/api/workspaces/{join_ws}/operations")]
check("join ledger records create and merge",
      kinds[:1] == ["create"] and "merge" in kinds, kinds)
fork_kinds = [item["kind"] for item in get(f"/api/workspaces/{a_ws}/operations")]
check("forked workspace records its fork", fork_kinds[:2] == ["create", "fork"], fork_kinds)
store.close()

sys.exit(1 if failures else 0)
PYFILE

echo
echo "PARALLEL VALIDATION PASSED"
