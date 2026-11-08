<!-- SPDX-License-Identifier: MIT -->

# Test quarantine

`quarantine.json` is MIT-licensed data. JSON cannot carry a comment header; this
schema note supplies its SPDX license declaration without changing the array shape.
The ledger tracks the reader EOF/close-gate race under full-suite load, a CLI
version warm-up stall, headless-focus failures on macOS, Vitest full-suite
timeouts, and a host-requested kill that can lose its exit status on slower
runners. The reader test retains its bounded waits while the public issue tracks
the remaining race.
The deferred scheduler proof is unwritten and remains an explicit skip with a
reason, rather than a quarantine of a failing test.

Each entry has four required strings:

- `id`: `vitest:repo/path.test.ts#suite > test`,
  `playwright:tests/path.spec.ts#suite > test`, or
  `cargo:crate::module::test_name`. Titles include every enclosing suite, in order.
- `reason`: the failure and why the test cannot run reliably yet, public-safe.
- `issue`: exactly `https://github.com/<owner>/<repo>/issues/<number>`, an open public issue in the
  ledger's issue repository (see below).
- `added`: a UTC date in `YYYY-MM-DD` format. Quarantine expires after 30 days.

Run `node --test scripts/check-quarantine.node-test.mjs` (also included in
`pnpm test:node-scripts`). The check rejects missing/placeholder URLs and expired
entries. The public CI quarantine job also queries each issue using a read-only GitHub
issues token and requires `OPEN`; closed issues and API errors fail that job.
The gate, runners and frame-security guard all read
`QUARANTINE_ISSUE_REPOSITORY`, defaulting to `codesensehq/codesense`. Public CI
uses that code default everywhere and does not read a repository-variable override.
The environment variable remains available for a local override; Node validation
and runner filtering use the same default. The workflow relies on Node validation
before checking whether each issue is open.
`GITHUB_REPOSITORY` does not select the quarantine issue repository.

Vitest loads `support/quarantine-vitest.ts` from its setup: it prints the reason
and skips the matching test. Playwright specs import `test` from
`support/quarantine-playwright.ts` (the pane harness does this for its consumers).
Its hook prints the reason and calls `test.fixme(true, reason)`. Frame-security's
fixture guard accepts only ledger-backed fixme annotations with the exact reason;
unlisted skips and missing-fixture failures still fail the run.

The inventory check verifies each Vitest/Playwright entry names an existing file.
Cargo IDs start with the library's `[lib].name` (or its package name with hyphens
replaced by underscores when no library name is set); binary and integration-test
IDs use their Cargo target name. Follow that with the Rust module path and function
name. The inventory follows conventional and `#[path]` modules and checks Cargo
attributes and ledger entries in both directions, including identical issue URLs.
Macro-generated/include-generated quarantine markers must be made explicit in a
resolved source module; unresolved markers fail the check.

Cargo does not load JSON at runtime. Add a matching ledger entry and mark the
Rust test `#[ignore = "quarantine: <issue>"]`. Cargo prints that reason; use
`cargo test -- --ignored` to run quarantined tests deliberately. Remove both the
attribute and ledger entry when fixed. Never silently retry or delete a red test.

These adapters skip the test body, not `beforeAll` or worker fixtures. Those may
already have run before a per-test skip; fixture failures still fail the run.
Hostile-frame security tests (`tests/frame-security/hostile.spec.ts`) may never be
quarantined. Runners treat invalid or expired entries as unlisted.

`vitest.harness.config.ts` inherits the default setup. The separate `site/vitest.*`
configs do not: they belong to the website toolchain and need an explicit setup
adapter before website entries can be quarantined. Other Playwright suites must
import the adapter too; the frame-security specs and pane harness already do.
