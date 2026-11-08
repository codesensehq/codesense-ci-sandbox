// SPDX-License-Identifier: MIT
// Integration seam: real Git histories, fake pinned scanners, no network.
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { resolveRange } from "./ci-scan-range.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
const lock = fs.readFileSync(path.join(root, "release/scanners.lock"), "utf8");
const pin = (tool) =>
  lock
    .split("\n")
    .find((line) => line.startsWith(`${tool} `))
    .split(/\s+/)[1];
function fixture(t, objectFormat = "sha1") {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ci-secret-test-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const repo = path.join(dir, "repo");
  const bin = path.join(dir, "bin");
  for (const sub of ["scripts", "release"]) fs.mkdirSync(path.join(repo, sub), { recursive: true });
  fs.mkdirSync(bin);
  for (const file of ["ci-secret-scan.sh", "ci-secret-report.mjs", "ci-scan-range.mjs"])
    fs.copyFileSync(path.join(root, "scripts", file), path.join(repo, "scripts", file));
  fs.writeFileSync(path.join(repo, "release/scanners.lock"), lock);
  const git = (...args) =>
    execFileSync("git", ["-C", repo, ...args], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    }).trim();
  git("init", "-q", "-b", "main", `--object-format=${objectFormat}`);
  git("config", "user.name", "Test");
  git("config", "user.email", "test@example.com");
  git("config", "commit.gpgsign", "false");
  const commit = () => {
    git("add", "--", "scripts/ci-secret-scan.sh", "release/scanners.lock");
    git("commit", "--allow-empty", "-qm", "fixture");
    return git("rev-parse", "HEAD");
  };
  const base = commit();
  const head = commit();
  git("update-ref", "refs/remotes/origin/main", head);
  for (const tool of ["gitleaks", "trufflehog"]) {
    fs.writeFileSync(
      path.join(bin, tool),
      `#!/bin/bash
if [ "$1" = version ] || [ "$1" = --version ]; then echo "${tool} \${FAKE_VERSION:-${pin(tool)}}"; exit 0; fi
printf '%s\\n' "$*" >> "$SCAN_CALLS"
echo candidate-value-must-stay-private >&2
${
  tool === "gitleaks"
    ? `
status=0
if [ "$1" = dir ]; then status=\${TREE_EXIT:-0}; fi
if [ "$1" = git ]; then status=\${HISTORY_EXIT:-0}; fi
report=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = --report-path ]; then shift; report=$1; fi
  shift
done
if [ "$status" = 1 ]; then
  printf '%s' '[{"File":"src/key.txt","StartLine":7,"RuleID":"test-key","Secret":"candidate-value-must-stay-private","Match":"candidate-value-must-stay-private"}]' > "$report"
else printf '[]' > "$report"; fi
exit "$status"
`
    : `
if [ "\${TRUFFLE_EXIT:-0}" = 183 ]; then
  printf '%s\\n' '{"SourceMetadata":{"Data":{"Filesystem":{"file":"src/key.txt","line":7}}},"DetectorName":"TestDetector","Raw":"candidate-value-must-stay-private","RawV2":"candidate-value-must-stay-private","Redacted":"candidate-value-must-stay-private"}'
fi
exit "\${TRUFFLE_EXIT:-0}"
`
}
`,
      { mode: 0o755 },
    );
  }
  const calls = path.join(dir, "calls");
  const run = (env = {}) =>
    spawnSync("bash", [path.join(repo, "scripts/ci-secret-scan.sh")], {
      encoding: "utf8",
      env: {
        ...process.env,
        PATH: `${bin}:${process.env.PATH}`,
        SCAN_BASE: base,
        SCAN_HEAD: head,
        DEFAULT_BRANCH: "main",
        SCAN_CALLS: calls,
        ...env,
      },
    });
  return { repo, git, base, head, run, calls };
}
test("normal range scans tree and history, disables verification and updates, and fails on findings", (t) => {
  const f = fixture(t);
  const result = f.run({ TREE_EXIT: "1", TRUFFLE_EXIT: "183" });
  assert.equal(result.status, 1);
  assert.doesNotMatch(result.stdout + result.stderr, /candidate-value/);
  assert.match(result.stdout, /src\/key.txt:7 test-key/);
  assert.match(result.stdout, /src\/key.txt:7 TestDetector/);
  assert.match(result.stdout, /gitleaks tree: 1 findings/);
  assert.match(result.stdout, /trufflehog tree: 1 findings/);
  assert.match(result.stderr, /findings detected/);
  assert.doesNotMatch(result.stderr, /scan error/);
  const calls = fs.readFileSync(f.calls, "utf8");
  assert.match(calls, /dir --no-banner --redact -c release\/public-export-gitleaks.toml/);
  assert.ok(calls.includes(`--log-opts=${f.base}..${f.head}`));
  assert.match(calls, /filesystem --no-update --no-verification --fail --json/);
});
test("clean scanners pass and a history finding fails independently", (t) => {
  const f = fixture(t);
  assert.equal(f.run().status, 0);
  assert.equal(f.run({ HISTORY_EXIT: "1" }).status, 1);
});
test("wrong scanner version fails before scanning", (t) => {
  const f = fixture(t);
  assert.equal(f.run({ FAKE_VERSION: "0.0.1" }).status, 1);
  assert.equal(fs.existsSync(f.calls), false);
});
test("zero before SHA scans all history when default tip equals head", (t) => {
  const f = fixture(t);
  assert.equal(f.run({ SCAN_BASE: "0".repeat(40) }).status, 0);
  assert.ok(fs.readFileSync(f.calls, "utf8").includes(`--log-opts=${f.head} .`));
});
test("rewritten history falls back to the merge base", (t) => {
  const f = fixture(t);
  f.git("checkout", "-qb", "rewritten", f.base);
  f.git("commit", "--allow-empty", "-qm", "rewritten");
  const rewritten = f.git("rev-parse", "HEAD");
  assert.equal(f.run({ SCAN_BASE: f.head, SCAN_HEAD: rewritten }).status, 0);
  assert.ok(fs.readFileSync(f.calls, "utf8").includes(`--log-opts=${f.base}..${rewritten}`));
});

