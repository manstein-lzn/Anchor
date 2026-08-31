# Anchor

Anchor is a Pi Extension for stable long-running work. It creates one
user-confirmed Task and turns Pi compaction into versioned cognition Checkpoints.

```text
new Pi session
      |
Anchor / Normal Pi choice
      |
read-only Anchor Planning (when selected)
      |
structured proposal + user confirmation
      |
Task + Checkpoint 0
      |
normal Pi execution
      |
Checkpoint Update at each compact boundary
```

## Requirements

- Pi `0.84.2` or newer;
- Node.js `22.19` or newer;
- Python `3.10` or newer.

The Python state core is included in this package and has no third-party Python
dependencies.

## Install

From this checkout:

```bash
pi install /absolute/path/to/Anchor
```

For only the current project:

```bash
pi install -l /absolute/path/to/Anchor
```

After publication, the same package can be installed from its npm or Git source
with Pi's normal `pi install` command.

## Use

```bash
cd <project-directory>
pi
```

An empty interactive session asks once whether to use Anchor or Normal Pi.
Normal Pi remains unchanged; `/anchor start` can activate Anchor later.
Non-interactive sessions default to Normal unless started with `--anchor`.

Anchor Planning is read-only. The agent asks focused questions and inspects the
project until it can propose a complete goal, acceptance criteria, scope,
constraints, verification, risks, execution plan, and initial cognition.
`anchor_propose` ends its turn before a separate review offers Accept, Revise,
or Cancel. Nothing becomes durable truth until the user accepts the sealed
proposal.

After confirmation Anchor creates its database under Pi's agent data directory,
records the Pi session identity on the new Task, restores normal tools, and begins
execution. It does not write runtime data into the project. Compact, automatic
threshold compact, and `/update` all run the dedicated Update Agent over only
the Episode selected by Pi's compaction preparation. The resulting Checkpoint
records its source frontier, parent hash, provenance, and complete current
cognition.

Useful commands:

```text
/anchor start     Enter read-only Planning from Normal Pi
/anchor status    Show the current mode or Task
/anchor review    Reopen review for a sealed proposal
/anchor cancel    Return from Planning to Normal Pi
/anchor recall checkpoint:<version>:item:<id>
/update           Run Update explicitly for an Active Anchor
```

Active Agents also have `anchor_recall` for the same exact Checkpoint item
lookup. Recall is read-only: its result enters Pi's current work window and is
eligible for the next Update; it does not mutate the authoritative Checkpoint.

Pi's `/resume` restores the same session Task automatically. Tree navigation
does not roll back Anchor State. A fork has a new Pi session identity, so it
chooses Anchor or Normal again and never inherits writable State.

Two Pi host limitations remain. Pi validates the selected model and its
credentials before invoking Extension compact hooks, so recovery still needs a
usable model configuration even when Anchor can replay a receipt without a
provider request. Pi also has no per-Extension active-tool ownership API;
leaving Planning restores its saved tool list and may overwrite tool changes
made concurrently by another Extension.

`--anchor-state <path>` selects a non-default database location for the current
session. `ANCHOR_ENABLED=1` and `ANCHOR_STATE_PATH` are the equivalent environment
variables. State created by the former pre-release governance schema is not
migrated; archive it and create a new Anchor.

## Verify

```bash
npm install
npm test
npm run check
```

## License

MIT. See [LICENSE](LICENSE).
