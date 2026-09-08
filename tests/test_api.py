from concurrent.futures import ThreadPoolExecutor
from uuid import UUID, uuid4

import pytest
import hashlib
import hmac

pytest.importorskip("fastapi")
pytest.importorskip("httpx")
from fastapi.testclient import TestClient

from anchor.api.app import create_app
from anchor.domain.context import input_hash
from anchor.domain.graph import GraphDefinition, GraphNode, GraphVersion, Trigger
from anchor.domain.models import VerificationRecord
from test_relational_store import database


TOKEN = "anchor-contract-test-token-not-a-real-secret"
HEADERS = {"Authorization": f"Bearer {TOKEN}"}


@pytest.fixture
def client(database):
    store, _ = database
    with TestClient(create_app(store, TOKEN), headers=HEADERS) as current:
        yield current, store


def definition():
    return {"graph_id": "review", "name": "Review",
            "nodes": [{"id": "research", "name": "Research", "type": "agent", "agent_ref": "research-v1"}],
            "edges": []}


def publish(client):
    result = client.put("/api/graphs/review/draft", json={"expected_revision": 0, "definition": definition()})
    assert result.status_code == 200, result.text
    result = client.post("/api/graphs/review/publish", json={"expected_revision": 1})
    assert result.status_code == 200, result.text
    return result.json()


def register(client, version):
    trigger_id = str(uuid4())
    result = client.put(f"/api/triggers/{trigger_id}", json={"graph_version_id": version["graph_version_id"]})
    assert result.status_code == 200, result.text
    return trigger_id


def test_authentication_and_host_boundary(client):
    api, store = client
    live = api.get("/health/live", headers={"Authorization": ""})
    assert live.status_code == 200
    assert live.headers["X-Anchor-Server-Time"].endswith("+00:00")
    assert api.get("/api/graphs", headers={"Authorization": ""}).status_code == 401
    assert api.get("/api/runs", headers={"Authorization": "Bearer wrong"}).status_code == 401
    assert api.post("/api/graphs/validate", json=definition(), headers={"Authorization": ""}).status_code == 401
    assert api.get("/health/live", headers={"Host": "untrusted.example"}).status_code == 400
    assert api.get("/health/ready").json()["execution_connected"] is False
    assert api.get("/health/ready").json()["worker_connected"] is False
    assert api.get("/health/ready").json()["verifier_worker_connected"] is False
    store.record_runtime_heartbeat("execution_receiver", uuid4())
    assert api.get("/health/ready").json()["execution_connected"] is True


def test_active_lease_endpoint_returns_serializable_list(client):
    api, _ = client
    response = api.get('/api/leases/active')
    assert response.status_code == 200
    assert isinstance(response.json(), list)


def test_graph_run_filter_and_stop_are_scoped_and_idempotent(client):
    api, store = client
    version = publish(api)
    trigger_id = register(api, version)
    receipt = api.post(f'/api/triggers/{trigger_id}/runs', json={'objective': 'Scoped execution'},
                      headers={'Idempotency-Key': 'graph-stop-test'}).json()
    run_id = receipt['run_id']
    assert [run['id'] for run in api.get('/api/runs?graph_id=review').json()] == [run_id]
    assert api.get('/api/runs?graph_id=another-graph').json() == []
    assert api.post(f'/api/runs/{run_id}/stop', json={'reason': 'stop'}).json()['status'] == 'cancelled'
    assert api.post(f'/api/runs/{run_id}/stop', json={'reason': 'retry'}).json()['status'] == 'cancelled'


def test_pause_and_resume_fence_new_claims_without_touching_artifacts(client):
    api, store = client
    version = publish(api)
    trigger_id = register(api, version)
    receipt = api.post(f'/api/triggers/{trigger_id}/runs', json={'objective': 'Pausable'},
                      headers={'Idempotency-Key': 'pause-resume-1'}).json()
    run_id = receipt['run_id']
    from anchor.runtime.receiver import DurableExecutionReceiver
    import asyncio
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    lease = store.claim_ready_node('worker-a', uuid4())
    assert lease is not None

    paused = api.post(f'/api/runs/{run_id}/pause', json={'reason': 'operator pause', 'actor': 'tester'}).json()
    assert paused['status'] == 'paused'
    # No worker can pick up ready work while paused.
    assert store.claim_ready_node('worker-b', uuid4()) is None
    # Pausing is idempotent and cannot run twice.
    assert api.post(f'/api/runs/{run_id}/pause', json={'reason': 'again'}).json()['status'] == 'paused'

    resumed = api.post(f'/api/runs/{run_id}/resume', json={'reason': 'continue', 'actor': 'tester'}).json()
    assert resumed['status'] == 'running'
    # In-flight lease is untouched; the run accepts new work again.
    assert store.list_active_leases()[0].claim_id == lease.claim_id
    assert api.post(f'/api/runs/{run_id}/resume', json={'reason': 'again'}).status_code == 409
    events = [event['event_type'] for event in store.list_events(UUID(run_id))]
    assert 'run.paused' in events and 'run.resumed' in events


