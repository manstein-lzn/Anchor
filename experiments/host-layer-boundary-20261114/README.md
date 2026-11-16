# Host tool dependency boundary repair

Date: 2026-11-14. Source repair only; no production service, credentials,
directory grants, persisted formats, or deployed sessions are changed.

Publication boundary: this summary and the two compact identity/result JSON
files are checked in. `changes.patch` and all raw log filenames below refer to
the original local evidence directory, not this public copy; see the
[local evidence rules](../../docs/development.md#文档归属). No Provider message
bodies or production inventory are included, and old hashes do not sign this checkout.

## Scope

The reported four violations were one service-to-application reference in
`codex_bridge_tools.rs`, two upward references in `project_tools.rs`, and one
application type import in Goose's channel confirmation adapter.

- Assistant authorization now lives in `application/assistants.rs`, replacing
  the two authorization implementations removed from the runtime tool modules.
- The existing tool composition root injects invocation-bound authorization
  callbacks and constructs project `NodeTools`. Project execution still checks
  current authorization on every call and keeps the directory lease until a
  command has actually stopped, even when its caller is dropped.
- `channel_platform.rs` replaces the enum formerly declared in
  `application/channel_management.rs`. All consumers use that single neutral
  type; there is no compatibility alias or serialization migration.
- Neither fence classification nor per-file exception limits were changed.

`changes.patch` contains only this task's source edits relative to the dirty
working-tree snapshot taken before editing, not unrelated parallel edits.
`source-identities.json` identifies those fourteen files before and after.

## Verification

Commands use `CARGO_TARGET_DIR=/tmp/anchor-dreaming-rollout-target` and the
stable toolchain. Logs are retained in this directory. The full unit run used
the default disk-backed temporary directory; the Goose fixture explicitly used
the existing user runtime tmpfs via `XDG_RUNTIME_DIR=/run/user/1001`.

| Check | Result |
| --- | --- |
| Four Host fences | 7 passed; no baseline increase or new exception |
| Project access, Bridge, channel management, tool grants | 31 passed |
| An extra `api::tests::wecom_channel_events::` filter | Matched zero tests; not counted as coverage |
| Changed-file rustfmt check | All fourteen files pass |
| Whole-crate `cargo fmt --check` | Fails in eight files outside this task's edit scope; see `fmt.log` |
| Tracked-text wording check | Passed |
| Full Host unit suite | 1,339 passed, 0 failed, 22 default-ignored |
| Strict all-target Host Clippy | Passed with `-D warnings` |
| Fixed Goose project-workspace integration | 1 passed, using a deterministic local provider and the pinned real Goose binary |

Exact commands beyond the filtered runs in `targeted-tests.log`:

```sh
scripts/test.sh -p anchor-runner-host --test layer_fence --test run_access_fence --test configuration_fence --test plugin_library_fence
scripts/test.sh -p anchor-runner-host --bin anchor-runner-host
cargo +stable clippy --manifest-path rust/Cargo.toml -p anchor-runner-host --all-targets -- -D warnings
cargo +stable fmt --manifest-path rust/Cargo.toml -p anchor-runner-host -- --check
scripts/check-sensitive-wording.sh
cargo +stable build --manifest-path rust/Cargo.toml -p anchor-runner-host -p anchor-sandbox-bwrap --bins
ANCHOR_GOOSE_BINARY="$PWD/.local/project-direct-20261113/fixture-bin/goose" \
ANCHOR_GOOSE_LOCAL_NETWORK=1 \
ANCHOR_GOOSE_RELAY_BINARY="$CARGO_TARGET_DIR/debug/anchor-net-relay" \
scripts/test.sh -p anchor-runner-host --test goose_project_workspace -- --ignored --test-threads=1 --nocapture
```

The changed-file formatting command was `rustfmt +stable --edition 2024
--config skip_children=true --check`, applied to the fourteen paths listed in
`source-identities.json`. Whole-crate formatting differences are confined to
`api/dreaming/tests.rs`, `application/dreaming/{cycle,evaluation,native}.rs`,
`application/plugins.rs`, `application/run_access/admission.rs`,
`application/schedule_subscriptions/delivery.rs`, and `node_tools.rs`.
They were not reformatted as part of this task.

`goose-project-summary.json` projects the fixture's actual checks and binary
identities without duplicating provider message bodies. It confirms direct
project read/edit/run, exclusion of protected directories, preservation of the
channel Session and Manager workspace, and no use of Codex. This is a local
deterministic-provider acceptance test, not a real-model-provider run.

The new Bridge wrapper test checks that a grant valid at preparation but
withdrawn before execution is rejected for both status and start, before any
service-controller operation. Existing project tests cover stopped Turns,
identity and directory changes, busy leases, and caller cancellation.

This does not claim a real-model-provider run, live Feishu acceptance, or a
production deployment.
