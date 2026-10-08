# anchor-scholarly: Rust-native Literature CLI

Standalone official scholarly command, not part of the Runtime kernel. This crate does not
install a `/tools` entrypoint, modify an
active Library/Plugin, or change the production backend.

## Delivered commands

```sh
anchor-scholarly sources
anchor-scholarly search --query 'learned cost models' --source crossref --limit 8 --offset 0
anchor-scholarly search --query 'all:compiler' --source arxiv
anchor-scholarly search-many --queries-file queries.txt --source openalex --budget 420
anchor-scholarly read --url https://publisher.example.org/paper.pdf --page-start 0 --offset 0
anchor-scholarly read-many --urls https://publisher.example.org/a,https://publisher.example.org/b
anchor-scholarly citations --identifier doi:10.1234/paper --direction cited_by --limit 8
```

`sources` probes Crossref, OpenAlex, then arXiv with the existing probe query and reports `sources`,
`usable`, `note`, and `retrieved_at`. A source refusing access is a status result, not a reason to
pretend that source answered. All sources unavailable still yields a successful status report.

`search` defaults to Crossref, limit 8, offset 0. All three source parsers retain the existing paper
fields and evidence levels. Crossref strips abstract markup and normalizes source links. OpenAlex
searches title/abstract, reconstructs the inverted abstract, and retains deduplicated locations.
arXiv tries the Atom API and falls back to its HTML search with reduced `listing` evidence,
appropriate page sizes, and empty relevance ordering.

`search-many` ignores blank lines and lines whose trimmed form begins with `#`, keeps order and
duplicates, permits at most 40 queries, and isolates per-query failures. It returns `queries`,
`results`, `attempted`, `with_results`, `not_attempted`, and `ran_out_of_time`. Limit is 1–100;
offset is nonnegative; budget is 0–3600 seconds with 0 meaning the default 420 seconds. Queries are
1–1000 Unicode characters. Content-bearing responses include `untrusted_source_content: true`.
stdout contains one JSON value and a newline; execution failures use stderr and exit 1, and syntax
failures exit 2.

## Compatibility and intentional bounds

- Crossref ignores offset and arXiv sends `start=0` (and no HTML `start`). OpenAlex uses
  `page = offset // limit + 1`, including non-page-aligned offsets. Correcting the other two source
  offsets is a follow-up compatibility decision, not an implemented capability.
- The batch budget is now a hard I/O deadline, including an in-flight query, DNS, lock waits,
  pacing, redirects, and retries. An interrupted query is recorded as an attempted
  timeout; later queries are named as not attempted. arXiv fallback shares the same deadline.
- Query files must be regular UTF-8 files of at most 1 MiB, without symlink components or `..`
  traversal. Descriptor-relative, no-follow opens avoid a check/open race; nonblocking opens
  reject FIFOs without hanging.
- Malformed source JSON/XML reports an error rather than substituting fake data. Atom parsing
  rejects DTDs and external entities, requires the Atom feed namespace, and limits XML nodes.
  OpenAlex requires a results array; abstract expansion is bounded to 8 MB across one response.
- Public-address rejection is conservative: special IPv4 blocks and IPv6 addresses outside native
  global unicast (including translated/tunnel ranges) fail closed. Some globally reachable
  special-purpose addresses are excluded.

## Transport and security

The production transport accepts HTTPS on port 443 only, without credentials. DNS must return
exclusively public addresses. The fetcher pins the selected IP with reqwest 0.12.28 `resolve`,
retaining the original URL hostname for TLS/SNI and HTTP Host. It does not resolve again when
connecting. Environment proxies, automatic redirects, decompression and implicit retries are
disabled. Each explicit redirect is validated and resolved/pinned afresh; no more than six
destinations are requested. Requests sharing a host are serialized; arXiv API/web share 3-second
pacing, and Crossref/OpenAlex each use 1 second. Pacing and cooldown state belongs to one
`Scholarly` instance, not a cross-process rate-limit service.

429/500/502/503/504 retry once with bounded numeric/date `Retry-After`; repeated failures install
the existing host cooldown semantics. Other status failures retain stable `ResearchToolError`
categories. Both advertised and streamed response sizes are limited to 8,000,000 bytes. Per-query
timeouts cover the entire fetch, not merely each socket read.