def test_active_lease_endpoint_run_filter_is_narrow(client):
    api, _ = client
    response = api.get(f'/api/leases/active?run_id={uuid4()}')
    assert response.status_code == 200
    assert response.json() == []


def test_capability_endpoint_never_returns_secrets(client, monkeypatch, tmp_path):
    api, _ = client
    config = tmp_path / "runtime.json"
    config.write_text('{"secret_file":"/private/auth.json","models":[{"ref":"m","provider":"rightcode","model":"gpt-6-astra","secret_ref":"OPENAI_API_KEY"}],"agents":[{"ref":"a","model_ref":"m","max_tool_calls":12,"max_parallel_tools":2}],"tools":[],"verifiers":[{"ref":"v","version":"v2","adapter":"model","model_ref":"m"}]}', encoding="utf-8")
    monkeypatch.setenv("ANCHOR_RUNTIME_CONFIG", str(config))
    response = api.get("/api/runtime/capabilities")
    assert response.status_code == 200
    payload = response.json()
    assert payload["agents"][0]["ref"] == "a"
    assert payload["agents"][0]["max_tool_calls"] == 12
    assert payload["agents"][0]["max_parallel_tools"] == 2
    assert payload["verifiers"] == [{"ref": "v", "version": "v2", "adapter": "model", "model_ref": "m"}]
    assert "secret_ref" not in response.text and "private/auth" not in response.text


def test_graph_capability_validation_reports_missing_refs(client, monkeypatch, tmp_path):
    api, _ = client
    config = tmp_path / "runtime.json"
    config.write_text('{"models":[],"agents":[],"tools":[]}', encoding="utf-8")
    monkeypatch.setenv("ANCHOR_RUNTIME_CONFIG", str(config))
    report = api.post("/api/graphs/capabilities/validate", json={"graph_id":"g","name":"G","nodes":[{"id":"a","name":"A","type":"agent","agent_ref":"missing"}]})
    assert report.status_code == 200
    assert report.json()["valid"] is False and report.json()["issues"][0]["code"] == "missing_agent_capability"
    verifier = api.post("/api/graphs/capabilities/validate", json={
        "graph_id": "v", "name": "V",
        "nodes": [{"id": "v", "name": "Verify", "type": "verifier", "verifier_ref": "missing"}],
    })
    assert verifier.status_code == 200
    assert verifier.json()["issues"][0]["code"] == "missing_verifier_capability"


def test_incomplete_draft_can_be_saved_but_not_published(client):
    api, _ = client
    draft = api.put("/api/graphs/review/draft", json={"expected_revision": 0, "definition": {"nodes": []},
                                                    "layout": {"canvas": {"x": 20, "y": 10}}})
    assert draft.status_code == 200
    assert draft.json()["revision"] == 1
    assert api.get("/api/graphs/review/draft").json()["layout"]["canvas"]["x"] == 20
    assert api.post("/api/graphs/review/publish", json={"expected_revision": 1}).status_code == 422
    assert api.get("/api/graphs/review/versions").json() == []


def test_draft_revision_conflict_prevents_overwriting_other_editor(client):
    api, _ = client
    publish(api)
    stale = api.put("/api/graphs/review/draft", json={"expected_revision": 0, "definition": {}})
    assert stale.status_code == 409
    assert api.get("/api/graphs/review/draft").json()["definition"] == definition()


def test_publication_is_idempotent_and_new_versions_do_not_replace_old(client):
    api, _ = client
    first = publish(api)
    assert api.post("/api/graphs/review/publish", json={"expected_revision": 1}).json() == first
    changed = definition()
    changed["nodes"][0]["agent_ref"] = "research-v2"
    assert api.put("/api/graphs/review/draft", json={"expected_revision": 1, "definition": changed}).status_code == 200
    second = api.post("/api/graphs/review/publish", json={"expected_revision": 2}).json()
    assert (first["version"], second["version"]) == (1, 2)
    assert first["graph_version_id"] != second["graph_version_id"]
    assert api.get(f"/api/graph-versions/{first['graph_version_id']}").json() == first
    assert api.post("/api/graphs/review/publish", json={"expected_revision": 1}).json() == first


