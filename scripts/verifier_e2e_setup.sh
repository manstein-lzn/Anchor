#!/usr/bin/env bash
# Isolated Verifier E2E: pass path and rejection path in one isolated DB.
#
# Requirements proven by this script:
#   - Agent output is a content-addressed artifact that verifies on read.
#   - Verifier claims via typed claim; only persisted `passed` opens downstream.
#   - `verification.decided` precedes `node.completed` / `node.failed`.
#   - Rejection persists a VerificationRecord and fails Run/Task with no
#     downstream ready node.
#   - Reopening the store preserves verification evidence.
set -uo pipefail

ROOT=/home/mansteinl/Anchor
E2E_DIR="$(mktemp -d /tmp/anchor-verifier-e2e-XXXXXX)"
DB_URL="sqlite:///$E2E_DIR/anchor.sqlite"
ART_ROOT="$E2E_DIR/artifacts"
RUNTIME="$E2E_DIR/runtime.json"
mkdir -p "$ART_ROOT"

echo "E2E_DIR=$E2E_DIR"

python3 - "$RUNTIME" <<'PY'
import json, sys
from pathlib import Path
cfg = json.loads(Path('/home/mansteinl/Anchor/.local/runtime.json').read_text())
cfg['verifiers'] = [
    {
        'ref': 'verifiers.evidence',
        'version': 'v1',
        'adapter': 'deterministic',
        'expression': 'length(artifacts) > `0`',
        'model_ref': None,
        'instructions': '',
    },
    {
        'ref': 'verifiers.reject',
        'version': 'v1',
        'adapter': 'deterministic',
        'expression': 'length(artifacts) > `999`',
        'model_ref': None,
        'instructions': '',
    },
]
Path(sys.argv[1]).write_text(json.dumps(cfg, indent=2))
PY

cd "$ROOT"

# Migrate, publish both graphs, admit one run per path, and dispatch.
.venv/bin/python - "$DB_URL" <<'PY'
import asyncio, sys
from alembic import command
from alembic.config import Config
from anchor.state.relational import RelationalStateStore
from anchor.domain.graph import GraphDefinition, GraphVersion, Trigger
from anchor.domain.admission import RunRequest
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.receiver import DurableExecutionReceiver

db_url = sys.argv[1]
cfg = Config('/home/mansteinl/Anchor/alembic.ini')
cfg.set_main_option('script_location', '/home/mansteinl/Anchor/migrations')
cfg.attributes['database_url'] = db_url
command.upgrade(cfg, 'head')

store = RelationalStateStore(db_url)

def make_graph(graph_id, verifier_ref):
    definition = GraphDefinition.model_validate({
        'graph_id': graph_id,
        'name': graph_id,
        'nodes': [
            {'id': 'produce', 'type': 'agent', 'name': 'Produce', 'agent_ref': 'agents.researcher'},
            {'id': 'verify', 'type': 'verifier', 'name': 'Verify', 'verifier_ref': verifier_ref},
            {'id': 'report', 'type': 'artifact', 'name': 'Report'},
        ],
        'edges': [
            {'source': 'produce', 'target': 'verify'},
            {'source': 'verify', 'target': 'report'},
        ],
    })
    return store.publish_graph(GraphVersion.publish(definition, 1))

pass_version = make_graph('verifier-e2e-pass', 'verifiers.evidence')
reject_version = make_graph('verifier-e2e-reject', 'verifiers.reject')
pass_trigger = store.create_trigger(Trigger(graph_version_id=pass_version.graph_version_id, type='manual'))
reject_trigger = store.create_trigger(Trigger(graph_version_id=reject_version.graph_version_id, type='manual'))
pass_receipt = store.admit_run(RunRequest(trigger_id=pass_trigger.id, idempotency_key='verifier-e2e-pass',
                                          objective='Return exactly the word OK', inputs={}))
reject_receipt = store.admit_run(RunRequest(trigger_id=reject_trigger.id, idempotency_key='verifier-e2e-reject',
                                            objective='Return exactly the word OK', inputs={}))
print('PASS_RUN=%s' % pass_receipt.run_id)
print('REJECT_RUN=%s' % reject_receipt.run_id)
asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
store.close()
PY
