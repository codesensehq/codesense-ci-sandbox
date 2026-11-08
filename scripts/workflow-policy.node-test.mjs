// SPDX-License-Identifier: MIT
// node --test scripts/workflow-policy.node-test.mjs (the repository's public workflows)
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { parse } from "yaml";
import { workflowPolicy } from "./workflow-policy.mjs";

// The workflow set under test: .github/workflows, or CODESENSE_WORKFLOWS_DIR (from the repo root).
const directory = path.resolve(
  fileURLToPath(new URL("..", import.meta.url)),
  process.env.CODESENSE_WORKFLOWS_DIR || ".github/workflows",
);
if (!fs.existsSync(path.join(directory, "../allowed-actions.json")))
  throw new Error(
    `${directory} has no ../allowed-actions.json; set CODESENSE_WORKFLOWS_DIR to the workflow set`,
  );
const files = fs.readdirSync(directory).filter((f) => /\.ya?ml$/.test(f));
assert.ok(files.length, "public workflows must exist");
for (const file of files) {
  test(`public workflow policy: ${file}`, () => {
    assert.deepEqual(workflowPolicy(fs.readFileSync(path.join(directory, file), "utf8"), file), []);
  });
}

// GitHub-owned actions are covered by github_owned_allowed; third parties must
// match the exported list exactly so workflow PRs cannot outrun repository policy.
const patterns = JSON.parse(
  fs.readFileSync(path.join(directory, "../allowed-actions.json"), "utf8"),
);
function actionPatterns(source) {
  const found = new Set();
  function walk(value) {
    if (!value || typeof value !== "object") return;
    if (typeof value.uses === "string" && !value.uses.startsWith("./")) {
      const match = /^([^/]+\/[^/@]+)(?:\/[^@]+)?@.+$/.exec(value.uses);
      assert.ok(match, `unrecognized uses: ${value.uses}`);
      const repo = match[1].toLowerCase();
      if (!/^(actions|github)\//.test(repo)) found.add(`${repo}@*`);
    }
    Object.values(value).forEach(walk);
  }
  walk(parse(source));
  return [...found];
}
function missingActions(source) {
  const allowed = patterns.map((pattern) => pattern.toLowerCase());
  return actionPatterns(source).filter((pattern) => !allowed.includes(pattern));
}
test("action allowlist has unique explicit repository patterns", () => {
  assert.ok(Array.isArray(patterns));
  for (const pattern of patterns) assert.match(pattern, /^[\w.-]+\/[\w.-]+@\*$/);
  assert.equal(new Set(patterns.map((pattern) => pattern.toLowerCase())).size, patterns.length);
});
test("action allowlist matches every workflow's third-party uses exactly", () => {
  const used = new Set();
  for (const file of files) {
    const source = fs.readFileSync(path.join(directory, file), "utf8");
    assert.deepEqual(missingActions(source), [], `${file}: update .github/allowed-actions.json`);
    actionPatterns(source).forEach((pattern) => used.add(pattern));
  }
  assert.deepEqual(patterns.map((pattern) => pattern.toLowerCase()).sort(), [...used].sort());
});
test("action allowlist rejects a new unlisted action, including reusable workflows", () => {
  assert.deepEqual(missingActions("jobs: {test: {uses: unlisted-org/action/workflow@v1}}"), [
    "unlisted-org/action@*",
  ]);
  assert.deepEqual(missingActions('jobs: {test: {steps: [{uses: "unlisted-org/action@v1"}]}}'), [
    "unlisted-org/action@*",
  ]);
  assert.deepEqual(
    missingActions(
      'jobs: {test: {steps: [{uses: "actions/cache/restore@v4"}, {uses: "github/codeql-action/upload-sarif@v4"}, {uses: "./local/action"}]}}',
    ),
    [],
  );
});

const good = `# SPDX-License-Identifier: MIT
on: [pull_request, workflow_dispatch]
permissions: {}
concurrency:
  group: ci-test
  cancel-in-progress: true
jobs:
  static:
    runs-on: ubuntu-24.04
    permissions:
      contents: read
    timeout-minutes: 15
    steps:
      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5 # v4.3.1
        with:
          persist-credentials: false
`;

// Three planted bad workflows exercise each of the original policy failure classes.
const badFixtures = [
  ["unpinned action", good.replace(/@[0-9a-f]{40}/, "@v4"), /40-hex/],
  ["privileged trigger", good.replace("pull_request,", "pull_request_target,"), /triggers/],
  ["secret reference", good + '      - run: echo "${{ secrets.TOKEN }}"\n', /secrets/],
];
for (const [name, fixture, error] of badFixtures) {
  test(`reject planted workflow: ${name}`, () => {
    assert.match(workflowPolicy(fixture).join("\n"), error);
  });
}

test("accept the valid fixture", () => assert.deepEqual(workflowPolicy(good), []));

const afterFailure = (condition, run = "cargo test --doc") => `      - id: compile
        run: cargo test --no-run
      - name: Doctests
        if: ${condition}
        run: ${run}
`;
test("accept a cargo step after a failure once an earlier step must have succeeded", () => {
  for (const gate of [
    "steps.compile.outcome == 'success'",
    "steps.compile.conclusion == 'success'",
    "steps.compile.outcome=='success'",
  ])
    assert.deepEqual(
      workflowPolicy(good + afterFailure(`(success() || failure()) && ${gate}`)),
      [],
    );
  for (const condition of [
    "${{ always() && steps.compile.outcome == 'success' }}",
    "(always() && steps.compile.outcome == 'success')",
  ])
    assert.deepEqual(workflowPolicy(good + afterFailure(condition)), []);
});
test("a step that only mentions cargo in a tool or path name is not a cargo step", () => {
  for (const run of ["cargo-nextest --version", "ls ~/.cargo/bin"])
    assert.deepEqual(workflowPolicy(good + afterFailure("always()", run)), []);
});
for (const [name, fixture, error] of [
  ["bracket secret reference", good + `      - run: echo "\${{ secrets['TOKEN'] }}"\n`, /secrets/],
  ["whole secret context", good + '      - run: echo "${{ toJSON(secrets) }}"\n', /secrets/],
  ["inherited secrets", good + "    secrets: inherit\n", /secrets/],
  ["unrecognized ubuntu label", good.replace("ubuntu-24.04", "ubuntu-build"), /GitHub-hosted/],
  ["self-hosted runner", good.replace("ubuntu-24.04", "self-hosted"), /GitHub-hosted/],
  // GitHub no longer serves macos-14; jobs on it are cancelled without a runner.
  ["retired macos-14 label", good.replace("ubuntu-24.04", "macos-14"), /GitHub-hosted/],
  [
    "self-hosted labels",
    good.replace("ubuntu-24.04", "[self-hosted, ubuntu-24.04]"),
    /GitHub-hosted/,
  ],
  ["runner expression", good.replace("ubuntu-24.04", "${{ vars.RUNNER }}"), /GitHub-hosted/],
  ["unpinned container", good + "    container: ubuntu:24.04\n", /container image/],
  [
    "unpinned service",
    good + "    services:\n      db:\n        image: postgres:17\n",
    /service db image/,
  ],
  ["workflow_run", good.replace("pull_request,", "workflow_run,"), /triggers/],
  ["missing top-level permissions", good.replace("permissions: {}\n", ""), /top-level permissions/],
  ["job without timeout", good.replace("    timeout-minutes: 15\n", ""), /timeout-minutes/],
  [
    "second job without timeout",
    good + "  another:\n    runs-on: ubuntu-24.04\n    steps: []\n",
    /another: missing positive timeout/,
  ],
  ["write permissions", good.replace("contents: read", "contents: write"), /contents: read/],
  ["missing version comment", good.replace(" # v4.3.1", ""), /version|vX.Y.Z/],
  [
    "checkout credentials",
    good.replace("persist-credentials: false", "persist-credentials: true"),
    /persist-credentials/,
  ],
  [
    "no cancellation",
    good.replace("cancel-in-progress: true", "cancel-in-progress: false"),
    /concurrency/,
  ],
  // A failed setup step once let doctests compile without the sidecars, hiding the real failure.
  ["cargo after a failure", good + afterFailure("success() || failure()"), /runs cargo after/],
  ["cargo under always()", good + afterFailure("always()"), /runs cargo after/],
  [
    "cargo gated on a later step",
    good + afterFailure("always() && steps.later.outcome == 'success'") + "      - id: later\n",
    /runs cargo after/,
  ],
  [
    "cargo gated only on one side of ||",
    good + afterFailure("always() || steps.compile.outcome == 'success'"),
    /runs cargo after/,
  ],
  [
    "cargo gated under a top-level ||",
    good + afterFailure("always() || x && steps.compile.outcome == 'success'"),
    /runs cargo after/,
  ],
  [
    "cargo gated on a negated success",
    good + afterFailure("always() && !(steps.compile.outcome == 'success')"),
    /runs cargo after/,
  ],
  [
    "cargo after a shell separator",
    good + afterFailure("always()", "cd src-tauri && cargo test --doc"),
    /runs cargo after/,
  ],
]) {
  test(`reject ${name}`, () => assert.match(workflowPolicy(fixture).join("\n"), error));
}

test("hosted runner families and digest-pinned containers and services are accepted", () => {
  for (const runner of [
    "ubuntu-latest",
    "ubuntu-24.04",
    "ubuntu-22.04",
    "ubuntu-24.04-arm",
    "macos-latest",
    "macos-15",
    "macos-26",
    "windows-latest",
    "windows-2022",
    "windows-2025",
  ]) {
    const workflow =
      good.replace("ubuntu-24.04", runner) +
      `    container: ubuntu@sha256:${"a".repeat(64)}\n    services:\n      db:\n        image: postgres@sha256:${"b".repeat(64)}\n`;
    assert.deepEqual(workflowPolicy(workflow), []);
  }
});

test("CLI accepts valid workflows and returns policy errors for bad workflows", (t) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "workflow-policy-"));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const file = path.join(directory, "ci.yml");
  const run = () =>
    spawnSync(
      process.execPath,
      [fileURLToPath(new URL("./workflow-policy.mjs", import.meta.url)), directory],
      { encoding: "utf8" },
    );
  fs.writeFileSync(file, good);
  const valid = run();
  assert.equal(valid.status, 0, valid.stderr);
  assert.match(valid.stdout, /^workflow-policy: 1 files OK\n$/);
  for (const [, fixture, error] of badFixtures) {
    fs.writeFileSync(file, fixture);
    const result = run();
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, error);
  }
});

