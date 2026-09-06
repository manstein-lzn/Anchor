"""Independent deterministic and model-backed verifier executor."""

from __future__ import annotations

import asyncio
import json
from dataclasses import dataclass
from typing import Any, Literal
from uuid import UUID, uuid4

from pydantic import Field, ValidationError

from anchor.domain.conditions import EVALUATOR_VERSION, ConditionError, evaluate_condition
from anchor.domain.context import canonical_json, input_hash
from anchor.domain.graph import NodeType
from anchor.domain.models import DomainModel, VerificationRecord, VerificationVerdict
from anchor.runtime.artifacts import ArtifactStore
from anchor.runtime.capabilities import CapabilityRegistry
from anchor.runtime.model_gateway import ModelGateway, ModelResponse
from anchor.runtime.resolution import PREDECESSOR_TEXT_LIMIT, ResolvedNodeContext, resolve_node_context
from anchor.runtime.sinks import VerificationCheckpointSink


MODEL_ADAPTER_VERSION = "anchor-verifier-json-v1"
ARTIFACT_PREFIX = "artifact://sha256/"


class ModelVerdict(DomainModel):
    verdict: Literal["passed", "rejected"]
    reason: str = Field(min_length=1, max_length=4000)


@dataclass(frozen=True)
class VerificationTarget:
    context: dict[str, Any]
    context_hash: str
    artifacts: tuple[dict[str, Any], ...]
    artifact_hashes: tuple[str, ...]
    errors: tuple[str, ...]


def parse_model_verdict(text: str) -> tuple[VerificationVerdict, str]:
    """Pure strict-JSON verdict rule shared by the online worker and evals.

    Ordinary prose, non-JSON output, or schema violations become `error`;
    only `{"verdict": "passed" | "rejected", "reason": non-empty}` passes.
    """
    try:
        parsed = ModelVerdict.model_validate_json(text)
    except ValidationError as exc:
        return (
            VerificationVerdict.ERROR,
            f"model response does not satisfy verifier JSON contract: {exc.error_count()} validation error(s)",
        )
    return (VerificationVerdict(parsed.verdict), parsed.reason)


@dataclass(frozen=True)
class VerificationDecision:
    verdict: VerificationVerdict
    reason: str
    adapter: str
    adapter_version: str
    response: ModelResponse | None = None


@dataclass(frozen=True)
class VerifierOutcome:
    claim_id: UUID
    node_run_id: UUID
    node_id: str
    verdict: VerificationVerdict
    evidence_ref: str


def _selected_source_ids(store, run_id: UUID, resolved: ResolvedNodeContext) -> list[str]:
    incoming = [
        item for item in store.list_edge_decisions(run_id)
        if item.target_node_id == resolved.node.id
    ]
    if incoming:
        indexes = {item.edge_index for item in incoming if item.selected}
        return sorted({
            edge.source for index, edge in enumerate(resolved.graph.definition.edges)
            if index in indexes
        })
    return sorted({
        edge.source for edge in resolved.graph.definition.edges
        if edge.target == resolved.node.id and edge.condition is None
    })


def resolve_verification_target(
    store,
    run_id: UUID,
    resolved: ResolvedNodeContext,
    artifacts: ArtifactStore,
) -> VerificationTarget:
    node_runs = {item.node_id: item for item in store.list_node_runs(run_id)}
    target_artifacts: list[dict[str, Any]] = []
    hashes: list[str] = []
    errors: list[str] = []
    for source_id in _selected_source_ids(store, run_id, resolved):
        source = node_runs.get(source_id)
        ref = source.output_ref if source is not None else None
        if not ref or not ref.startswith(ARTIFACT_PREFIX):
            errors.append(f"selected predecessor {source_id!r} has no content-addressed artifact")
            continue
        digest = ref[len(ARTIFACT_PREFIX):]
        try:
            text = artifacts.get_text(ref)
        except (OSError, ValueError) as exc:
            errors.append(f"selected predecessor {source_id!r} artifact is unavailable or invalid: {exc}")
            continue
        truncated = len(text) > PREDECESSOR_TEXT_LIMIT
        displayed = text[:PREDECESSOR_TEXT_LIMIT] if truncated else text
        try:
            content: Any = json.loads(displayed) if not truncated else displayed
        except json.JSONDecodeError:
            content = displayed
        hashes.append(digest)
        target_artifacts.append({
            "source_node_id": source_id,
            "ref": ref,
            "sha256": digest,
            "content": content,
            "truncated": truncated,
        })
    context = {
        "task": {
            "objective": resolved.task.objective,
            "constraints": resolved.task.constraints,
            "success_criteria": resolved.task.success_criteria,
        },
        "context": resolved.snapshot,
        "artifacts": target_artifacts,
    }
    return VerificationTarget(
        context=context,
        context_hash=input_hash(resolved.snapshot),
        artifacts=tuple(target_artifacts),
        artifact_hashes=tuple(sorted(set(hashes))),
        errors=tuple(errors),
    )