`Scholarly::with_transport` is an explicit library dependency-injection seam. Tests implement their
own transport to reach a loopback HTTP fixture while still exercising source URLs, public-address
validation, request policy, parsers, CLI argument dispatch and output serialization. The production
CLI has no endpoint override, private-network switch, fixture mode, or environment bypass.
The binary's OS sandbox remains responsible for its file/network authorization; this crate does
not invent another host permissions system.

Version-specific interfaces were checked against the published crate sources: reqwest 0.12.28
`ClientBuilder::{resolve,no_proxy,redirect,retry,https_only}` and roxmltree 0.20.0
`ParsingOptions::{allow_dtd,nodes_limit}`. These are library APIs, not provider availability evidence.

## Reading And Citations

`read` extracts article HTML using dom_smoothie 0.18.2, PDF using lopdf 0.42.0 and
pdf-extract 0.12.1, and UTF-8 plain text. Results preserve requested/final URL,
title, content type, character offset, page start, pages and continuation fields.
Each excerpt is at most 24,000 Unicode characters. PDF extraction uses at most 40
pages from the requested zero-based page start; corrupt, encrypted, unsupported,
empty or OCR-only documents return errors rather than fake evidence. arXiv
abstract pages are rejected; PDF URLs first try the HTML representation and share
one I/O deadline with the PDF fallback.

`read-many` accepts at most eight URLs, keeps exact input order and duplicates,
and returns per-document failures. Duplicate URLs retain their actual input positions.
Reads are not cached, so repeated URLs
can issue repeated requests. `citations` resolves DOI, arXiv or OpenAlex work
identifiers and follows `cited_by` or `cites`, using the same Paper projection as
search. Reference queries retain the legacy 200-reference/50-item chunk limit.

HTML parsing limits article scoring to 100,000 elements; input
and extracted text are capped at 8,000,000 bytes. These are not hard parser
memory/CPU quotas: lopdf can decompress internal streams without a public hard
limit, and an extraction task is not stopped by the I/O deadline. Unsupported
PDF parser panics are caught as extraction failures, not successful content.
Use the operator-authorized Runtime sandbox for untrusted documents; parser
resource isolation remains a deployment acceptance item.

## Distribution

The `academic-research` Plugin declares the relative `bin/anchor-scholarly`
stdio entry point. `anchor-distribution --scholarly <ELF>` installs the built
binary into the Graph bundle when that Plugin is referenced and refreshes its
frozen resource summary; no host `PATH` preinstallation is required.

## Integration Limits

The shared workspace includes `anchor-scholarly`. Packaging and `/tools/scholarly/run` integration
remain separate work. All production
providers, public source availability/authentication, real scholarly results, public TLS/SNI and
deployment behavior remain unverified. Tests do not contact public academic APIs or real models.

## Local verification

Run from the repository root after temporary workspace membership is present:

```sh
CARGO_TARGET_DIR=/tmp/anchor-production-parallel-FE8ZQu/scholarly-target CARGO_BUILD_JOBS=2 \
  cargo +stable test --manifest-path rust/Cargo.toml -p anchor-scholarly
CARGO_TARGET_DIR=/tmp/anchor-production-parallel-FE8ZQu/scholarly-target CARGO_BUILD_JOBS=2 \
  cargo +stable clippy --manifest-path rust/Cargo.toml -p anchor-scholarly --all-targets -- -D warnings
cargo +1.92.0 fmt --manifest-path rust/Cargo.toml -p anchor-scholarly -- --check
```

Fixtures are deterministic, small academic records plus explicitly oversized/error responses. Unit
tests use virtual Tokio time for retry, cooldown, deadline, concurrent locks, and pacing; integration
tests use real loopback HTTP. CLI binary tests cover stderr/exit codes, help and
command dispatch. Reader tests use actual HTML/PDF parsers and local HTTP fixtures.

The original 2026-10-06 first-slice commands passed in a detached worktree: 39 tests (18 unit,
7 batch, 9 boundaries, 5 search), zero failures/ignored tests; clippy with `-D warnings` and
1.92.0 format check both passed. The initial clippy pass found a collapsible arXiv `if`, corrected
before the final pass. No real provider or public scholarly endpoint was exercised. Temporary
workspace membership and lockfile changes were excluded from that worker delivery.
Subsequent implementation and acceptance are recorded in the development ledger.