def test_invalid_topology_and_mismatched_graph_identity(client):
    api, _ = client
    invalid = definition()
    invalid["edges"] = [{"source": "research", "target": "missing"}]
    report = api.post("/api/graphs/validate", json=invalid)
    assert report.status_code == 200
    assert report.json()["valid"] is False
    api.put("/api/graphs/review/draft", json={"expected_revision": 0, "definition": invalid})
    assert api.post("/api/graphs/review/publish", json={"expected_revision": 1}).status_code == 422
    valid = definition()
    valid["graph_id"] = "other"
    api.put("/api/graphs/review/draft", json={"expected_revision": 1, "definition": valid})
    assert api.post("/api/graphs/review/publish", json={"expected_revision": 2}).status_code == 422


def test_start_receipt_run_nodes_and_cursor_events(client):
    api, store = client
    version = publish(api)
    trigger_id = register(api, version)
    url = f"/api/triggers/{trigger_id}/runs"
    body = {"objective": "Review report", "inputs": {"artifact_ref": "report-1"}}
    headers = {"Idempotency-Key": "click-1"}
    first = api.post(url, json=body, headers=headers)
    assert first.status_code == 202
    assert api.post(url, json=body, headers=headers).json() == first.json()
    assert api.post(url, json={"objective": "different"}, headers=headers).status_code == 409
    receipt = first.json()
    run = api.get(f"/api/runs/{receipt['run_id']}").json()
    assert run["status"] == "created"
    assert run["graph_version_id"] == version["graph_version_id"]
    assert api.get(f"/api/tasks/{receipt['task_id']}").json()["objective"] == body["objective"]
    assert api.get(f"/api/runs/{receipt['run_id']}/nodes").json()[0]["status"] == "pending"
    assert api.get(f"/api/runs/{receipt['run_id']}/decisions").json() == []
    assert api.get(f"/api/runs/{uuid4()}/decisions").status_code == 404
    assert api.get(f"/api/runs/{receipt['run_id']}/operations").json() == []
    assert api.get(f"/api/runs/{uuid4()}/operations").status_code == 404
    events = api.get(f"/api/runs/{receipt['run_id']}/events").json()
    assert events[0]["event_type"] == "run.requested"
    assert api.get(f"/api/runs/{receipt['run_id']}/events?after=1").json() == []
    assert len(store.pending_dispatches()) == 1


def test_context_snapshot_endpoint_returns_persisted_generation(client):
    api, store = client
    version = publish(api)
    trigger_id = register(api, version)
    receipt = api.post(f"/api/triggers/{trigger_id}/runs", json={"objective": "snapshot"},
                       headers={"Idempotency-Key": "snapshot-1"}).json()
    from anchor.runtime.receiver import DurableExecutionReceiver
    import asyncio
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    lease = store.claim_ready_node("api-snapshot-worker", uuid4())
    store.complete_node_and_propagate(lease.claim_id, "api-snapshot-worker",
                                      output_ref="artifact://sha256/snapshot",
                                      input_snapshot={"inputs": {"source": "api"}})
    response = api.get(f"/api/runs/{receipt['run_id']}/contexts")
    assert response.status_code == 200
    body = response.json()
    assert len(body) == 1 and body[0]["generation"] == 1
    assert body[0]["snapshot"] == {"inputs": {"source": "api"}}


def test_verification_endpoint_returns_persisted_evidence(client):
    api, store = client
    graph_id = "verification-api"
    definition = {
        "graph_id": graph_id,
        "name": "Verification API",
        "nodes": [{
            "id": "verify", "name": "Verify", "type": "verifier",
            "verifier_ref": "verifiers.api",
        }],
    }
    assert api.put(f"/api/graphs/{graph_id}/draft", json={
        "expected_revision": 0, "definition": definition,
    }).status_code == 200
    version = api.post(f"/api/graphs/{graph_id}/publish", json={"expected_revision": 1}).json()
    trigger_id = register(api, version)
    receipt = api.post(
        f"/api/triggers/{trigger_id}/runs",
        json={"objective": "verify API evidence", "inputs": {"approved": True}},
        headers={"Idempotency-Key": "verification-api-1"},
    ).json()
    from anchor.runtime.receiver import DurableExecutionReceiver
    import asyncio
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    lease = store.claim_ready_verifier_node("api-verifier", uuid4())
    snapshot = {"inputs": {"approved": True}}
    evidence_ref = "artifact://sha256/" + "c" * 64
    record = VerificationRecord(
        claim_id=lease.claim_id,
        run_id=lease.run_id,
        node_run_id=lease.node_run_id,
        node_id=lease.node_id,
        verifier_ref="verifiers.api",
        verifier_version="v1",
        adapter="deterministic_test",
        adapter_version="v1",
        verdict="passed",
        reason="API evidence passed",
        evidence_ref=evidence_ref,
        verified_context_hash=input_hash(snapshot),
    )
    store.complete_node_and_propagate(
        lease.claim_id,
        "api-verifier",
        output_ref=evidence_ref,
        input_snapshot=snapshot,
        verification=record,
    )
    response = api.get(f"/api/runs/{receipt['run_id']}/verifications")
    assert response.status_code == 200
    assert response.json()[0]["verification_id"] == str(record.verification_id)
    assert response.json()[0]["verified_context_hash"] == input_hash(snapshot)
    assert api.get(f"/api/runs/{uuid4()}/verifications").status_code == 404


