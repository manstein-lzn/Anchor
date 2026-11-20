# Library and release closeout - 2026-11-20

Tested source: `985a9d1`. This follows the [integrated source regression](README.md)
on `75cdba6`. Only the regression command, fixtures and regression guide changed;
the runtime, network authorization and persistence contracts did not change.

| Check | Observed result |
| --- | --- |
| Library update in an admitted Run | `goose_library`: 1 passed, 0 failed. The bundle references `demo` and the normalized `network:auto` policy; only `demo` is bound to the Run. After replacement the same Run discovers the new Skill, reads it from a read-only mount and delivers its exact bytes. The bundle has no copied Plugin resources and the admitted bindings do not change. |
| Candidate tooling | 13 targeted validator/driver tests and 6 CLI tests passed. Strict Clippy on the affected tooling and fixture targets, workspace formatting, and wording checks passed. |
| Official standalone Graph candidate | `anchor-devtools regression candidate`: one scenario passed, none ignored. The extracted release Host and fixed Goose used two deterministic model requests before stop, two after resume, and none after a completed restart. The same Run and native Session completed with one workspace write, read-only downstream input, and exact Artifact HTTP readback. All 153 inventoried files were verified, including Web and the official scholarly and WeCom binaries; no business tools were called. |
| Complete format 5 platform | Archive SHA256 and fresh `scripts/install-platform.sh` admission passed. Installed preflight passed. The installed read-only manifest matched the builder report; all 182 inventoried file bytes and modes matched, including 16 executable identities. The archive has no default Graph bundle. |
| Installed startup and restart | Two `anchor-platform start --listen 127.0.0.1:49157` launches served health, readiness, index, JS and CSS from the installed release. Six Plugins were listed with the expected digests (`network`, `graph-tasks`, `feishu`, `wecom`, `git`, `anchor-steward`). The installation started without a configured model, business Graphs or plans. A local Op Graph completed, its Artifact was read by HTTP, and the same completed Run and Artifact survived restart without a second invocation. Both test processes exited cleanly. |

The old official Plugin fixture failed bundle admission because it included the
previous manifest's Plugin content fields rather than ID-only references. The
first official rerun passed the extracted fixture but failed the outer evidence
check on an obsolete Artifact storage path. Its Cargo test also rebuilt the
Host with different bytes from the initial release build. The final check reads
Artifact bytes from the public endpoint and tests the release binaries frozen
before Cargo compiles the harness. It does not accept a passing inner test alone.

| Retained artifact | SHA256 |
| --- | --- |
| Standalone format 1 archive | `e381aedee627fb78bd3a1d3599b19ac15cc6f0d88e9075f73716b8dee22ed81a` |
| Complete format 5 archive | `19e373be1e548c0e146048e7ea9be312fb3170b8c9a040b057d1396ff9c570c1` |
| Installed format 5 manifest | `df1fa459b0d4b5529e2aac86cf94c38dcdd831feabe8cf17c697d54a50e54db9` |
| Exact release Host in both packages | `a3619bc62c23a9bfe126e8d37eb681f5ce72877f53fbce20e5c50a331713b09d` |

Raw evidence is local, not part of the clone, under
`.local/source-closeout-20261120.vim0cPxR/release-closeout/`. Retained entry points:
`library.log`, `graph-candidate-final/report.json` (logs, Provider and archive),
and `platform/{build,install,preflight,smoke,shared-bytes}.json`. The complete
archive is `platform/anchor-platform.tar.gz`. The shared-byte check also matched
Goose, both sandbox binaries, the WeCom gateway, and every Web file between the
packages. The fixed uv, Rustup, pixi and lark-cli inputs were checked against
their pins; no previous Host or Web build was used for this candidate.

No production data, provider, channel sends, push or cutover was used for this
follow-up. The primary worktree was unchanged. Fresh private installation does
not verify upgrades, new DEB/RPM transactions, target-system service behavior
or real morning-report delivery on the candidate. The existing live receipts
are the separate observation in the [baseline record](README.md).