const scorecard = good
  .replace("pull_request, workflow_dispatch", "workflow_dispatch")
  .replace("  static:", "  scorecard:")
  .replace(
    "      contents: read",
    "      contents: read\n      security-events: write\n      id-token: write",
  );
test("Scorecard grants are confined to the exact file and job", () => {
  assert.deepEqual(workflowPolicy(scorecard, "scorecard.yml"), []);
  for (const file of ["ci.yml", "other.yml", "scorecard.yaml", "nested/scorecard.yml", ""]) {
    assert.match(workflowPolicy(scorecard, file).join("\n"), /permissions/);
  }
  assert.match(
    workflowPolicy(scorecard.replace("  scorecard:", "  other:"), "scorecard.yml").join("\n"),
    /permissions/,
  );
  for (const grant of ["security-events: write", "id-token: write"]) {
    assert.match(
      workflowPolicy(
        scorecard.replace(grant, grant.replace("write", "read")),
        "scorecard.yml",
      ).join("\n"),
      /permissions/,
    );
  }
  assert.match(
    workflowPolicy(
      scorecard.replace("id-token: write", "id-token: write\n      actions: read"),
      "scorecard.yml",
    ).join("\n"),
    /permissions/,
  );
  assert.match(
    workflowPolicy(scorecard.replace("contents: read", "contents: write"), "scorecard.yml").join(
      "\n",
    ),
    /permissions/,
  );
  assert.match(
    workflowPolicy(scorecard + "    secrets: inherit\n", "scorecard.yml").join("\n"),
    /secrets/,
  );
});
for (const trigger of ["schedule", "branch_protection_rule"]) {
  test(`${trigger} is confined to scorecard.yml`, () => {
    const fixture = scorecard.replace("workflow_dispatch", trigger);
    assert.deepEqual(workflowPolicy(fixture, "scorecard.yml"), []);
    assert.match(workflowPolicy(fixture, "ci.yml").join("\n"), /triggers/);
  });
}

