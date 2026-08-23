# Anchor

Anchor is a state-driven agent runtime built on top of the Pi execution loop.
Its central rule is **State as Context**:

> The model does not carry the whole task history. Each invocation receives a
> context compiled from the current authoritative state and relevant evidence.

Anchor keeps Pi's useful, proven mechanics:

- model and provider adapters;
- streaming responses;
- tool execution;
- interruption and retry;
- session persistence and terminal interfaces.

Anchor changes the memory boundary. The long-lived object is the task state,
not an Agent identity or an ever-growing transcript.

```text
Durable State + Evidence
          |
   ContextEngine projection
          |
   short-lived invocation
          |
       Pi loop
          |
  Result + tool observations
          |
  validate, reduce, commit
          |
   next State revision
```

The repository contains the first minimal runtime implementation. It is
intentionally a local JSON StateStore and Pi adapter; production database and
MetaLoop adapters remain separate follow-up work.

## Documents

- [Architecture](docs/ARCHITECTURE.md): the complete State as Context model.
- [Principles](docs/PRINCIPLES.md): non-negotiable design rules.
- [Pi Adaptation](docs/PI_ADAPTATION.md): the deliberately small fork boundary.
- [Experiment](docs/EXPERIMENT.md): the first recovery experiment and its limits.
- [Roadmap](docs/ROADMAP.md): incremental implementation and verification.

## Quick start

```bash
npm install
npm test
anchor
```

`anchor` opens the same interactive terminal experience as Pi: streaming,
tools, slash commands, session switching, and keyboard controls. The State
Context adapter is active only at the model-facing boundary; Pi's normal
execution loop and UI remain in charge. Anchor's default State is stored under
Pi's user-level agent data directory and keyed by session ID, so normal use does
not create runtime files in the project workspace.

For a first task, pass the initial prompt exactly as with Pi:

```bash
anchor "inspect the repository and propose the next change"
```

The lower-level commands remain available for scripts and diagnostics:

```bash
anchor init --state /tmp/anchor-task-state.json "ship a feature"
anchor context --state /tmp/anchor-task-state.json --purpose resume
anchor run --state /tmp/anchor-task-state.json --goal "ship a feature" "inspect the repository"
```

## License

MIT. See [LICENSE](LICENSE).
