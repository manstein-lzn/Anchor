# Native Docmost attachment tool

The binary defaults to an rmcp 2.2 stdio MCP server. It preserves the
`upload_page_image` name, description, input schema, multipart field order, and
attachment JSON of `plugins/docmost/upload_server.py`. The original plugin is
not modified.

## Deployment

```sh
anchor-docmost-tools
anchor-docmost-tools --endpoint https://docmost.example/api/files/upload
anchor-docmost-tools package-plugin /existing/parent/native-docmost
```

Only `DOCMOST_API_KEY` is read from the environment. There is no dotenv loading,
endpoint environment override, root CLI override, or endpoint fallback. The
default upload endpoint is `https://docmost.cwise.dev/api/files/upload`.
`--endpoint` is an explicit operator deployment/fixture setting; it accepts
absolute HTTP(S) URLs without userinfo or fragments. Redirects and retries are
disabled, including reqwest transport retries. A failed upload can have an
unknown external outcome; it is not automatically resent. HTTP error bodies,
URLs, authorization values, and JSON parser details are not returned in errors.

Stdio reads ordinary files only under `/in/publish/assets`. Directory components
and the final file are opened without following symlinks, relative to directory
descriptors. Parent traversal, other roots, directories, FIFOs, sockets, and
symlinks (including links within the permitted root) are rejected. Reads are
bounded to 20 MiB plus one byte; empty and oversized inputs are rejected. MIME
selection follows the original case-insensitive `.svg`, `.png`, `.jpg`, `.jpeg`,
and `.webp` extension mapping, not image-content decoding. Control characters,
quotes, and backslashes in filenames are rejected to prevent multipart header
injection. UUIDs are checked without changing the submitted identifier text.

`package-plugin` atomically publishes a new destination using a no-replace
rename relative to a retained parent directory descriptor. Staging creation,
file writes, publication, and cleanup stay anchored to that descriptor even if
the parent path is replaced. Parent identity and symlink-free ancestors are
checked before and after publication. The executable is opened without following
symlinks and checked against the running image; inode, size, timestamps, and mode
are checked again before and after publication. Copied length and SHA-256 are
verified from the staged descriptor. Packaging requires Linux `/proc/self/exe`
and `/proc/self/fd`. Existing destinations and symlinks are refused; errors do not leave a
partial destination presented as a completed plugin. The only files are
`plugin.json`, the byte-identical `skills/docmost/SKILL.md`, and
`bin/anchor-docmost-tools` copied from `current_exe`. The remote Docmost MCP URL,
API-key environment references, and all other manifest fields are retained;
only the attachments command/args/cwd change. No Python source, dotenv file,
or environment credentials are copied.

## Library and fixtures

`Uploader::new(Config::new(key))` exposes the same upload behavior.
`Config::with_endpoint(url)` and `Config::with_upload_root(root)` explicitly
inject an HTTP endpoint and an existing absolute non-symlink directory for
trusted library callers and local fixtures. These are not tool arguments and do
not expand stdio's fixed input-root authorization. `AttachmentServer` implements
rmcp's `ServerHandler`; main uses `ServiceExt` and the native stdio transport.

From the worktree root, after the integration owner updates the shared workspace
lockfile:

```sh
export CARGO_TARGET_DIR=/tmp/anchor-rust-platform-HEW1JK/target-docmost
export CARGO_BUILD_JOBS=2
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-docmost-tools --all-targets --locked --offline -- --test-threads=2
cargo +stable clippy --manifest-path rust/Cargo.toml -p anchor-docmost-tools --all-targets --locked --offline -- -D warnings
cargo +1.92.0 fmt --manifest-path rust/Cargo.toml -p anchor-docmost-tools -- --check
```

For package-only regression, use `test --lib --test package` instead of
`test --all-targets`. Validation logs belong under `/tmp`, not in the plugin
crate or package; temporary standalone manifests/lockfiles are not delivered.

Tests use loopback HTTP fixtures and check exact multipart bytes, authorization,
JSON, failure redaction/no-resend, path/UUID/MIME/size rejection, and package
contents/collision behavior. Real stdio success tests require `/usr/bin/bwrap`
and usable user namespaces to mount a fixture read-only at the fixed asset root
inside a private tmpfs filesystem. Tests do not create `/in` on the host, load
credentials, contact production Docmost, or call a model. Component tests do not
claim real Host/Graph composition or production upload acceptance.