for (const trigger of [
  "pull_request",
  "pull_request_target",
  "pull_request_review",
  "pull_request_review_comment",
]) {
  test(`Scorecard rejects ${trigger}`, () => {
    assert.match(
      workflowPolicy(scorecard.replace("workflow_dispatch", trigger), "scorecard.yml").join("\n"),
      /triggers/,
    );
  });
}
for (const [name, fixture, error] of [
  ["run step", scorecard + "      - run: echo unsafe\n", /approved uses/],
  [
    "unapproved action",
    scorecard.replace("actions/checkout@", "actions/setup-node@"),
    /approved uses/,
  ],
  ["checkout ref", scorecard + "          ref: another-branch\n", /checkout ref/],
  ["checkout repository", scorecard + "          repository: another/repository\n", /checkout ref/],
  [
    "persist credentials",
    scorecard.replace("persist-credentials: false", "persist-credentials: true"),
    /persist-credentials/,
  ],
  ["workflow env", scorecard + "env: {}\n", /workflow env/],
  ["workflow defaults", scorecard + "defaults: {}\n", /workflow env/],
  ["job env", scorecard + "    env: {}\n", /job env/],
  ["job defaults", scorecard + "    defaults: {}\n", /job defaults/],
  ["step env", scorecard + "        env: {}\n", /step env/],
  ["container", scorecard + `    container: ubuntu@sha256:${"a".repeat(64)}\n`, /job container/],
  ["services", scorecard + "    services: {}\n", /job services/],
]) {
  test(`Scorecard rejects ${name}`, () =>
    assert.match(workflowPolicy(fixture, "scorecard.yml").join("\n"), error));
}
test("exception grants do not cross between Scorecard and quarantine jobs", () => {
  const quarantine = good
    .replace("  static:", "  quarantine-issues:")
    .replace("contents: read", "contents: read\n      issues: read");
  assert.match(workflowPolicy(quarantine, "scorecard.yml").join("\n"), /permissions/);
  assert.match(
    workflowPolicy(
      quarantine.replace("  quarantine-issues:", "  scorecard:"),
      "scorecard.yml",
    ).join("\n"),
    /permissions/,
  );
  assert.match(workflowPolicy(scorecard, "quarantine.yml").join("\n"), /permissions/);
  assert.match(
    workflowPolicy(scorecard.replace("  scorecard:", "  quarantine-issues:"), "scorecard.yml").join(
      "\n",
    ),
    /permissions/,
  );
});
for (const on of [
  "push",
  "{push: {branches: [other]}}",
  "{push: {branches: [main], tags: ['*']}}",
]) {
  test(`Scorecard push must target main: ${on}`, () => {
    assert.match(
      workflowPolicy(scorecard.replace("[workflow_dispatch]", on), "scorecard.yml").join("\n"),
      /push must target/,
    );
  });
}
test("PR-only cancellation is allowed so push history scans remain uninterrupted", () => {
  const conditional = good.replace(
    "cancel-in-progress: true",
    "cancel-in-progress: ${{ github.event_name == 'pull_request' }}",
  );
  assert.deepEqual(workflowPolicy(conditional), []);
  assert.match(
    workflowPolicy(conditional.replace("== 'pull_request'", "== 'push'")).join("\n"),
    /concurrency/,
  );
});

