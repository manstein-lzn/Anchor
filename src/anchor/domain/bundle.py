"""Versioned Graph Bundle: the portable unit of modular delivery.

A bundle carries one pinned Graph Version plus everything needed to run it
elsewhere: trigger bindings, required capability references, and provenance.
It never carries secret values — only references resolved at runtime.

Integrity is tamper-evident: the bundle embeds the version content hash and
import recomputes it before accepting anything.
"""

from __future__ import annotations

import hashlib
import json
from datetime import datetime, timezone

from pydantic import Field

from anchor.domain.graph import GraphDefinition, Trigger
from anchor.domain.models import DomainModel


BUNDLE_VERSION = 1


class GraphBundle(DomainModel):
    bundle_version: int = Field(default=BUNDLE_VERSION)
    graph: GraphDefinition
    version: int = Field(gt=0)
    content_hash: str = Field(min_length=64, max_length=64)
    triggers: list[Trigger] = Field(default_factory=list)
    required_agents: list[str] = Field(default_factory=list)
    required_tools: list[str] = Field(default_factory=list)
    required_verifiers: list[str] = Field(default_factory=list)
    exported_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))

    def verify(self) -> GraphBundle:
        """Fail closed when the embedded content does not match its hash."""
        canonical = json.dumps(self.graph.model_dump(mode="json"), sort_keys=True,
                               separators=(",", ":"))
        digest = hashlib.sha256(canonical.encode("utf-8")).hexdigest()
        if digest != self.content_hash:
            raise ValueError("bundle content does not match its content hash")
        if self.bundle_version != BUNDLE_VERSION:
            raise ValueError(f"unsupported bundle version: {self.bundle_version}")
        return self


def build_bundle(definition: GraphDefinition, *, version: int, content_hash: str,
                 triggers: list[Trigger]) -> GraphBundle:
    agents = sorted({node.agent_ref for node in definition.nodes if node.agent_ref})
    tools = sorted({node.tool_ref for node in definition.nodes if node.tool_ref})
    verifiers = sorted({node.verifier_ref for node in definition.nodes if node.verifier_ref})
    return GraphBundle(graph=definition, version=version, content_hash=content_hash,
                       triggers=list(triggers), required_agents=agents,
                       required_tools=tools, required_verifiers=verifiers).verify()
