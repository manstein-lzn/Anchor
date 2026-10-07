# Rust Library installer

Operator-only Plugin installation. The root is the Library directory containing
`plugins/`, not the Anchor data root. Installation does not execute Plugin code,
run Python, grant filesystem/network permissions, authorize OAuth, or modify
existing Graph bundles.

```sh
anchor-library --root /operator/test-library install \
  --source https://github.com/owner/repo/tree/main/plugins/example
anchor-library --root /operator/test-library install \
  --directory /operator/fixture --id example --replace-existing
```

The library API provides `Library::new`, `install`, `install_with_checkout` and
`install_directory`. `Checkout: Send + Sync` receives only validated
`GithubSource`; production HTTP adapters must not expose either custom Checkout
or local-directory installation. `InstallOutcome` contains the installed ID,
FilePluginCatalog content digest, and directory. Public error Display messages
exclude source URLs, Git stderr, environment values, and manifest contents.

## Validation And Publication

- Sources must be HTTPS `github.com/owner/repo/tree/ref/path` URLs, without
  credentials, ports, query, fragment, encoded paths, traversal, or Git options.
  Git uses direct shallow/sparse argv, no shell, no host Git configuration or
  credential helpers, and no redirects. Clone is bounded to 180 seconds and
  sparse checkout to 60 seconds; timeout kills/reaps the process group.
- Root `plugin.json` takes precedence over `.codex-plugin/plugin.json`.
  Installation flattens the selected manifest, removes the top-level
  `.codex-plugin` directory, and excludes `.git` entries. No source provenance
  or credentials are added to the installed bundle or result.
- No-follow ancestor walks and source-tree checks reject symlinks and special
  files, including ignored metadata. Source reads use no-follow descriptors.
  Staged resources are validated by the actual FilePluginCatalog, with
  environment expansion disabled.
- A blocking `flock` on `plugins/.install.lock` serializes cooperating installers
  across processes and is held through checkout, validation, and publication.
  Owner exit releases the lease; the lock file is never unlinked.
  `catalog_read_guard` gives Graph resource freezing a nonblocking shared lease on
  the same file; an independent installer in progress returns `CatalogBusy`
  instead of creating a mixed resource snapshot. Ordinary catalog readers and
  noncooperating host filesystem edits are not coordinated.
- A private temporary directory on the same filesystem holds staging. Files and
  directories are fsynced before publication. Linux `renameat2(NOREPLACE)` creates
  new installs; `EXCHANGE` atomically swaps replacements without a missing-plugin
  window. Existing Plugins are not replaced unless explicitly requested.
  Unsupported atomic-rename filesystems fail closed without a weaker fallback.
- Publication fsync failure attempts an atomic rollback and fsync. If rollback
  or its durability cannot be confirmed, `PublicationUncertain` preserves the
  private transaction for operator inspection. It is a storage failure, not
  invalid input or OAuth approval. No recovery journal or new engine is added.

## Evidence And Limits

Crate tests use deterministic Checkout, local directories, actual catalog and
bundle loading, CLI subprocesses, cross-process leases, owner termination, and
injected publication fsync failures. They check malformed replacements leave the
old install unchanged and that replacement leaves an independent frozen Graph
bundle intact while bundle drift is rejected. No live GitHub success, real model,
OAuth, external send, actual power-loss durability, or Host HTTP execution is
claimed by this crate's tests.

There are currently **no total-byte, per-file-size, file-count, or depth limits**
on Git checkout or source trees. The time limits are not disk, memory, or stack
quotas; a malicious repository can exhaust these resources. Use only
operator-reviewed sources and an appropriately isolated/quota-controlled test
Library until resource limits are added. Library paths and source directories
are operator-managed; hostile concurrent root/ancestor renames are not supported.
Killed processes can leave private staging directories requiring inspection and
cleanup, but cannot expose partially copied resources as an installed Plugin.

## OAuth Foundation

OAuthBinding isolates an authorization by provider, Plugin, MCP server, and
owner. FileOAuthTokenStore stores authorization metadata and access/refresh
tokens below the Library root's private oauth/ directory. Records are written
atomically with mode 0600; binding directories and refresh locks use 0700/0600.
OAuthClient accepts an injected OAuthRefreshTransport, so an expired
authorization refreshes while a per-binding file lock is held. A single
concurrent caller wins the refresh; provider errors, malformed responses,
missing refresh tokens, and explicit revocation remove the local authorization
and return no token. Secret wrappers redact Debug and Display.

This is an operator/Host wiring foundation, not a production authorization
flow. It does not open a browser, implement a callback server, discover MCP
metadata, or connect to a real OAuth provider. The future Host owns consent,
client registration, redirect/callback validation, and the concrete HTTPS
transport; it should pass only validated provider responses into this crate.
Tests use an injected deterministic transport and never use production
credentials or external providers.
