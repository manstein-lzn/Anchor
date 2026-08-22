import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { compileContext, renderContext } from "../src/context.js";
import { StateStore, createState } from "../src/state.js";

const BELIEF = {
  id: "core:metric-basis",
  text: "Rank-head P@1 0.93 was a metric sign-inversion artifact; honest basis is pw 0.8126.",
  kind: "negative_result",
  scope: "workstream:core_tenset",
  confidence: "high",
  source: "projects/statetune_core/NOW.md",
};

test("StateStore accepts and normalizes beliefs at init", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-"));
  const store = new StateStore(join(dir, "state.json"));
  const state = await store.init({ goal: "align TGraph", beliefs: [BELIEF] });
  assert.equal(state.beliefs.length, 1);
  assert.equal(state.beliefs[0].status, "active");
  assert.equal(state.beliefs[0].scope, "workstream:core_tenset");
});

test("applyBeliefOps adds, confirms and supersedes with revision checks", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-"));
  const store = new StateStore(join(dir, "state.json"));
  const initial = await store.init({ goal: "evolve cognition", beliefs: [BELIEF] });
  await store.applyBeliefOps([
    { op: "add", belief: { ...BELIEF, id: "core:new-protocol", text: "OrderScale protocol is the validated A-protocol." , kind: "finding" } },
    { op: "confirm", id: "core:new-protocol" },
    { op: "supersede", id: "core:metric-basis", by: "core:new-protocol" },
  ], { expectedRevision: initial.revision });
  const state = await store.read();
  const old = state.beliefs.find((belief) => belief.id === "core:metric-basis");
  const added = state.beliefs.find((belief) => belief.id === "core:new-protocol");
  assert.equal(old.status, "superseded");
  assert.equal(old.superseded_by, "core:new-protocol");
  assert.equal(added.status, "confirmed");
  assert.equal(added.established_rev, initial.revision + 1);
  await assert.rejects(() => store.applyBeliefOps([{ op: "confirm", id: "missing" }]), /unknown belief id/);
  await assert.rejects(() => store.applyBeliefOps([{ op: "add", belief: { ...BELIEF, id: "core:new-protocol" } }]), /duplicate belief id/);
});

test("normalizeState rejects malformed beliefs", () => {
  assert.throws(() => createState({ goal: "g", beliefs: [{ text: "no id" }] }), /\.id is required/);
  assert.throws(() => createState({ goal: "g", beliefs: [{ id: "a" }] }), /\.text is required/);
  assert.throws(() => createState({ goal: "g", beliefs: [{ id: "a", text: "t", status: "vibes" }] }), /status must be one of/);
  assert.throws(() => createState({ goal: "g", beliefs: [{ id: "a", text: "t" }, { id: "a", text: "dup" }] }), /duplicate belief id/);
});

test("ContextEngine leads with active cognition and hides superseded beliefs", () => {
  const state = createState({
    goal: "continue core work",
    beliefs: [
      BELIEF,
      { ...BELIEF, id: "core:stale", status: "refuted", text: "old wrong claim" },
    ],
  });
  const envelope = compileContext(state, { purpose: "resume" });
  assert.equal(envelope.cognition.length, 1);
  assert.deepEqual(envelope.belief_stats, { total: 2, active: 1, stale: 0, superseded: 0, refuted: 1 });
  const rendered = renderContext(envelope);
  assert.match(rendered, /CURRENT COGNITION/);
  assert.match(rendered, /\[negative_result\/active\]/);
  assert.doesNotMatch(rendered, /old wrong claim/);
});

test("amend revises belief fields with revision provenance", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-"));
  const store = new StateStore(join(dir, "state.json"));
  const initial = await store.init({ goal: "amend flow", beliefs: [BELIEF] });
  const next = await store.applyBeliefOps([
    { op: "amend", id: "core:metric-basis", set: { confidence: "medium", scope: "workstream:core_unified" } },
  ], { expectedRevision: initial.revision });
  const belief = next.beliefs[0];
  assert.equal(belief.confidence, "medium");
  assert.equal(belief.scope, "workstream:core_unified");
  assert.equal(belief.revised_rev, next.revision);
  assert.equal(belief.text, BELIEF.text);
  await assert.rejects(() => store.applyBeliefOps([{ op: "amend", id: "core:metric-basis", set: { id: "x" } }]), /cannot be amended/);
  await assert.rejects(() => store.applyBeliefOps([{ op: "amend", id: "core:metric-basis", set: { status: "vibes" } }]), /status must be one of/);
});
