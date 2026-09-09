"""Result sinks that bridge model responses to canonical state."""

from __future__ import annotations

from typing import Mapping
from uuid import UUID

from anchor.domain.conditions import build_condition_context
from anchor.domain.context import canonical_json
from anchor.domain.models import VerificationRecord, VerificationVerdict
from anchor.runtime.artifacts import ArtifactStore
from anchor.runtime.model_gateway import ModelResponse
from anchor.domain.propagation import RoutingDecisionError
from anchor.state.protocols import StateStore


class ArtifactCheckpointSink:
    """Persist response content, then atomically checkpoint its reference."""

    def __init__(self, store: StateStore, artifacts: ArtifactStore, worker_id: str) -> None:
        self.store = store
        self.artifacts = artifacts
        self.worker_id = worker_id

    async def persist_model_result(self, *, claim_id: UUID, node_run_id: UUID,
                                   response: ModelResponse, input_hash: str | None = None,
                                   input_snapshot: Mapping[str, object] | None = None,
                                   output_ref: str | None = None,
                                   event_payload: Mapping[str, object] | None = None) -> None:
        # The model text is always persisted; when the node's output is a
        # workspace revision the artifact is referenced from the completion
        # event instead of becoming the output reference.
        response_ref = self.artifacts.put_text(response.text)
        payload: dict[str, object] = {"response_ref": response_ref}
        if event_payload:
            payload.update(event_payload)
        snapshot = dict(input_snapshot) if input_snapshot is not None else None
        try:
            self.store.complete_node_and_propagate(
                claim_id, self.worker_id, output_ref=output_ref or response_ref,
                node_input_hash=input_hash,
                input_snapshot=snapshot,
                condition_context=build_condition_context(response.text, snapshot),
                event_payload=payload,
            )
        except RoutingDecisionError:
            # Evaluation is local and the completion transaction rolled back,
            # so this is a known failure rather than an unknown model outcome.
            self.store.fail_node_and_propagate(
                claim_id,
                self.worker_id,
                error_code="routing_condition_invalid",
                phase="routing",
            )
            raise

    async def persist_control_result(
        self,
        *,
        claim_id: UUID,
        node_run_id: UUID,
        output: Mapping[str, object],
        input_snapshot: Mapping[str, object],
    ) -> str:
        """Checkpoint deterministic control output without a model response."""
        text = canonical_json(dict(output))
        ref = self.artifacts.put_text(text, media_type="application/json")
        snapshot = dict(input_snapshot)
        try:
            self.store.complete_node_and_propagate(
                claim_id,
                self.worker_id,
                output_ref=ref,
                input_snapshot=snapshot,
                condition_context={"output": dict(output), "inputs": snapshot},
            )
        except RoutingDecisionError:
            self.store.fail_node_and_propagate(
                claim_id,
                self.worker_id,
                error_code="routing_condition_invalid",
                phase="routing",
            )
            raise
        return ref


class VerificationCheckpointSink:
    """Atomically bind a verifier verdict to its canonical state transition."""

    def __init__(self, store: StateStore, worker_id: str) -> None:
        self.store = store
        self.worker_id = worker_id

    async def persist(
        self,
        *,
        verification: VerificationRecord,
        input_snapshot: Mapping[str, object],
    ) -> None:
        if verification.verdict is VerificationVerdict.PASSED:
            condition_context = {
                "output": {
                    "verdict": verification.verdict.value,
                    "reason": verification.reason,
                    "verifier_ref": verification.verifier_ref,
                    "verifier_version": verification.verifier_version,
                },
                "inputs": dict(input_snapshot),
            }
            try:
                self.store.complete_node_and_propagate(
                    verification.claim_id,
                    self.worker_id,
                    output_ref=verification.evidence_ref,
                    input_snapshot=dict(input_snapshot),
                    condition_context=condition_context,
                    verification=verification,
                )
            except RoutingDecisionError:
                # The verifier passed, but its outgoing Graph condition is a
                # known local failure. Preserve both facts atomically.
                self.store.fail_node_and_propagate(
                    verification.claim_id,
                    self.worker_id,
                    error_code="routing_condition_invalid",
                    phase="routing",
                    verification=verification,
                    input_snapshot=dict(input_snapshot),
                )
                raise
            return
        self.store.fail_node_and_propagate(
            verification.claim_id,
            self.worker_id,
            error_code=(
                "verification_rejected"
                if verification.verdict is VerificationVerdict.REJECTED
                else "verification_error"
            ),
            phase="verification",
            verification=verification,
            input_snapshot=dict(input_snapshot),
        )
