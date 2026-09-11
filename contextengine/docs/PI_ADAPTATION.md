# Pi Extension Boundary

Anchor is loaded by Pi from `src/extension.js`. It does not construct or wrap a
Pi runtime.

## Hooks

| Pi API | Anchor use |
|---|---|
| `session_start` | Restore session mode/State or ask once on a new session |
| `before_agent_start` | Apply Planning or blocked directive |
| `agent_settled` | Seal the complete proposing turn, then open review |
| `context` | Prepend current RecoveryView without trimming Pi messages |
| `session_before_compact` | Replace compact summary with Update |
| `registerTool` | Expose the structured `anchor_propose` boundary |
| `registerCommand` | Expose `/anchor` and `/update` |
| `appendEntry` | Persist session-owned mode and proposal audit entries |
| `setActiveTools` | Enforce read-only Planning and restore execution tools |

Pi remains responsible for model selection, streaming, project tool execution,
terminal UI, sessions, interruption, retry, and compaction thresholds. Anchor's
model request may attach one request-local submission function. It is not
registered through `pi.registerTool`, has no executor, and exists only so the
provider returns schema-constrained candidate arguments.

## Planning boundary

Normal mode removes Anchor's model tools and otherwise leaves Pi unchanged.
Planning enables only read-oriented inspection tools, `anchor_ask`, and
`anchor_propose`. A proposal remains a candidate until `agent_settled` proves
the full turn was stored. Review can accept, revise, or cancel; only accept
invokes one atomic Anchor initialization.

Mode entries are read from the complete Pi session and carry the owning session
identity. Tree navigation therefore does not change the mode, while copied
entries in a fork are ignored because the new session identity differs.

## Update boundary

Manual `/compact`, automatic threshold compact, overflow recovery, and `/update`
all reach the same hook in Active mode. Update calls the selected model directly
with exactly one mandatory, side-effect-free `anchor_submit_update` function and
no project tools. Anchor consumes the call arguments without executing a tool.
Failure returns `{ cancel: true }`; Pi retains the usable window and does not run
ordinary compact as fallback.
