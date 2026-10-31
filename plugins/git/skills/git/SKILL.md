---
name: git
description: Maintain Git repositories in the current Agent workspace with native Git.
---

Use the Git Plugin's `git_run` tool for native Git operations. Discover its actual
tool name and schema through `anchor_tools` and `anchor_tool_detail`. Pass Git
arguments as an array without a shell or a leading `git` executable. `cwd` is an
existing directory relative to `/workspace`, default `.`.

The `git` server is always available for local and unauthenticated operations.
The `ssh` server appears when both SSH fields are configured; the `https` server
appears when all three HTTPS fields are configured. The `host` server appears
when the operator has granted host SSH paths and configured `ssh_config`; use
`git-host_git_run` for that identity. Each exposes `git_run` with
the same schema and workspace. Select the server matching the remote's transport
for authenticated operations. Missing credentials never disable the local tool.
Local development works without task networking. Remote clone/fetch/push also
requires the current task's existing network authorization (`network` Plugin
and the node/session permission); this Plugin does not grant connectivity.

The Plugin runs the deployment's `git` from PATH. It shares `/workspace` with
`anchor_read`, `anchor_edit` and `anchor_run`: clone once into a subdirectory,
edit and test those same files, and continue in that directory on later turns.
It does not create a separate repository, developer environment or Agent loop.

Examples of `git_run` arguments:

```json
{"args":["clone","git@example.org:team/project.git","project"]}
{"cwd":"project","args":["status","--short","--branch"]}
{"cwd":"project","args":["switch","-c","fix/issue"]}
{"cwd":"project","args":["diff","--stat"]}
{"cwd":"project","args":["add","--","src/module.py"]}
{"cwd":"project","args":["commit","-m","Fix reported issue"]}
{"cwd":"project","args":["push","-u","origin","fix/issue"],"timeout_seconds":300}
```

Inspect status, branch, remotes and existing work before changing a repository.
Run actual project checks with `anchor_run`; toolchains and dependencies come
from the current sandbox environment. A successful commit is not proof that
tests passed. Preserve existing work. Do not reset, clean, force push, delete
branches or overwrite changes without the user's authorization. Push and other
remote writes follow the user's standing authorization; do not repeatedly ask
for actions already authorized.

Local Git also works through `anchor_run`, including shell pipelines. Use
`git_run` for authenticated remote operations: credentials configured in the
Plugin are injected only into its MCP process, not into general shell tools.
SSH requires an unencrypted private key and verified `known_hosts` content in
the Plugin settings, or the `host` server's explicitly granted original SSH
configuration. In host mode OpenSSH reads the existing config, identity and
known_hosts through read-only mounts; do not copy their contents into the
workspace or tool output. A configured path alone does not grant filesystem
access. The settings form accepts a separate Plugin key as single-line Base64
(operators can also supply the raw multiline key through the configuration API).
HTTPS requires a URL, username and Token; the credential
helper only answers for that HTTPS origin and optional repository path prefix.
Never put credentials in tool arguments, remote URLs, chat or repository files.
Host SSH files and host Git configuration are not implicitly inherited.
`Host key verification failed` identifies a trust-file/host-key problem; it
does not prove that a private key is absent or the account lacks repository
access. Distinguish it from `Permission denied (publickey)` and connection
errors using the actual stderr. Inspect the available authenticated server
before proposing a different transport or asking for credentials again.
Configured credentials grant the remote account's actual access; mounting the
Plugin does not independently enforce per-branch or read-only restrictions.

The result includes `status`, `exit_code`, `stdout`, `stderr` and `truncated`.
Nonzero exit, timeout and cancellation are errors, not completed work. Output
is bounded; use narrower diffs/logs when truncated. After an interrupted commit
or push, inspect local and remote state before retrying. Git commands serialize
within each server connection; avoid concurrent changes to the same repo across
the local/host/SSH/HTTPS tools or shell commands.
