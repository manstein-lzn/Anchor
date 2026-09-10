"""Typed client for the Anchor API: the one operation layer agents build on.

The CLI and the MCP server are both thin adapters over this class, so their
semantics cannot drift. Every call goes through the authenticated HTTP API; the
client is never a privileged shortcut around the kernel's guards, which is what
keeps approvals, leases, the operation ledger and the audit trail meaningful
when an agent rather than a human is driving.
"""

from __future__ import annotations

import os
import time
from pathlib import Path
from typing import Any, Iterator
from uuid import uuid4

import httpx


class AnchorApiError(RuntimeError):
    """A structured API failure: stable code plus HTTP status."""

    def __init__(self, status: int, code: str, detail: Any = None, *, path: str = "") -> None:
        super().__init__(f"{code} (HTTP {status})" + (f" at {path}" if path else ""))
        self.status = status
        self.code = code
        self.detail = detail
        self.path = path

    @property
    def retryable(self) -> bool:
        return self.status >= 500 or self.status == 429

    def as_dict(self) -> dict:
        return {"status": self.status, "code": self.code,
                "detail": self.detail, "path": self.path}


class AnchorClient:
    """Synchronous HTTP client for orchestration, execution and observation."""

    def __init__(self, base_url: str | None = None, token: str | None = None,
                 *, timeout: float = 30.0, token_file: str | os.PathLike | None = None,
                 transport: httpx.BaseTransport | None = None) -> None:
        if token is None:
            token = self._read_token(token_file)
        # An MCP client configures the server through the environment, so the
        # default must come from the environment before the built-in address.
        base_url = base_url or os.environ.get("ANCHOR_API_URL") or "http://127.0.0.1:8090"
        self.base_url = base_url.rstrip("/")
        self._client = httpx.Client(base_url=self.base_url, timeout=timeout, trust_env=False,
                                    transport=transport,
                                    headers={"Authorization": f"Bearer {token}"})

    @staticmethod
    def _read_token(token_file: str | os.PathLike | None) -> str:
        token = os.environ.get("ANCHOR_API_TOKEN")
        if token:
            return token
        path = Path(token_file or os.environ.get("ANCHOR_TOKEN_FILE", ".local/api-token"))
        if not path.exists():
            raise RuntimeError(
                "no API token: set ANCHOR_API_TOKEN or point ANCHOR_TOKEN_FILE at the token file")
        return path.read_text(encoding="utf-8").strip()

    def close(self) -> None:
        self._client.close()

    def __enter__(self) -> "AnchorClient":
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()

    # -- transport ---------------------------------------------------------
    def _request(self, method: str, path: str, **kwargs: Any) -> Any:
        response = self._client.request(method, path, **kwargs)
        if response.status_code >= 400:
            try:
                body = response.json()
            except ValueError:
                body = response.text
            code = body.get("error", {}).get("code") if isinstance(body, dict) else None
            detail = body.get("error", {}).get("detail") if isinstance(body, dict) else body
            if code is None and isinstance(body, dict):
                code = body.get("detail") if isinstance(body.get("detail"), str) else "api_error"
            raise AnchorApiError(response.status_code, code or "api_error", detail, path=path)
        if not response.content:
            return None
        content_type = response.headers.get("content-type", "")
        return response.json() if "json" in content_type else response.text

    # -- discovery ---------------------------------------------------------
    def health(self) -> dict:
        return self._request("GET", "/health/ready")

    def capabilities(self) -> dict:
        """Available model, agent, tool and verifier references."""
        return self._request("GET", "/api/runtime/capabilities")

    def ir(self) -> dict:
        """Machine-readable Graph IR authoring reference."""
        return self._request("GET", "/api/graphs/ir")

    def list_graphs(self, *, limit: int = 50, offset: int = 0) -> list[dict]:
        return self._request("GET", "/api/graphs", params={"limit": limit, "offset": offset})

    def get_draft(self, graph_id: str) -> dict | None:
        try:
            return self._request("GET", f"/api/graphs/{graph_id}/draft")
        except AnchorApiError as exc:
            if exc.status == 404:
                return None
            raise

    def list_versions(self, graph_id: str, *, limit: int = 50, offset: int = 0) -> list[dict]:
        return self._request("GET", f"/api/graphs/{graph_id}/versions",
                             params={"limit": limit, "offset": offset})

    def get_version(self, version_id: str) -> dict:
        return self._request("GET", f"/api/graph-versions/{version_id}")

    # -- authoring ---------------------------------------------------------
    def validate(self, definition: dict) -> dict:
        return self._request("POST", "/api/graphs/validate", json=definition)

    def validate_capabilities(self, definition: dict) -> dict:
        return self._request("POST", "/api/graphs/capabilities/validate", json=definition)

    def save_draft(self, graph_id: str, definition: dict, *, expected_revision: int = 0,
                   layout: dict | None = None) -> dict:
        return self._request("PUT", f"/api/graphs/{graph_id}/draft", json={
            "expected_revision": expected_revision, "definition": definition,
            "layout": layout or {}})

    def publish(self, graph_id: str, *, expected_revision: int) -> dict:
        return self._request("POST", f"/api/graphs/{graph_id}/publish",
                             json={"expected_revision": expected_revision})

    def export_bundle(self, version_id: str) -> dict:
        return self._request("GET", f"/api/graph-versions/{version_id}/bundle")

    def import_bundle(self, bundle: dict, *, expected_revision: int = 0,
                      publish: bool = True, import_triggers: bool = True) -> dict:
        return self._request("POST", "/api/bundles/import", json={
            "bundle": bundle, "expected_revision": expected_revision,
            "publish": publish, "import_triggers": import_triggers})

    def install(self, definition: dict, *, publish: bool = True) -> dict:
        """Validate, save and optionally publish in one idempotent operation."""
        graph_id = definition["graph_id"]
        structural = self.validate(definition)
        if not structural.get("valid"):
            raise AnchorApiError(422, "graph_invalid", structural.get("issues"), path="validate")
        capability = self.validate_capabilities(definition)
        if not capability.get("valid"):
            raise AnchorApiError(422, "capability_invalid", capability.get("issues"),
                                 path="capabilities/validate")
        draft = self.get_draft(graph_id)
        revision = draft["revision"] if draft else 0
        saved = self.save_draft(graph_id, definition, expected_revision=revision)
        result: dict[str, Any] = {"draft": saved, "version": None}
        if publish:
            result["version"] = self.publish(graph_id, expected_revision=saved["revision"])
        return result

    # -- triggers ----------------------------------------------------------
    def register_trigger(self, graph_version_id: str, *, trigger_id: str | None = None,
                         type: str = "manual", enabled: bool = True, **fields: Any) -> dict:
        return self._request("PUT", f"/api/triggers/{trigger_id or uuid4()}", json={
            "graph_version_id": graph_version_id, "type": type, "enabled": enabled, **fields})

    def list_triggers(self, version_id: str) -> list[dict]:
        return self._request("GET", f"/api/graph-versions/{version_id}/triggers")

    def set_trigger_enabled(self, trigger_id: str, enabled: bool) -> dict:
        return self._request("PATCH", f"/api/triggers/{trigger_id}", json={"enabled": enabled})

    # -- execution ---------------------------------------------------------
    def start_run(self, trigger_id: str, *, objective: str, inputs: dict | None = None,
                  idempotency_key: str | None = None) -> dict:
        return self._request("POST", f"/api/triggers/{trigger_id}/runs",
                             json={"objective": objective, "inputs": inputs or {}},
                             headers={"Idempotency-Key": idempotency_key or str(uuid4())})

    def get_run(self, run_id: str) -> dict:
        return self._request("GET", f"/api/runs/{run_id}")

    def list_runs(self, *, graph_id: str | None = None, status: list[str] | None = None,
                  include_archived: bool = False, limit: int = 50, offset: int = 0) -> list[dict]:
        params: list[tuple[str, Any]] = [("limit", limit), ("offset", offset)]
        if graph_id:
            params.append(("graph_id", graph_id))
        if include_archived:
            params.append(("include_archived", "true"))
        for value in status or []:
            params.append(("status", value))
        return self._request("GET", "/api/runs", params=params)

    def run_nodes(self, run_id: str) -> list[dict]:
        return self._request("GET", f"/api/runs/{run_id}/nodes")

    def run_events(self, run_id: str, *, after: int = 0, limit: int = 100) -> list[dict]:
        return self._request("GET", f"/api/runs/{run_id}/events",
                             params={"after": after, "limit": limit})

    def run_decisions(self, run_id: str) -> list[dict]:
        return self._request("GET", f"/api/runs/{run_id}/decisions")

    def run_verifications(self, run_id: str) -> list[dict]:
        return self._request("GET", f"/api/runs/{run_id}/verifications")

    def run_operations(self, run_id: str) -> list[dict]:
        return self._request("GET", f"/api/runs/{run_id}/operations")

    def run_usage(self, run_id: str) -> dict:
        """Token spend per node. A tool loop re-sends its conversation on every
        call, so this is the number that decides whether a run is affordable."""
        return self._request("GET", f"/api/runs/{run_id}/usage")

    def run_diagnostics(self, run_id: str) -> list[dict]:
        return self._request("GET", f"/api/runs/{run_id}/diagnostics")

    def run_progress(self, run_id: str) -> list[dict]:
        return self._request("GET", f"/api/runs/{run_id}/progress")

    def pause_run(self, run_id: str, *, reason: str, actor: str = "agent") -> dict:
        return self._request("POST", f"/api/runs/{run_id}/pause",
                             json={"reason": reason, "actor": actor})

    def resume_run(self, run_id: str, *, reason: str, actor: str = "agent") -> dict:
        return self._request("POST", f"/api/runs/{run_id}/resume",
                             json={"reason": reason, "actor": actor})

    def stop_run(self, run_id: str, *, reason: str) -> dict:
        return self._request("POST", f"/api/runs/{run_id}/stop", json={"reason": reason})

    def archive_run(self, run_id: str) -> dict:
        return self._request("POST", f"/api/runs/{run_id}/archive")

    def unarchive_run(self, run_id: str) -> dict:
        return self._request("POST", f"/api/runs/{run_id}/unarchive")

    # -- observation -------------------------------------------------------
    TERMINAL = ("completed", "failed", "cancelled")

    def run_digest(self, run_id: str) -> dict:
        """Compact, token-bounded view: counts and blockers, not raw history."""
        run = self.get_run(run_id)
        nodes = self.run_nodes(run_id)
        counts: dict[str, int] = {}
        for node in nodes:
            counts[node["status"]] = counts.get(node["status"], 0) + 1
        waiting = [node for node in nodes
                   if node["status"] in ("waiting_approval", "waiting_event")]
        failed = [{"node_id": node["node_id"], "error_code": node.get("error_code"),
                   "attempt": node["attempt"]}
                  for node in nodes if node["status"] == "failed"]
        return {
            "run_id": run["id"], "status": run["status"], "phase": run["current_phase"],
            "revision": run["revision"], "last_event_sequence": run["last_event_sequence"],
            "node_status_counts": counts, "waiting": [
                {"node_run_id": node["id"], "node_id": node["node_id"],
                 "status": node["status"]} for node in waiting],
            "failed_nodes": failed,
            "terminal": run["status"] in self.TERMINAL,
            "updated_at": run["updated_at"],
        }

    def wait_for_run(self, run_id: str, *, timeout: float = 300.0,
                     interval: float = 2.0) -> dict:
        """Bounded long-poll. Returns the digest, terminal or timed out."""
        deadline = time.monotonic() + timeout
        while True:
            digest = self.run_digest(run_id)
            if digest["terminal"] or time.monotonic() >= deadline:
                digest["timed_out"] = not digest["terminal"]
                return digest
            time.sleep(interval)

    def list_waits(self) -> list[dict]:
        return self._request("GET", "/api/waits")

    def read_artifact(self, ref: str) -> str:
        digest = ref.rsplit("/", 1)[-1]
        result = self._request("GET", f"/api/artifacts/{digest}")
        return result["content"] if isinstance(result, dict) else result

    # -- intervention ------------------------------------------------------
    def reconcile_operation(self, operation_id: str, *, status: str, reconciliation_ref: str,
                            result_ref: str | None = None, error_code: str | None = None,
                            reason: str = "", actor: str = "agent") -> dict:
        body: dict[str, Any] = {"status": status, "reconciliation_ref": reconciliation_ref,
                                "reason": reason, "actor": actor}
        if result_ref is not None:
            body["result_ref"] = result_ref
        if error_code is not None:
            body["error_code"] = error_code
        return self._request("POST", f"/api/operations/{operation_id}/reconcile", json=body)

    # -- human-in-the-loop (the MCP surface refuses these by default) ------
    def approve_wait(self, node_run_id: str, *, reason: str, actor: str = "operator") -> dict:
        return self._request("POST", f"/api/waits/{node_run_id}/approve",
                             json={"reason": reason, "actor": actor})

    def reject_wait(self, node_run_id: str, *, reason: str, actor: str = "operator") -> dict:
        return self._request("POST", f"/api/waits/{node_run_id}/reject",
                             json={"reason": reason, "actor": actor})

    def resume_wait(self, node_run_id: str, *, event_type: str, payload: dict | None = None,
                    actor: str = "operator") -> dict:
        return self._request("POST", f"/api/waits/{node_run_id}/resume",
                             json={"event_type": event_type, "payload": payload or {},
                                   "actor": actor})

    # -- storage -----------------------------------------------------------
    def storage_report(self) -> dict:
        return self._request("GET", "/api/storage")

    def get_budget(self) -> dict:
        return self._request("GET", "/api/storage/budget")

    def set_budget(self, *, global_bytes: int | None = None,
                   graphs: dict[str, int | None] | None = None) -> dict:
        body: dict[str, Any] = {}
        if global_bytes is not None:
            body["global_bytes"] = global_bytes
        if graphs:
            body["graphs"] = graphs
        return self._request("PUT", "/api/storage/budget", json=body)

    def retention_preview(self) -> dict:
        return self._request("GET", "/api/retention/preview")

    def retention_sweep(self) -> dict:
        return self._request("POST", "/api/retention/sweep")

    def retention_audit(self, *, limit: int = 50) -> list[dict]:
        return self._request("GET", "/api/retention/audit", params={"limit": limit})

    def iter_events(self, run_id: str, *, after: int = 0, page: int = 200) -> Iterator[dict]:
        """Incremental event stream, safe to resume with the last sequence."""
        cursor = after
        while True:
            batch = self.run_events(run_id, after=cursor, limit=page)
            if not batch:
                return
            for event in batch:
                cursor = max(cursor, event["sequence"])
                yield event
            if len(batch) < page:
                return
