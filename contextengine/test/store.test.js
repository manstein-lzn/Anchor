import test from "node:test";
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { access, mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import { AnchorClient } from "../src/store.js";
import { hashValue } from "../src/update.js";

const run = promisify(execFile);

test("Anchor persists one session Task and an idempotent Checkpoint chain", async () => {
  const workspace = await mkdtemp(join(tmpdir(), "anchor-workspace-"));
  const statePath = join(await mkdtemp(join(tmpdir(), "anchor-state-")), "anchor.db");
  const kernel = join(process.cwd(), "python/anchor_kernel.py");
  const sessionId = "session-1";
  const proposalHash = hashValue({ proposal: 1 });
  const checkpoint = candidate({ kind: "planning", session_id: sessionId, source_hash: proposalHash }, {
    current_understanding: "The confirmed plan is ready.",
    current_directive: "Produce a verified result.",
    accepted_next_action: "Implement the result.",
    confirmed_facts: ["The user accepted the Contract."],
    next_plan: ["Implement the result."],
  }, "user");
  const begin = {
    title: "test",
    plan: "implement and verify",
    sessionId,
    proposalHash,
    contract: {
      schema: "anchor.contract.v1",
      goal: "Produce a verified result.",
      rationale: ["The work must survive context updates."],
      constraints: ["Keep changes scoped."],
      non_goals: ["Do not redesign unrelated modules."],
      acceptance_criteria: ["The focused test passes."],
      verification_commands: ["node --test test/store.test.js"],
      allowed_paths: ["test/store.test.js"],
      risks: ["A stale Checkpoint could resume incorrect work."],
      execution_plan: "Implement the bounded change and run the focused test.",
    },
    checkpoint,
  };
  const creator = new AnchorClient({ workspace, statePath, sessionId, kernel });
  const started = await creator.begin(begin);
  const repeated = await creator.begin(begin);

  assert.equal(repeated.task_id, started.task_id);
  assert.equal(repeated.checkpoint.receipt.event_id, started.checkpoint.receipt.event_id);
  assert.equal(started.checkpoint.checkpoint_version, 0);
  assert.equal(started.checkpoint.parent, null);
  assert.equal(started.task.state_version, 0);
  await assert.rejects(
    () => new AnchorClient({ workspace, statePath, sessionId: "session-2", taskId: started.task_id, kernel }).recovery(),
    /different session/,
  );

  const client = new AnchorClient({ workspace, statePath, sessionId, taskId: started.task_id, kernel });
  const next = candidate({
    kind: "compact",
    session_id: sessionId,
    first_kept_entry_id: "entry-1",
    episode_hash: hashValue([{ role: "user", content: "work" }]),
    is_split_turn: false,
  }, {
    current_understanding: "Implementation started.",
    current_directive: "Continue implementation.",
    accepted_next_action: "Verify the implementation.",
    failed_paths: ["The first command failed because its flag was invalid."],
    next_plan: ["Verify the implementation."],
  });
  const committed = await client.update(next, started.task.state_version);
  const replayed = await client.update(next, started.task.state_version);

  assert.equal(replayed.event_id, committed.event_id);
  assert.equal(committed.payload.parent.content_hash, started.checkpoint.receipt.content_hash);
  await assert.rejects(
    () => client.update(candidate(next.frontier, { ...next.cognition, current_understanding: "Conflicting replay." }), 1),
    /different cognition/,
  );
  await assert.rejects(
    () => client.update(candidate({ ...next.frontier, first_kept_entry_id: "entry-2", episode_hash: hashValue([2]) }), 0),
    /stale/,
  );

  const restartedClient = new AnchorClient({ workspace, statePath, sessionId, taskId: started.task_id, kernel });
  const restarted = await restartedClient.recovery();
  assert.equal(restarted.task.state_version, 1);
  assert.equal(restarted.checkpoint.checkpoint_version, 1);
  assert.equal(restarted.checkpoint.cognition.accepted_next_action, "Verify the implementation.");
  const recalledItem = restarted.checkpoint.cognition.experience.failed_paths[0];
  const recalled = await restartedClient.recall(`checkpoint:1:item:${recalledItem.id}`, hashValue(recalledItem));
  assert.deepEqual(recalled.item, recalledItem);
  assert.equal(recalled.schema, "anchor.recall.v1");
  await assert.rejects(
    () => restartedClient.recall(`checkpoint:1:item:${recalledItem.id}`, hashValue({ changed: true })),
    /content hash mismatch/,
  );
  await assert.rejects(() => restartedClient.recall("checkpoint:1:item:missing"), /item not found/);
  await assert.rejects(access(join(workspace, ".anchor")));

  await run("python3", ["-c", "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('update task set state_version=2'); c.commit()", statePath]);
  await assert.rejects(() => restartedClient.recovery(), /state_version does not match/);
  await run("python3", ["-c", "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('update task set state_version=1'); c.commit()", statePath]);
  await run("python3", ["-c", "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute(\"update checkpoints set cognition_hash='sha256:' || printf('%064d',0) where checkpoint_version=1\"); c.commit()", statePath]);
  await assert.rejects(() => restartedClient.recovery(), /Checkpoint index hash mismatch/);
  await run("python3", ["-c", "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute(\"update checkpoints set cognition_hash=? where checkpoint_version=1\",(sys.argv[2],)); c.commit()", statePath, hashValue(restarted.checkpoint.cognition)]);
  await run("python3", ["-c", "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute(\"update task set contract_json='{}'\"); c.commit()", statePath]);
  await assert.rejects(() => restartedClient.recovery(), /Contract content hash mismatch/);
});

function candidate(frontier, overrides = {}, confirmedBy) {
  return {
    schema: "anchor.checkpoint-candidate.v1",
    frontier,
    cognition: {
      schema: "anchor.cognition.v2",
      current_understanding: "Current understanding.",
      current_directive: "Continue.",
      accepted_next_action: "Take the next action.",
      confirmed_facts: [],
      active_hypotheses: [],
      unresolved_conflicts: [],
      decisions: [],
      failed_paths: [],
      blockers: [],
      open_questions: [],
      next_plan: ["Take the next action."],
      evidence_refs: [],
      directive_history: ["Continue."],
      ...overrides,
    },
    provenance: {
      kind: frontier.kind,
      model: "test/model",
      ...(confirmedBy ? { confirmed_by: confirmedBy } : {}),
    },
  };
}