test("issues read is limited to the exact quarantine workflow and job", () => {
  const quarantine = good
    .replace("  static:", "  quarantine-issues:")
    .replace("      contents: read", "      contents: read\n      issues: read");
  assert.deepEqual(workflowPolicy(quarantine, "quarantine.yml"), []);
  assert.deepEqual(
    workflowPolicy(
      quarantine.replace(
        "cancel-in-progress: true",
        "cancel-in-progress: ${{ github.event_name == 'pull_request' }}",
      ),
      "quarantine.yml",
    ),
    [],
  );
  for (const file of [
    "ci.yml",
    "quarantine.yaml",
    "other-quarantine.yml",
    "nested/quarantine.yml",
    "",
  ]) {
    assert.match(workflowPolicy(quarantine, file).join("\n"), /permissions must grant exactly/);
  }
  assert.match(
    workflowPolicy(quarantine.replace("  quarantine-issues:", "  other:"), "quarantine.yml").join(
      "\n",
    ),
    /permissions must grant exactly/,
  );
  assert.match(
    workflowPolicy(quarantine.replace("issues: read", "issues: write"), "quarantine.yml").join(
      "\n",
    ),
    /permissions must grant exactly/,
  );
  assert.match(
    workflowPolicy(quarantine.replace("      contents: read\n", ""), "quarantine.yml").join("\n"),
    /permissions must grant exactly/,
  );
  assert.match(
    workflowPolicy(quarantine.replace("      issues: read\n", ""), "quarantine.yml").join("\n"),
    /permissions must grant exactly/,
  );
  assert.match(
    workflowPolicy(
      scorecard.replace("  scorecard:", "  quarantine-issues:"),
      "quarantine.yml",
    ).join("\n"),
    /permissions must grant exactly/,
  );
});

