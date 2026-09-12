"""Machine-readable reference for authoring a Graph IR.

An agent cannot guess the IR. This module is the single source of truth for the
node contract, the edge condition language, input mapping, metadata conventions
and a working template, and it is served over the API so any client sees the
schema of the running instance rather than a copy that can drift.
"""

from __future__ import annotations

from .graph import NodeType

IR_VERSION = "1"

# Node types that require a capability reference, and the metadata key that
# capability validation additionally demands.
NODE_REFERENCE = {
    NodeType.AGENT: ("agent_ref", "an agent registered in the runtime profile"),
    NodeType.TOOL: ("tool_ref", "a tool registered in the runtime profile; the node "
                                 "must also set metadata.owner_agent and the owner "
                                 "must list the tool in its tool_refs"),
    NodeType.VERIFIER: ("verifier_ref", "a verifier registered in the runtime profile"),
    NodeType.SUBGRAPH: ("subgraph_version_id", "a published graph_version_id"),
}

NODE_NOTES = {
    NodeType.LOOP: "requires exit_condition; each return to a completed node starts a "
                   "new attempt, so cycles are unbounded by design",
    NodeType.ROUTER: "deterministic control node; outgoing edge conditions are "
                     "evaluated against this node's own resolved snapshot",
    NodeType.PARALLEL: "deterministic fan-out; every outgoing edge with a true or "
                       "absent condition opens",
    NodeType.JOIN: "deterministic fan-in; waits for the incoming branches selected "
                   "by edge decisions",
    NodeType.ARTIFACT: "deterministic export; set metadata.behavior_ref when a "
                       "registered behavior builds the artifact",
    NodeType.APPROVAL: "parks as waiting_approval until a human decides; the node "
                       "resumes only through the waits API",
    NodeType.WAIT_FOR_EVENT: "parks as waiting_event until a matching event resumes it",
    NodeType.HUMAN_TASK: "parks for human input like an approval, but records free "
                         "form input instead of a boolean decision",
    NodeType.TOOL: "runs through the ledger-backed gateway under metadata.owner_agent; "
                   "a side-effect tool additionally needs a completed approval or "
                   "human_task predecessor",
}

COMMON_NODE_FIELDS = {
    "id": "unique, starts with a letter, [A-Za-z0-9_-]{0..63}",
    "type": f"one of: {', '.join(item.value for item in NodeType)}",
    "name": "human-readable label",
    "input_schema": "optional opaque schema name for the node input",
    "output_schema": "set to 'json' to require the agent to return JSON",
    "retry_policy": "opaque policy name; defaults to 'default'",
    "timeout_seconds": "optional positive integer",
    "approval_required": "optional boolean flag on the node",
    "exit_condition": "required for loop nodes; a JMESPath expression",
    "progress_signal": "optional JMESPath expression describing forward progress",
    "metadata": "string map; see metadata_keys",
}

METADATA_KEYS = {
    "owner_agent": "required on tool nodes: the agent that authorizes the tool call",
    "behavior_ref": "binds a registered node behavior (for example academic.report)",
    "context_mode": "'full' disables predecessor text truncation for this node",
    "run_timeout_seconds": "graph-level metadata only; an opt-in wall clock budget",
    "max_rounds": ("graph-level metadata only; an opt-in revision ceiling. A back-edge that "
                   "would start a revision past it is not taken, and the decision records "
                   "reason=revision_ceiling"),
}

CONDITION_REFERENCE = {
    "language": "JMESPath",
    "context": {
        "output": "the source node output: parsed JSON when it is valid JSON, "
                  "otherwise the raw text",
        "inputs": "the resolved input snapshot of the source node",
    },
    "result": "the expression must evaluate to a JSON boolean; any other type fails "
              "the edge closed and is recorded as a decision",
    "examples": [
        "output.verdict == 'pass'",
        "output.review.verdict == 'revise'",
        "inputs.approved == `true`",
        "length(output.sources) >= `3`",
    ],
    "evaluator": "jmespath",
}

INPUT_MAPPING_REFERENCE = {
    "purpose": "map values from the resolved upstream snapshot into the target node "
               "input under a chosen key",
    "syntax": "{\"<target_input_key>\": \"outputs.<source_node_id>.<path>\"}",
    "notes": "outputs.<node> resolves to that predecessor's parsed JSON output, or "
             "its text when not JSON; the target node sees the mapped keys at the "
             "top level of its input snapshot",
    "examples": [
        "{\"args\": \"outputs.start\"}",
        "{\"topic\": \"outputs.plan.topic\"}",
    ],
}

DEFINITION_FIELDS = {
    "graph_id": "stable identifier, max 128 characters",
    "name": "human-readable graph name",
    "nodes": "at least one node",
    "edges": "optional list of edges",
    "entry_node_id": "optional explicit entry; defaults to nodes with no incoming edge",
    "metadata": ("optional string map; run_timeout_seconds and max_rounds are the recognized "
                 "keys, both optional, neither defaulted"),
}

AUTHORING_FLOW = [
    "GET /api/runtime/capabilities to learn available agent_ref, tool_ref and "
    "verifier_ref values",
    "POST /api/graphs/validate with the definition to check structure",
    "POST /api/graphs/capabilities/validate to check that every reference resolves",
    "PUT /api/graphs/{graph_id}/draft with expected_revision to save",
    "POST /api/graphs/{graph_id}/publish with expected_revision to publish an "
    "immutable version",
    "PUT /api/triggers/{trigger_id} then POST /api/triggers/{trigger_id}/runs "
    "with an Idempotency-Key to start a run",
]

TEMPLATE = {
    "graph_id": "example-graph",
    "name": "Example graph",
    "nodes": [
        {"id": "plan", "type": "agent", "name": "Plan",
         "agent_ref": "agents.academic.planner", "output_schema": "json"},
        {"id": "gate", "type": "approval", "name": "Human review"},
        {"id": "report", "type": "artifact", "name": "Export report",
         "metadata": {"behavior_ref": "academic.report", "context_mode": "full"}},
    ],
    "edges": [
        {"source": "plan", "target": "gate"},
        {"source": "gate", "target": "report"},
    ],
}


def describe_ir() -> dict:
    """Return the full authoring reference for the current Graph IR version."""
    node_types = {}
    for node_type in NodeType:
        reference = NODE_REFERENCE.get(node_type)
        node_types[node_type.value] = {
            "required_reference": reference[0] if reference else None,
            "reference_meaning": reference[1] if reference else None,
            "notes": NODE_NOTES.get(node_type),
        }
    return {
        "ir_version": IR_VERSION,
        "definition_fields": DEFINITION_FIELDS,
        "common_node_fields": COMMON_NODE_FIELDS,
        "node_types": node_types,
        "edge_fields": {
            "source": "source node id",
            "target": "target node id",
            "condition": "optional JMESPath expression; absent means unconditional",
            "input_mapping": "optional map, see input_mapping",
        },
        "condition": CONDITION_REFERENCE,
        "input_mapping": INPUT_MAPPING_REFERENCE,
        "metadata_keys": METADATA_KEYS,
        "authoring_flow": AUTHORING_FLOW,
        "template": TEMPLATE,
    }
