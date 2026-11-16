# Codex project execution pool

An ordinary Anchor Streamable HTTP MCP Plugin. This directory contains only a
manifest, documentation and a shared project-manager Skill; it does not bundle
Codex, launch a process, or add a second agent loop. The separately operated
`anchor-codex-bridge` owns local project access, Codex app-server connections and
managed worktrees. See the [bridge contract](../../docs/codex-project-manager.md).

## Explicit setup

1. An operator builds and starts the Bridge under the local user whose Codex
   configuration should be used, with a bearer token and explicit `--allow-root`
   grants. The default listener is `127.0.0.1:8091`; installing this Plugin does
   not start it or expose it to other machines.
2. Import this directory into the Plugin Library as `codex`, or explicitly run:

   ```sh
   rust/target/debug/anchor-library --root /path/to/library \
     install --directory plugins/codex --id codex
   ```

3. Save the owner-bound connection in the Plugin settings:

   | Field | Meaning |
   | --- | --- |
   | `bridge_url` | Base URL, default `http://127.0.0.1:8091`; `/mcp` is appended. Use no trailing slash. |
   | `api_key` | Required Bridge bearer token, entered through the credential control, never a Graph, Skill, chat message or committed file. |
   | `project_scope` | Optional relative project/group subdirectory such as `Anchor` or `marketdata/simai`; omitted or empty means all Bridge-authorized roots. |

   The server uses `bearer_token_env_var: ANCHOR_CODEX_BRIDGE_TOKEN` and
   `X-Codex-Project-Scope: ${ANCHOR_CODEX_PROJECT_SCOPE:-}`. These environment
   names are Host configuration selectors, not instructions to populate the
   model's shell. Scope only narrows the Bridge whitelist; paths remain subject
   to Bridge canonicalization. This is one default connection per owner, shared
   by that owner's uses of this Plugin, not an independent per-robot setting.
   A project named in Graph instructions is not a security boundary.
4. Import the [watch worker](../../examples/graphs/codex-watch-worker.json) and
   either the [Feishu manager](../../examples/graphs/feishu-codex-project-manager.json)
   or [WeCom manager](../../examples/graphs/wecom-codex-project-manager.json).
   These are separate persistent assistants; do not put both channels on one node
   or rebind an existing robot's saved conversation history to another channel.
5. In the manager node, explicitly review and save the selected
   `codex-watch-worker` Graph under `graph-tasks`, including its approved `codex`
   and `network` dependency closure. This operator save creates the authorization
   receipt; JSON alone does not. No Graph creation, plugin development or command
   capability is requested by these examples. Network mode is `auto`, required
   by the HTTP MCP connection and still subject to Host/Session grants. The
   client connects from the Host, not the node's isolated loopback.
6. Configure members, credentials and enabled state for the chosen Graph's own
   robot following the [Feishu](../../docs/feishu-assistant.md) or
   [WeCom](../../docs/wecom-assistant.md) contract. No configuration, permission,
   deployment, connection, task or message is created automatically.

## Tools and observation contract

Server `codex` publishes raw tools `projects`, `spawn`, `board`, `inspect`,
`steer_or_resume`, `control`, `btw`, `models`, `history`, `session`,
`native_discover`, `native_call`, `native_events` and `native_respond`. Anchor
combines Plugin ID and server name, then the raw tool name, for example
`codex-codex_session`.
Discover the actual tool inventory and schemas; the manifest does not duplicate
the Bridge's tool schemas. The shared [Skill](skills/codex/SKILL.md) covers
project-level delegation, separate worktrees for parallel writes, bounded initial
waits, native session lifecycle and history, exact-request approvals and
evidence-based acceptance. Loading is separate from starting work; historical
side questions do not require loading or sending a prompt to the parent.
Native discovery uses the installed Codex schemas; allowed project methods are
callable without another wrapper. Host-wide or unreviewed authority remains
blocked. Native queue/add may start work immediately when idle. Events are
bounded observations, not durable recovery logs or automatic channel delivery.

After the user authorizes monitoring, the manager saves a real Graph Tasks
polling schedule and retains its returned ID. The worker's input is:

```json
{
  "targets": [
    {"project": "Anchor", "thread_id": "actual-thread-id-returned-by-spawn"}
  ]
}
```

Replace the illustrative values with actual tool results. The worker performs
one observation, writes `codex-watch.json` with `observations` and `issues`, and
selects that file as its terminal Artifact. It has no Session operations, channel
Plugin or task-delegation capability. Read-only watching is its instruction
contract, not a separate tool-level ACL: the ordinary `codex` Plugin still exposes
all six tools. The Bridge's path and approval enforcement remains authoritative.

The original manager receives the Graph Tasks result, distinguishes watcher
completion from actual Codex completion, and uses its own channel's authorized
notification tool for requested status changes, blockers and verified outcomes.
Task-result `session.reply` means handled, not sent; require the real confirmed
receipt. Missing authorization, schedule-save failure, unavailable notifications
or unknown delivery must be reported rather than silently treated as success.
After verified terminal outcomes, remove or narrow only the corresponding saved
schedule. Cancelling monitoring does not stop Codex work.

## Verification boundary

`rust/anchor-graph-host/tests/codex_plugin.rs` checks the shipped package with
`FilePluginCatalog`, owner-supplied bearer/header expansion with scope absent,
empty and populated, and all three examples with `GraphSnapshot`. These checks
do not launch the Bridge, invoke a model, create a live schedule, grant channel
permissions or prove an external delivery. Bridge protocol tests and real
provider/channel acceptance remain separate evidence.
