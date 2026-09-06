# Run Admission and Dispatch

## Implemented contract

`RelationalStateStore.admit_run(RunRequest)` is the local-development entry point for
an authenticated, normalized trigger occurrence. It is not an HTTP webhook handler
and does not evaluate filters, cron expressions, permissions or event signatures.
Those checks belong in the future trigger ingress adapter, before admission.

The request identifies a stored trigger and a stable occurrence key. UI retries use
the same key; a new intentional run uses a new key. A schedule adapter must derive a
key from the scheduled occurrence, not from the delivery attempt. A webhook adapter
must namespace the upstream event identity within its registered trigger.

In one database transaction, admission:

1. Checks the `(trigger_id, idempotency_key)` receipt.
2. Rejects different content under an already-used key, or returns the original receipt.
3. Validates that the trigger is enabled and its graph version is published.
4. Creates Task, version-pinned Run and initial pending NodeRun records.
5. Writes task/run creation events and a versioned `run.requested` outbox envelope.
6. Stores the receipt and commits everything together.

The request's inputs are retained in the admission record and dispatch envelope.
They must not contain raw credentials. Node inputs have not yet been resolved and
pending NodeRun records are not execution checkpoints.

The stored graph content cannot be replaced via `publish_graph`. Publication
revalidates its schema, topology, graph identity and content hash. Returned Python
DTOs are still mutable; changing one does not update the stored version.

## Delivery contract

`dispatch_pending` is a single-dispatcher batch pump, not a scheduler or workflow
engine. Its batch size limits a database page, not Agent execution or task lifetime.

The target's `accept` method must durably accept and deduplicate the message before
returning. It receives stable `message_id` and `run_id` identifiers. Only then does
the pump acknowledge the outbox row. If the target accepts but the response or local
acknowledgement is lost, the same envelope is sent again. This is at-least-once
delivery, not exactly-once execution.

The first production-shaped target is `DurableExecutionReceiver`. Acceptance stores
the complete immutable envelope in `execution_inbox`, appends `run.dispatch_accepted`
and `node.ready`, advances the Task to `ready` and the Run to `queued`, then allows the
outbox acknowledgement. The transaction is replay-safe by `message_id`; an
acknowledgement loss re-delivers the same envelope without another event or revision.
Only the resolved entry node becomes ready. This receiver does not call a model or tool.

A worker claims a ready node using its own stable `claim_id`. Claim replay returns the
same lease; a different worker cannot reuse it. Before invoking any external tool, the
worker must register a stable `operation_id` and the canonical request hash in the tool
operation ledger, then mark it running before the remote call. Outcomes are `succeeded`,
`failed`, or `outcome_unknown`. Unknown is terminal pending explicit reconciliation and
must never trigger an automatic retry. Operation arguments must not contain credentials.
An explicit reconciliation may resolve an unknown outcome only to succeeded or failed,
and must record a durable provider/evidence reference. Replaying the same evidence is
idempotent; conflicting evidence fails closed. The read API exposes the ledger for audit.

Failures leave the message pending. The local receiver daemon logs a failed batch and
polls again; it has no dead-letter queue, multi-dispatcher lease or poison-message
quarantine yet. The existing `InProcessWorkflowService` is a deterministic test harness,
not a compatible durable dispatch target; it must not be wired to this pump as-is.

## Verified failure cases

- Duplicate requests, reordered JSON keys and concurrent connections.
- Conflicting reuse of an occurrence key.
- Failed outbox insertion rolls back nested Task/Run/NodeRun writes.
- Subprocess exit before commit leaves no partial admission.
- Subprocess exit after commit preserves the original receipt for retry.
- Receiver accepts, acknowledgement is lost, then the same message is redelivered.
- A new graph version does not change an admitted run's version binding.
- A node outside the pinned graph cannot be inserted into its run.

The relational adapter now exercises the same admission/delivery principles on real
PostgreSQL as well as SQLite; see POSTGRES.md for setup and verification. Tests use
temporary databases/schemas and test receivers. They do not prove
Prefect/Temporal recovery, machine power-loss durability, external tool idempotency,
multi-machine coordination or months-long Agent quality. The production workflow
adapter remains required; PostgreSQL migrations are available but not a legacy data importer.
