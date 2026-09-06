"""Exercise the live local API without exposing its bearer token."""

import argparse
import json
import os
from pathlib import Path
from uuid import uuid4

import httpx


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--url", default="http://127.0.0.1:8090")
    parser.add_argument("--token-file", type=Path, default=Path(".local/api-token"))
    args = parser.parse_args()
    token = os.environ.get("ANCHOR_API_TOKEN") or args.token_file.read_text().strip()
    graph_id = f"smoke-{uuid4().hex[:12]}"
    definition = {"graph_id": graph_id, "name": "API Contract Smoke Test",
                  "nodes": [{"id": "agent", "name": "Agent", "type": "agent", "agent_ref": "test-only-v1"}]}
    with httpx.Client(base_url=args.url, headers={"Authorization": f"Bearer {token}"},
                      timeout=10, trust_env=False) as client:
        def call(method, path, **kwargs):
            result = client.request(method, path, **kwargs)
            result.raise_for_status()
            return result.json()
        call("GET", "/health/ready")
        call("PUT", f"/api/graphs/{graph_id}/draft", json={"expected_revision": 0, "definition": definition})
        version = call("POST", f"/api/graphs/{graph_id}/publish", json={"expected_revision": 1})
        trigger_id = str(uuid4())
        call("PUT", f"/api/triggers/{trigger_id}", json={"graph_version_id": version["graph_version_id"]})
        path = f"/api/triggers/{trigger_id}/runs"
        request = {"json": {"objective": "Verify API admission only; no model execution"},
                   "headers": {"Idempotency-Key": "smoke-occurrence"}}
        receipt = call("POST", path, **request)
        assert call("POST", path, **request) == receipt
        run = call("GET", f"/api/runs/{receipt['run_id']}")
        assert run["status"] == "created"
        print(json.dumps({"graph_id": graph_id, "run_id": receipt["run_id"],
                          "status": run["status"], "duplicate_request_reused_run": True}))


if __name__ == "__main__":
    main()