def test_internal_event_trigger_ingress_uses_event_and_idempotency_contract(client):
    api, _ = client
    version = publish(api)
    trigger_id = str(uuid4())
    created = api.put(f"/api/triggers/{trigger_id}", json={
        "graph_version_id": version["graph_version_id"], "type": "internal_event",
        "event_type": "research.completed", "idempotency_field": "event_id",
    })
    assert created.status_code == 200, created.text
    body = {"objective": "Process event", "inputs": {"event_id": "evt-1", "value": 3}}
    headers = {"X-Anchor-Event-Type": "research.completed"}
    first = api.post(f"/api/triggers/{trigger_id}/events", json=body, headers=headers)
    assert first.status_code == 202, first.text
    replay = api.post(f"/api/triggers/{trigger_id}/events", json=body, headers=headers)
    assert replay.status_code == 202 and replay.json() == first.json()
    wrong = api.post(f"/api/triggers/{trigger_id}/events", json=body,
                     headers={"X-Anchor-Event-Type": "other"})
    assert wrong.status_code == 409


def test_webhook_trigger_requires_valid_hmac(client, monkeypatch, tmp_path):
    api, _ = client
    config = tmp_path / "runtime.json"
    config.write_text('{"secret_file":null,"models":[],"agents":[],"tools":[]}', encoding="utf-8")
    monkeypatch.setenv("ANCHOR_RUNTIME_CONFIG", str(config))
    monkeypatch.setenv("ANCHOR_SECRET_WEBHOOK", "test-webhook-secret")
    version = publish(api); trigger_id = str(uuid4())
    created = api.put(f"/api/triggers/{trigger_id}", json={"graph_version_id": version["graph_version_id"], "type":"webhook", "event_type":"hook.received", "webhook_secret_ref":"WEBHOOK"})
    assert created.status_code == 200
    body = {"objective":"Handle hook","inputs":{"event_id":"h-1"}}
    base = {"X-Anchor-Event-Type":"hook.received"}
    assert api.post(f"/api/triggers/{trigger_id}/events", json=body, headers=base).status_code == 401
    raw = '{"objective":"Handle hook","constraints":[],"success_criteria":[],"inputs":{"event_id":"h-1"}}'
    signature = hmac.new(b"test-webhook-secret", raw.encode(), hashlib.sha256).hexdigest()
    ok = api.post(f"/api/triggers/{trigger_id}/events", json=body, headers={**base, "X-Anchor-Signature":f"sha256={signature}", "Idempotency-Key":"hook-1"})
    assert ok.status_code == 202, ok.text


def test_memory_api_lists_and_tombstones_records(client, monkeypatch, tmp_path):
    api, _ = client
    monkeypatch.setenv("ANCHOR_MEMORY_PATH", str(tmp_path / "memory.jsonl"))
    from anchor.runtime.memory import LocalMemoryStore, MemoryRecord
    run_id = uuid4(); record = LocalMemoryStore(tmp_path / "memory.jsonl").put(MemoryRecord.create("audit fact", run_id=run_id))
    assert api.get(f"/api/memory?run_id={run_id}").json()[0]["content"] == "audit fact"
    assert api.delete(f"/api/memory/{record.memory_id}").status_code == 200
    assert api.get(f"/api/memory?run_id={run_id}").json() == []
    assert len(api.get(f"/api/memory?run_id={run_id}&include_deleted=true").json()) == 1
    purge = api.post("/api/memory/purge")
    assert purge.status_code == 200 and purge.json()["purged"] == 1
    assert api.get(f"/api/memory?run_id={run_id}&include_deleted=true").json() == []


def test_artifact_api_reads_only_verified_content(client, monkeypatch, tmp_path):
    api, _ = client
    monkeypatch.setenv("ANCHOR_ARTIFACT_ROOT", str(tmp_path / "artifacts"))
    from anchor.runtime.artifacts import LocalArtifactStore
    ref = LocalArtifactStore(tmp_path / "artifacts").put_text("artifact body")
    digest = ref.rsplit("/", 1)[-1]
    response = api.get(f"/api/artifacts/{digest}")
    assert response.status_code == 200 and response.json()["content"] == "artifact body"
    assert api.get("/api/artifacts/not-a-digest").status_code == 404


