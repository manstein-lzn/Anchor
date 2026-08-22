import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, writeFile, readFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { beliefStaleness, compileContext, renderContext, verifyEvidence } from "../src/context.js";
import { AnchorRuntime } from "../src/runtime.js";
import { createState, StateStore } from "../src/state.js";

const sha256 = (buffer) => createHash("sha256").update(buffer).digest("hex");

test("beliefs older than the as_of threshold are marked stale at compile time", () => {
  const state = createState({
    goal: "freshness",
    beliefs: [
      { id: "a:old", text: "stale handover knowledge", kind: "finding", as_of: "2020-01-01", confidence: "high" },
      { id: "a:fresh", text: "recent finding", kind: "finding", as_of: new Date().toISOString(), confidence: "high" },
    ],
  });
  const envelope = compileContext(state, { purpose: "resume" });
  const old = envelope.cognition.find((belief) => belief.id === "a:old");
  const fresh = envelope.cognition.find((belief) => belief.id === "a:fresh");
  assert.match(old.stale_reason, /older than 30 days/);
  assert.equal(fresh.stale, undefined);
  assert.equal(envelope.belief_stats.stale, 1);
});

test("beliefs aged beyond the revision threshold are marked stale", () => {
  const belief = { id: "a:aged", text: "old conclusion", kind: "finding", established_rev: 0 };
  assert.match(beliefStaleness(belief, 500, { staleRevisions: 200 }), /aged 500 revisions/);
  assert.equal(beliefStaleness(belief, 100, { staleRevisions: 200 }), null);
});

test("renderContext surfaces stale markers and evidence-drift warnings", () => {
  const state = createState({
    goal: "drift",
    beliefs: [{ id: "a:old", text: "old claim", kind: "finding", as_of: "2020-01-01" }],
  });
  const envelope = compileContext(state, { purpose: "work" });
  envelope.evidence_check = { verified: 2, drifted: [{ path: "/tmp/x.json", expected: "deadbeef1234", actual: "unreadable/missing" }] };
  const rendered = renderContext(envelope);
  assert.match(rendered, /⚠stale\]/);
  assert.match(rendered, /EVIDENCE DRIFT WARNINGS/);
  assert.match(rendered, /verify before trusting dependent beliefs/);
});

test("verifyEvidence detects hash drift and uses mtime-keyed cache", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-"));
  const filePath = join(dir, "result.json");
  await writeFile(filePath, "v1");

  const good = createState({ goal: "g", evidence: [] });
  good.evidence = [{ path: filePath, sha256: sha256(Buffer.from("v1")), type: "experiment_result" }];
  const cache = new Map();

  const first = await verifyEvidence(good, { cache });
  assert.deepEqual(first, { verified: 1, drifted: [] });

  // File changes -> mtime key changes -> re-hashed -> drift reported.
  await writeFile(filePath, "v2 with different content");
  const second = await verifyEvidence(good, { cache });
  assert.equal(second.verified, 1);
  assert.equal(second.drifted.length, 1);
  assert.notEqual(second.drifted[0].actual, second.drifted[0].expected);

  // Unchanged file with a fresh cache verifies clean.
  await writeFile(filePath, "v1");
  const third = await verifyEvidence(good, { cache: new Map() });
  assert.deepEqual(third, { verified: 1, drifted: [] });

  // Missing files are drift too.
  good.evidence = [{ path: join(dir, "gone.json"), sha256: sha256(Buffer.from("x")) }];
  const missing = await verifyEvidence(good, { cache: new Map() });
  assert.equal(missing.drifted[0].actual, "unreadable/missing");
});

test("AnchorRuntime merges evidence_check into every compiled envelope", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-"));
  const store = new StateStore(join(dir, "state.json"));
  const artifactPath = join(dir, "artifact.txt");
  await writeFile(artifactPath, "stable content");
  await store.init({
    goal: "runtime freshness",
    evidence: [{ path: artifactPath, sha256: sha256(Buffer.from("WRONG")), type: "file_change" }],
  });

  const listeners = [];
  const session = {
    agent: {
      transformContext: undefined,
      subscribe(listener) { listeners.push(listener); return () => {}; },
    },
    dispose() {},
  };
  const runtime = new AnchorRuntime({ session, store, purpose: "work" });
  // Trigger a compile through the transform hook.
  await session.agent.transformContext([{ role: "user", content: [{ type: "text", text: "go" }] }]);
  assert.equal(runtime.lastContext.evidence_check.verified, 1);
  assert.equal(runtime.lastContext.evidence_check.drifted.length, 1);
  runtime.dispose();
});