class VerifierNodeWorker:
    """Claim only Verifier nodes and require a durable structured verdict."""

    def __init__(
        self,
        store,
        registry: CapabilityRegistry,
        gateways: dict[str, ModelGateway],
        artifacts: ArtifactStore,
        sink: VerificationCheckpointSink,
    ) -> None:
        self.store = store
        self.registry = registry
        self.gateways = gateways
        self.artifacts = artifacts
        self.sink = sink

    async def execute_once(
        self,
        *,
        worker_id: str,
        claim_id: UUID | None = None,
        heartbeat_interval: float = 5.0,
    ) -> VerifierOutcome | None:
        claim_id = claim_id or uuid4()
        lease = self.store.claim_ready_verifier_node(worker_id, claim_id)
        if lease is None:
            return None
        return await self.execute_claimed_once(
            worker_id=worker_id,
            lease=lease,
            heartbeat_interval=heartbeat_interval,
        )

    async def execute_claimed_once(
        self,
        *,
        worker_id: str,
        lease,
        heartbeat_interval: float = 5.0,
    ) -> VerifierOutcome:
        if heartbeat_interval <= 0:
            raise ValueError("heartbeat_interval must be positive")

        async def heartbeat() -> None:
            while True:
                await asyncio.sleep(heartbeat_interval)
                self.store.heartbeat_node_lease(lease.claim_id, worker_id)

        heartbeat_task = asyncio.create_task(heartbeat())
        try:
            resolved = resolve_node_context(
                self.store, lease.run_id, lease.node_id, self.artifacts,
            )
            if resolved.node.type is not NodeType.VERIFIER or not resolved.node.verifier_ref:
                raise ValueError(f"node {lease.node_id} is not an executable Verifier node")
            capability = self.registry.validate_verifier(resolved.node.verifier_ref)
            target = resolve_verification_target(
                self.store, lease.run_id, resolved, self.artifacts,
            )
            decision = await self._decide(capability, target)
            evidence = {
                "schema": "anchor.verification.evidence.v1",
                "verifier_ref": capability.ref,
                "verifier_version": capability.version,
                "adapter": decision.adapter,
                "adapter_version": decision.adapter_version,
                "verdict": decision.verdict.value,
                "reason": decision.reason,
                "verified_context_hash": target.context_hash,
                "verified_artifact_hashes": list(target.artifact_hashes),
                "target_errors": list(target.errors),
                "model_response": decision.response.text if decision.response is not None else None,
            }
            evidence_ref = self.artifacts.put_text(
                canonical_json(evidence), media_type="application/json",
            )
            profile = (
                self.registry.model(capability.model_ref)
                if capability.model_ref is not None else None
            )
            record = VerificationRecord(
                claim_id=lease.claim_id,
                run_id=lease.run_id,
                node_run_id=lease.node_run_id,
                node_id=lease.node_id,
                verifier_ref=capability.ref,
                verifier_version=capability.version,
                adapter=decision.adapter,
                adapter_version=decision.adapter_version,
                verdict=decision.verdict,
                reason=decision.reason,
                evidence_ref=evidence_ref,
                verified_artifact_hashes=list(target.artifact_hashes),
                verified_context_hash=target.context_hash,
                model_ref=capability.model_ref,
                model_provider=profile.provider if profile is not None else None,
                model_name=profile.model if profile is not None else None,
                model_response_id=(decision.response.response_id if decision.response is not None else None),
            )
            await self.sink.persist(
                verification=record,
                input_snapshot=resolved.snapshot,
            )
            return VerifierOutcome(
                claim_id=lease.claim_id,
                node_run_id=lease.node_run_id,
                node_id=lease.node_id,
                verdict=decision.verdict,
                evidence_ref=evidence_ref,
            )
        finally:
            heartbeat_task.cancel()
            try:
                await heartbeat_task
            except asyncio.CancelledError:
                pass

    async def _decide(self, capability, target: VerificationTarget) -> VerificationDecision:
        if target.errors:
            return VerificationDecision(
                verdict=VerificationVerdict.ERROR,
                reason="; ".join(target.errors)[:4000],
                adapter=("deterministic_jmespath" if capability.adapter == "deterministic" else "model_json"),
                adapter_version=(EVALUATOR_VERSION if capability.adapter == "deterministic" else MODEL_ADAPTER_VERSION),
            )
        if capability.adapter == "deterministic":
            try:
                passed = evaluate_condition(capability.expression, target.context)
            except ConditionError as exc:
                return VerificationDecision(
                    verdict=VerificationVerdict.ERROR,
                    reason=str(exc)[:4000],
                    adapter="deterministic_jmespath",
                    adapter_version=EVALUATOR_VERSION,
                )
            return VerificationDecision(
                verdict=(VerificationVerdict.PASSED if passed else VerificationVerdict.REJECTED),
                reason=f"JMESPath expression evaluated to {str(passed).lower()}",
                adapter="deterministic_jmespath",
                adapter_version=EVALUATOR_VERSION,
            )
        gateway = self.gateways.get(capability.model_ref)
        if gateway is None:
            raise RuntimeError(f"no model gateway configured: {capability.model_ref}")
        prompt = (
            "Verify the following immutable task context and artifacts. Return exactly one JSON object "
            "with schema {\"verdict\":\"passed|rejected\",\"reason\":\"specific evidence-based reason\"}. "
            "Do not claim passed unless the supplied evidence satisfies the success criteria.\n\n"
            + canonical_json(target.context)
        )
        response = await gateway.generate(
            prompt=prompt,
            system_prompt=(
                "You are an independent quality verifier. Output strict JSON only. "
                + capability.instructions
            ).strip(),
        )
        verdict, reason = parse_model_verdict(response.text)
        return VerificationDecision(
            verdict=verdict,
            reason=reason,
            adapter="model_json",
            adapter_version=MODEL_ADAPTER_VERSION,
            response=response,
        )