def test_lease_recovery_api_requires_reason(client):
    api, store = client
    version = publish(api); trigger_id = register(api, version)
    receipt = api.post(f"/api/triggers/{trigger_id}/runs", json={"objective":"recover"}, headers={"Idempotency-Key":"recover-1"}).json()
    from anchor.runtime.receiver import DurableExecutionReceiver
    import asyncio
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    lease = store.claim_ready_node("api-test-worker", uuid4())
    response = api.post(f"/api/leases/{lease.claim_id}/recover", json={"reason":"operator confirmed worker interruption"})
    assert response.status_code == 200 and response.json()["status"] == "ready"


def test_control_lease_recovery_api_is_explicit_and_idempotent(client):
    api, store = client
    version = store.publish_graph(GraphVersion.publish(GraphDefinition(
        graph_id="control-recovery-api", name="Control recovery API",
        nodes=[GraphNode(id="route", type="router", name="Route")],
    ), 1))
    trigger = store.create_trigger(Trigger(
        graph_version_id=version.graph_version_id, type="manual",
    ))
    response = api.post(
        f"/api/triggers/{trigger.id}/runs",
        json={"objective": "recover control"},
        headers={"Idempotency-Key": "recover-control"},
    )
    from anchor.runtime.receiver import DurableExecutionReceiver
    import asyncio
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    lease = store.claim_ready_control_node("control-worker", uuid4())
    body = {"reason": "operator confirmed control worker interruption"}
    first = api.post(f"/api/leases/{lease.claim_id}/recover", json=body)
    second = api.post(f"/api/leases/{lease.claim_id}/recover", json=body)
    assert first.status_code == 200 and first.json()["status"] == "ready"
    assert second.status_code == 200 and second.json() == first.json()


def test_agent_lease_can_be_explicitly_failed(client):
    api, store = client
    version = publish(api); trigger_id = register(api, version)
    receipt = api.post(
        f"/api/triggers/{trigger_id}/runs", json={"objective": "fail"},
        headers={"Idempotency-Key": "fail-1"}).json()
    from anchor.runtime.receiver import DurableExecutionReceiver
    import asyncio
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    lease = store.claim_ready_node("api-test-worker", uuid4())
    response = api.post(
        f"/api/leases/{lease.claim_id}/fail",
        json={"error_code": "operator_confirmed_failure", "phase": "model"})
    assert response.status_code == 200
    assert response.json()["status"] == "failed"
    assert store.get_run(receipt["run_id"]).status.value == "failed"
    assert store.get_task(receipt["task_id"]).status.value == "failed"
    assert api.post(f"/api/leases/{lease.claim_id}/fail", json={"error_code": ""}).status_code == 422


def test_tool_lease_cannot_bypass_operation_reconciliation(client):
    api, store = client
    tool_definition = {
        "graph_id": "tool-failure-boundary", "name": "Tool failure boundary",
        "nodes": [{"id": "side_effect", "name": "Side effect", "type": "tool", "tool_ref": "tools.write"}],
        "edges": [],
    }
    saved = api.put(
        "/api/graphs/tool-failure-boundary/draft",
        json={"expected_revision": 0, "definition": tool_definition})
    assert saved.status_code == 200
    version = api.post(
        "/api/graphs/tool-failure-boundary/publish",
        json={"expected_revision": 1}).json()
    trigger_id = register(api, version)
    api.post(
        f"/api/triggers/{trigger_id}/runs", json={"objective": "write"},
        headers={"Idempotency-Key": "tool-fail-1"})
    from anchor.runtime.receiver import DurableExecutionReceiver
    import asyncio
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    lease = store.claim_ready_node("tool-worker", uuid4())
    response = api.post(
        f"/api/leases/{lease.claim_id}/fail",
        json={"error_code": "operator_confirmed_failure"})
    assert response.status_code == 409
    assert store.list_node_runs(lease.run_id)[0].status.value == "running"


def test_trigger_registration_retry_and_disable(client):
    api, _ = client
    version = publish(api)
    trigger_id = register(api, version)
    body = {"graph_version_id": version["graph_version_id"]}
    assert api.put(f"/api/triggers/{trigger_id}", json=body).status_code == 200
    assert len(api.get(f"/api/graph-versions/{version['graph_version_id']}/triggers").json()) == 1
    assert api.put(f"/api/triggers/{trigger_id}", json={**body, "enabled": False}).status_code == 409
    assert api.patch(f"/api/triggers/{trigger_id}", json={"enabled": False}).status_code == 200
    result = api.post(f"/api/triggers/{trigger_id}/runs", json={"objective": "test"}, headers={"Idempotency-Key": "new"})
    assert result.status_code == 422
    assert api.get("/api/runs").json() == []