test("scanner errors are distinct from findings and do not expose diagnostics", (t) => {
  const f = fixture(t);
  const result = f.run({ TREE_EXIT: "2", TRUFFLE_EXIT: "1" });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /gitleaks tree scan error/);
  assert.match(result.stderr, /trufflehog tree scan error/);
  assert.doesNotMatch(result.stderr, /findings detected|candidate-value/);
});
test("missing base object falls back, missing default ref fails closed", (t) => {
  const f = fixture(t);
  assert.equal(f.run({ SCAN_BASE: "a".repeat(40) }).status, 0);
  fs.rmSync(f.calls);
  const missing = f.run({ SCAN_BASE: "0".repeat(40), DEFAULT_BRANCH: "missing" });
  assert.equal(missing.status, 1);
  assert.match(missing.stderr, /default branch commit is unavailable/);
  assert.equal(fs.existsSync(f.calls), false);
});

test("unrelated histories fail closed without scanning", (t) => {
  const f = fixture(t);
  f.git("checkout", "--orphan", "unrelated");
  f.git("commit", "--allow-empty", "-qm", "unrelated");
  const unrelated = f.git("rev-parse", "HEAD");
  const result = f.run({ SCAN_BASE: "0".repeat(40), SCAN_HEAD: unrelated });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /no common ancestor with the default branch/);
  assert.equal(fs.existsSync(f.calls), false);
});

for (const [format, length] of [
  ["sha1", 40],
  ["sha256", 64],
]) {
  test(`shared resolver and secret scan CLI agree on ${format} ranges`, (t) => {
    const f = fixture(t, format);
    assert.equal(f.head.length, length);
    assert.deepEqual(
      resolveRange({ cwd: f.repo, base: f.base, head: f.head, defaultBranch: "main" }),
      {
        base: f.base,
        head: f.head,
        allHistory: false,
      },
    );
    assert.equal(f.run().status, 0);
    assert.ok(fs.readFileSync(f.calls, "utf8").includes(`--log-opts=${f.base}..${f.head}`));
    assert.deepEqual(
      resolveRange({ cwd: f.repo, base: "0".repeat(length), head: f.head, defaultBranch: "main" }),
      {
        base: f.head,
        head: f.head,
        allHistory: true,
      },
    );
    assert.equal(f.run({ SCAN_BASE: "0".repeat(length) }).status, 0);
    assert.ok(fs.readFileSync(f.calls, "utf8").includes(`--log-opts=${f.head} .`));
  });
}
