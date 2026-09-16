# Graphs from the runtime before `9a7497f`

Records of the previous design, kept for the same reason the top-level documents are: the product
intent may still be a backlog even though the implementation is not in this branch.

**They do not load.** They are written against a schema this runtime does not have — node types
(`artifact`, `loop`, `verifier`, `human_task`), dotted `agent_ref` references, `input_mapping`,
`progress_signal`, and, for the bundle, an export with `bundle_version`, `content_hash`, `triggers`
and required verifiers. Nothing in `src/` reads any of it.

They are here rather than in `examples/graphs/` so that everything in that directory is a graph this
runtime can run, which `tests/test_examples.py` checks.

The academic flow they describe has been rewritten against the current schema: `academic.json` and
`academic-simple.json` are that rewrite, role for role — `plan`/`gather`/`write`/`review`/`report`
became `planner`/`gatherer`/`writer`/`reviewer`/`reporter`, and the `coverage` and `check` loop nodes
became the gatherer's campaign and the reviewer's routing. The one role with no counterpart is
`needs_input`, a human task; this runtime has no human gate and says so in its README.

`academic-research.README.md` is documentation for that design, not for this one: it names
`alembic`, worker services and `ANCHOR_DATABASE_URL`, none of which exist here.