def test_manual_ingress_cannot_bypass_schedule_rules(client):
    api, store = client
    version = publish(api)
    trigger = store.create_trigger(Trigger(graph_version_id=version["graph_version_id"], type="cron", cron="0 8 * * *"))
    response = api.post(f"/api/triggers/{trigger.id}/runs", json={"objective": "bypass"}, headers={"Idempotency-Key": "key"})
    assert response.status_code == 409


def test_missing_resources_pagination_and_key_validation(client):
    api, _ = client
    assert api.get(f"/api/runs/{uuid4()}").status_code == 404
    assert api.get(f"/api/runs/{uuid4()}/events").status_code == 404
    assert api.get("/api/graphs?limit=0").status_code == 422
    assert api.get("/api/runs?offset=-1").status_code == 422
    trigger_id = register(api, publish(api))
    assert api.post(f"/api/triggers/{trigger_id}/runs", json={"objective": "test"}).status_code == 422


def test_validation_errors_do_not_echo_payload_or_tokens(client):
    api, _ = client
    sentinel = "sensitive-content-do-not-echo"
    response = api.put("/api/graphs/review/draft", json={"expected_revision": {"secret": sentinel}, "definition": {}})
    assert response.status_code == 422
    assert sentinel not in response.text
    assert TOKEN not in response.text


def test_openapi_advertises_bearer_and_no_arbitrary_execution(client):
    api, _ = client
    spec = api.get("/openapi.json").json()
    assert spec["paths"]["/api/graphs"]["get"]["security"] == [{"HTTPBearer": []}]
    # Operator controls are explicit and authenticated; no endpoint executes a
    # model or tool on demand outside the durable claim/lease path.
    for path in ("/api/runs/{run_id}/stop", "/api/runs/{run_id}/pause", "/api/runs/{run_id}/resume"):
        assert spec["paths"][path]["post"]["security"] == [{"HTTPBearer": []}]
    assert not any("execute" in path for path in spec["paths"])


def test_concurrent_publication_returns_one_version(client):
    api, store = client
    api.put("/api/graphs/review/draft", json={"expected_revision": 0, "definition": definition()})
    with ThreadPoolExecutor(max_workers=2) as pool:
        results = list(pool.map(lambda _: api.post("/api/graphs/review/publish", json={"expected_revision": 1}), range(2)))
    assert [result.status_code for result in results] == [200, 200]
    assert results[0].json() == results[1].json()
    assert len(store.list_graph_versions("review")) == 1


def test_api_restart_preserves_receipt(database):
    store, _ = database
    with TestClient(create_app(store, TOKEN), headers=HEADERS) as api:
        trigger_id = register(api, publish(api))
        url = f"/api/triggers/{trigger_id}/runs"
        first = api.post(url, json={"objective": "recover"}, headers={"Idempotency-Key": "one"}).json()
    with TestClient(create_app(store, TOKEN), headers=HEADERS) as api:
        second = api.post(url, json={"objective": "recover"}, headers={"Idempotency-Key": "one"}).json()
        assert first == second


def test_missing_secret_fails_closed(database, monkeypatch):
    store, _ = database
    monkeypatch.delenv("ANCHOR_API_TOKEN", raising=False)
    with pytest.raises(RuntimeError, match="ANCHOR_API_TOKEN"):
        create_app(store)


def test_database_error_does_not_leak_connection_details(client, monkeypatch):
    from sqlalchemy.exc import OperationalError
    api, store = client
    def fail(*args, **kwargs):
        raise OperationalError("SELECT sensitive_data", {}, RuntimeError("password=do-not-echo"))
    monkeypatch.setattr(store, "list_drafts", fail)
    response = api.get("/api/graphs")
    assert response.status_code == 503
    assert response.json() == {"error": {"code": "storage_unavailable"}}
    assert "do-not-echo" not in response.text


def test_readiness_reports_schema_mismatch_without_details(client, monkeypatch):
    api, store = client
    def fail():
        raise RuntimeError("internal schema details")
    monkeypatch.setattr(store, "check_schema", fail)
    response = api.get("/health/ready")
    assert response.status_code == 503
    assert "internal schema details" not in response.text