test("Rust jobs preserve package coverage for nextest and doctests and always upload JUnit", () => {
  const workflow = parse(fs.readFileSync(path.join(directory, "rust.yml"), "utf8"));
  const packages = (command) => [...command.matchAll(/-p ([a-z-]+)/g)].map((m) => m[1]).sort();
  // Linux runs only the crates that pass there today; the rest join once fixed.
  const portable = ["codesense-host-proto", "codesense-fs-core"];
  for (const [name, expected] of [
    [
      "rust-macos",
      [
        ...portable,
        "codesense-pty-core",
        "codesense-session-server",
        "codesense-guards",
        "codesense",
      ],
    ],
    ["rust-linux", portable],
  ]) {
    const job = workflow.jobs[name];
    assert.deepEqual(packages(job.env.RUST_PACKAGES), [...expected].sort());
    const steps = job.steps;
    const runs = steps.filter((step) => step.run?.includes("cargo nextest run"));
    assert.equal(runs.length, 2, "compile and run both use nextest");
    for (const step of runs) {
      assert.match(step.run, /--profile ci/);
      assert.match(step.run, /\$RUST_PACKAGES/);
      assert.deepEqual(packages(step.run), []);
    }
    assert.ok(runs.some((step) => step.run.includes("--no-run")));
    assert.ok(runs.some((step) => !step.run.includes("--no-run")));
    const docs = steps.find((step) => step.run?.includes("cargo test --doc"));
    assert.match(docs.run, /\$RUST_PACKAGES/);
    assert.deepEqual(packages(docs.run), []);
    assert.equal(docs["timeout-minutes"], 15);
    // Doctests run after failed tests, but only once the test build ran and succeeded.
    assert.equal(docs.if, "(success() || failure()) && steps.compile.outcome == 'success'");
    assert.ok(runs.some((step) => step.id === "compile" && step.run.includes("--no-run")));
    assert.ok(steps.some((step) => step.run?.includes("--tool cargo-nextest")));
    const junit = steps.find((step) => step.with?.path === "target/nextest/ci/junit.xml");
    assert.equal(junit.if, "always()");
    assert.equal(junit.uses, "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02");
  }
});

test("the open-issue check covers every quarantine ledger file", (t) => {
  const workflow = parse(fs.readFileSync(path.join(directory, "quarantine.yml"), "utf8"));
  const step = workflow.jobs["quarantine-issues"].steps.find(
    (s) => s.name === "Require open quarantine issues",
  );
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "quarantine-open-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const bin = path.join(dir, "bin");
  fs.mkdirSync(bin);
  fs.mkdirSync(path.join(dir, "tests"));
  // Fake gh: issue 2 is closed, every other issue is open; each lookup is recorded.
  fs.writeFileSync(
    path.join(bin, "gh"),
    `#!/bin/sh\necho "$3" >> "${dir}/gh.calls"\ncase "$3" in */issues/2) echo CLOSED ;; *) echo OPEN ;; esac\n`,
    { mode: 0o755 },
  );
  // Fake jq for `jq -r '.[].issue' <file>`, so the test needs only node.
  fs.writeFileSync(
    path.join(bin, "jq"),
    `#!/bin/sh\nexec node -e 'for (const e of JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"))) console.log(e.issue)' "$3"\n`,
    { mode: 0o755 },
  );
  const issue = (n) => [{ issue: `https://github.com/example/project/issues/${n}` }];
  const run = () => {
    fs.rmSync(path.join(dir, "gh.calls"), { force: true });
    return spawnSync("bash", ["-c", step.run], {
      cwd: dir,
      encoding: "utf8",
      env: { ...process.env, PATH: `${bin}:${process.env.PATH}` },
    });
  };
  fs.writeFileSync(path.join(dir, "tests/quarantine.json"), JSON.stringify(issue(1)));
  fs.writeFileSync(path.join(dir, "tests/quarantine.extra.json"), JSON.stringify(issue(3)));
  let result = run();
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(fs.readFileSync(path.join(dir, "gh.calls"), "utf8").trim().split("\n").sort(), [
    "https://github.com/example/project/issues/1",
    "https://github.com/example/project/issues/3",
  ]);
  fs.writeFileSync(path.join(dir, "tests/quarantine.extra.json"), JSON.stringify(issue(2)));
  result = run();
  assert.equal(result.status, 1);
  assert.match(result.stderr, /issues\/2 \(CLOSED, tests\/quarantine\.extra\.json\)/);
});
