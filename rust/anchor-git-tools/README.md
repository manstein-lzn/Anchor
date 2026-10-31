# Git Plugin

`plugins/git` is a normal Anchor Plugin. Its Rust stdio MCP calls the native
`git` on the sandbox PATH; it does not embed Git or require Host/Kernel changes.
The executable and Plugin files can be installed into an existing Anchor
Library without rebuilding or restarting its Host. Reusing host SSH files
additionally requires the Host's operator-managed, Plugin-scoped tool grant
described below.

## Install and enable

Fresh source-checkout onboarding includes this Plugin through
`examples/starter.json`. An existing installation keeps its Library contents;
install the Plugin explicitly through the normal Library installer or Web UI.
For an operator-managed Library, build and stage a candidate:

```sh
cargo +stable build --manifest-path rust/Cargo.toml --locked \
  -p anchor-git-tools -p anchor-library --bins
git_candidate=$(mktemp -d)
cp -R plugins/git "$git_candidate/git"
mkdir "$git_candidate/git/bin"
install -m 0755 rust/target/debug/anchor-git-tools "$git_candidate/git/bin/"
rust/target/debug/anchor-library --root /path/to/library \
  install --directory "$git_candidate/git" --id git --review
rust/target/debug/anchor-library --root /path/to/library \
  install --directory "$git_candidate/git" --id git
```

Mount `git` on the AgentNode or Pilot Session through the existing Plugin
controls. An active persistent assistant reads an operator-saved Plugin change
on its next Turn, retaining its workspace. The host installation must provide
Git; SSH remotes also require OpenSSH. Language compilers and project dependencies
remain the deployment's existing development environment.

## Workspace and credentials

All servers expose `git_run` with native `args`, workspace-relative `cwd` and
`timeout_seconds` (default 120, maximum 600). `git` is always present for local
and anonymous operations; `ssh` and `https` appear only when their corresponding
configuration fields are complete; `host` uses an explicitly granted existing
SSH configuration. The tool uses the same `/workspace` as
`anchor_read`, `anchor_edit` and `anchor_run`. No repository lives in the Plugin
directory. Normal local Git commands also work through `anchor_run`.

Optional author name/email are provided through Git's author/committer
environment. Without them, configure identity in the workspace repository
using normal `git config user.name` and `git config user.email`.

Configure remote credentials through the existing Plugin settings:

- Host SSH: the operator registers the original SSH paths read-only for the
  Git Plugin, then sets `ssh_config` to the absolute config file path. OpenSSH
  retains its existing IdentityFile, known_hosts and host aliases. No key
  contents are copied into Plugin settings or the workspace. The `host`
  server uses strict host checking and batch mode. Encrypted keys requiring
  an interactive prompt or an ungranted agent socket cannot authenticate.
- SSH: unencrypted private key and verified `known_hosts` content. For the
  single-line settings field, encode the key with `base64 -w0 < /path/to/key`
  and paste the result; the configuration API also accepts the raw multiline
  key. The process
  uses a private temporary directory inside its sandbox, strict host checking
  and batch mode. It does not copy host SSH configuration into the workspace.
- HTTPS: URL, username and Token. A Git credential helper answers only for that
  HTTPS origin and optional repository path prefix. It does not persist a
  credential store or put the Token in remote URLs or process arguments.

Credentials belong to the Host's existing owner-bound Plugin configuration;
they are injected into the matching MCP process. General node shell commands
receive no Plugin credentials. Task networking is independently authorized;
local Git remains usable offline. Remote account permissions determine actual
repository/branch access: the Plugin accepts native Git arguments and is not a
separate per-command permission system.

For example, register `<Library>/tools/git-host-ssh/tool.json` as the operator:

```json
{
  "plugin": "git",
  "entrypoint": "/usr/bin/ssh",
  "imports": ["/home/operator/.ssh"]
}
```

Set `ssh_config` to `/home/operator/.ssh/config` using Plugin settings and
restart the Host after changing the deployment's tool registrations. Paths
outside that directory referenced by Include, IdentityFile, known_hosts or
ProxyCommand need their own explicit grants. The optional `plugin` field
scopes the existing tool environment to that Plugin's MCP launches, including
launches using a dependency view. General node shells and other Plugins do
not receive these paths. This is a deployment grant for nodes mounting Git,
not a per-repository ACL. Revocation requires removing the grant and restarting
the Host; changing a field does not revoke already-running processes.
Before rolling back to a Host without Plugin-scoped tool registrations, remove
the scoped registrations first. Older Hosts ignore the unknown `plugin` field
and would interpret the remaining registration as a shared tool environment.

Results preserve the exit code and bound each output stream to 64 KiB, with
`truncated` reporting omission. Configured credential literals are redacted from
returned output. Nonzero exits, timeouts and cancellations are reported as tool
errors. Cancellation terminates the command's process group; after interruption,
the Agent must inspect actual repository/remote state before continuing.

## Verification boundary

The crate tests use actual local Git repositories. The native Plugin scenario
in `anchor-runner-host/tests/native_plugins.rs` executes Host, Runner, Goose ACP,
Plugin discovery, Bubblewrap, Git and node edit/test tools, with a deterministic
local model Provider. It checks a local clone/edit/test/commit/push cycle and the
packaged HTTPS credential helper. These checks do not establish access to a
real private Git service or real model task quality. The same scenario also
checks native SSH config resolution through a Plugin-scoped read-only grant.