def test_waits_api_lists_and_approves_to_terminal(client, monkeypatch, tmp_path):
    api, store = client
    monkeypatch.setenv("ANCHOR_ARTIFACT_ROOT", str(tmp_path / "artifacts"))
    version = store.publish_graph(GraphVersion.publish(GraphDefinition(
        graph_id="approval-api", name="Approval API",
        nodes=[GraphNode(id="a", type="agent", name="A", agent_ref="a"),
               GraphNode(id="g", type="approval", name="G"),
               GraphNode(id="b", type="agent", name="B", agent_ref="b")],
        edges=[{"source": "a", "target": "g"}, {"source": "g", "target": "b"}],
    ), 1))
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id, type="manual"))
    receipt = api.post(f"/api/triggers/{trigger.id}/runs", json={"objective": "approve"},
                       headers={"Idempotency-Key": "approve-1"}).json()
    from anchor.runtime.receiver import DurableExecutionReceiver
    import asyncio
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    lease = store.claim_ready_node("worker", uuid4())
    store.complete_node_and_propagate(lease.claim_id, "worker",
                                      output_ref="artifact://sha256/a",
                                      input_snapshot={"inputs": {}})
    waiting = api.get("/api/waits").json()
    assert len(waiting) == 1
    assert waiting[0]["node_run"]["node_id"] == "g"
    assert waiting[0]["node_type"] == "approval"
    node_run_id = waiting[0]["node_run"]["id"]
    decided = api.post(f"/api/waits/{node_run_id}/approve",
                       json={"reason": "ship it", "actor": "owner"})
    assert decided.status_code == 200, decided.text
    assert decided.json()["status"] == "completed"
    assert api.get("/api/waits").json() == []
    assert api.get(f"/api/runs/{receipt['run_id']}").json()["status"] == "running"
    nxt = store.claim_ready_node("worker", uuid4())
    store.complete_node_and_propagate(nxt.claim_id, "worker",
                                      output_ref="artifact://sha256/b",
                                      input_snapshot={"inputs": {}})
    assert api.get(f"/api/runs/{receipt['run_id']}").json()["status"] == "completed"


def test_waits_api_reject_and_event_resume(client, monkeypatch, tmp_path):
    api, store = client
    monkeypatch.setenv("ANCHOR_ARTIFACT_ROOT", str(tmp_path / "artifacts"))
    version = store.publish_graph(GraphVersion.publish(GraphDefinition(
        graph_id="reject-api", name="Reject API",
        nodes=[GraphNode(id="g", type="approval", name="G")],
    ), 1))
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id, type="manual"))
    api.post(f"/api/triggers/{trigger.id}/runs", json={"objective": "reject"},
             headers={"Idempotency-Key": "reject-1"})
    from anchor.runtime.receiver import DurableExecutionReceiver
    import asyncio
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    gate = api.get("/api/waits").json()[0]["node_run"]["id"]
    rejected = api.post(f"/api/waits/{gate}/reject",
                        json={"reason": "not now", "actor": "owner"})
    assert rejected.status_code == 200
    assert rejected.json()["status"] == "failed"

    event_version = store.publish_graph(GraphVersion.publish(GraphDefinition(
        graph_id="event-api", name="Event API",
        nodes=[GraphNode(id="w", type="wait_for_event", name="W",
                         metadata={"wait_event": "deploy.done"})],
    ), 1))
    event_trigger = store.create_trigger(Trigger(graph_version_id=event_version.graph_version_id,
                                                 type="manual"))
    api.post(f"/api/triggers/{event_trigger.id}/runs", json={"objective": "wait"},
             headers={"Idempotency-Key": "wait-1"})
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[1]))
    waiter = [item for item in api.get("/api/waits").json()
              if item["node_type"] == "wait_for_event"][0]["node_run"]["id"]
    bad = api.post(f"/api/waits/{waiter}/resume",
                   json={"event_type": "other", "payload": {}})
    assert bad.status_code == 409
    good = api.post(f"/api/waits/{waiter}/resume",
                    json={"event_type": "deploy.done", "payload": {"n": 1}})
    assert good.status_code == 200, good.text
    assert good.json()["status"] == "completed"


def test_bundle_export_import_round_trip_with_triggers(client):
    api, store = client
    version = publish(api)
    trigger_id = register(api, version)
    bundle = api.get(f"/api/graph-versions/{version['graph_version_id']}/bundle").json()
    assert bundle["content_hash"] == version["content_hash"]
    assert bundle["bundle_version"] == 1
    assert bundle["required_agents"] == ["research-v1"]
    assert len(bundle["triggers"]) == 1
    imported = api.post("/api/bundles/import", json={"bundle": bundle, "expected_revision": 1,
                                                     "publish": True, "import_triggers": True}).json()
    assert imported["version"]["content_hash"] == version["content_hash"]
    assert len(imported["triggers"]) == 1
    assert imported["triggers"][0]["id"] != trigger_id
    assert imported["triggers"][0]["graph_version_id"] == imported["version"]["graph_version_id"]


def test_bundle_tamper_fails_closed(client):
    api, _ = client
    version = publish(api)
    bundle = api.get(f"/api/graph-versions/{version['graph_version_id']}/bundle").json()
    bundle["graph"]["name"] = "Tampered"
    result = api.post("/api/bundles/import", json={"bundle": bundle, "expected_revision": 0})
    assert result.status_code == 422


def test_memory_propose_review_and_filters(client, monkeypatch, tmp_path):
    api, _ = client
    monkeypatch.setenv("ANCHOR_MEMORY_PATH", str(tmp_path / "memory.jsonl"))
    proposed = api.post("/api/memory/propose", json={"content": "verify hashes",
                                                     "domain": "evidence"}).json()
    assert proposed["status"] == "proposed"
    assert api.get("/api/memory?status=promoted").json() == []
    reviewed = api.post(f"/api/memory/{proposed['memory_id']}/review",
                        json={"status": "promoted", "reviewer": "owner",
                              "reason": "confirmed"}).json()
    assert reviewed["status"] == "promoted"
    assert reviewed["reviewed_by"] == "owner"
    assert len(api.get("/api/memory?status=promoted").json()) == 1
    assert len(api.get("/api/memory?status=promoted&domain=evidence").json()) == 1
    assert api.get("/api/memory?status=promoted&domain=other").json() == []
    bad = api.post(f"/api/memory/{proposed['memory_id']}/review",
                   json={"status": "promoted", "reviewer": "owner", "reason": "again"})
    assert bad.status_code == 422


def test_unknown_operation_reconciliation_completes_or_fails_the_node(client, monkeypatch, tmp_path):
    from anchor.domain.graph import GraphDefinition, GraphNode, GraphVersion, Trigger
    from anchor.domain.operations import OperationStatus, ToolOperation
    from anchor.runtime.artifacts import LocalArtifactStore
    from anchor.runtime.receiver import DurableExecutionReceiver
    import asyncio

    api, store = client
    monkeypatch.setenv("ANCHOR_ARTIFACT_ROOT", str(tmp_path / "artifacts"))
    artifacts = LocalArtifactStore(str(tmp_path / "artifacts"))

    def unknown_operation(graph_id, key):
        version = store.publish_graph(GraphVersion.publish(GraphDefinition(
            graph_id=graph_id, name=graph_id,
            nodes=[GraphNode(id="notify", type="tool", name="Notify", tool_ref="http.post",
                             metadata={"owner_agent": "agents.writer"})]), 1))
        trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id,
                                               type="manual"))
        receipt = api.post(f"/api/triggers/{trigger.id}/runs", json={"objective": "notify"},
                           headers={"Idempotency-Key": key}).json()
        asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[-1]))
        lease = store.claim_ready_node("tool-worker", uuid4())
        operation_id = uuid4()
        store.register_tool_operation(ToolOperation.register(
            operation_id=operation_id, claim_id=lease.claim_id, node_run_id=lease.node_run_id,
            run_id=lease.run_id, tool_ref="http.post", arguments={"url": "https://example.test"}))
        store.start_tool_operation(operation_id, lease.claim_id)
        store.finish_tool_operation(operation_id, lease.claim_id,
                                    status=OperationStatus.OUTCOME_UNKNOWN,
                                    error_code="transport_unknown")
        return receipt, lease, operation_id

    # Success: reconciliation completes the node and opens the terminal run.
    receipt, lease, operation_id = unknown_operation("reconcile-success", "reconcile-1")
    result_ref = artifacts.put_text('{"delivered": true}', media_type="application/json")
    response = api.post(f"/api/operations/{operation_id}/reconcile", json={
        "status": "succeeded", "reconciliation_ref": "provider://receipt/1",
        "result_ref": result_ref, "reason": "provider confirms delivery", "actor": "tester"})
    assert response.status_code == 200, response.text
    assert response.json()["operation"]["status"] == "succeeded"
    assert response.json()["node_run"]["status"] == "completed"
    assert api.get(f"/api/runs/{receipt['run_id']}").json()["status"] == "completed"
    # Unknown outcomes are never retried automatically: the ledger keeps the
    # reconciliation evidence and no second attempt row was created.
    assert len([n for n in store.list_node_runs(receipt["run_id"]) if n.node_id == "notify"]) == 1

    # Failure: reconciliation fails the node and the run, with no downstream work.
    receipt, lease, operation_id = unknown_operation("reconcile-failure", "reconcile-2")
    response = api.post(f"/api/operations/{operation_id}/reconcile", json={
        "status": "failed", "reconciliation_ref": "provider://receipt/2",
        "error_code": "reconciled_not_delivered", "reason": "provider confirms failure",
        "actor": "tester"})
    assert response.status_code == 200, response.text
    assert response.json()["node_run"]["status"] == "failed"
    assert api.get(f"/api/runs/{receipt['run_id']}").json()["status"] == "failed"
