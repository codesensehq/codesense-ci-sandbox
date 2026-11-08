// SPDX-License-Identifier: MIT
// The shared lint entry point must pass the workflow set to both tools and fail closed.
import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const script = fileURLToPath(new URL("./lint-workflows.sh", import.meta.url));
// The workflow set under test: .github/workflows, or CODESENSE_WORKFLOWS_DIR (from the repo root).
const workflows = path.resolve(
  fileURLToPath(new URL("..", import.meta.url)),
  process.env.CODESENSE_WORKFLOWS_DIR || ".github/workflows",
);
if (!fs.existsSync(path.join(workflows, "../allowed-actions.json")))
  throw new Error(
    `${workflows} has no ../allowed-actions.json; set CODESENSE_WORKFLOWS_DIR to the workflow set`,
  );
const good = fs.readFileSync(path.join(workflows, "ci.yml"), "utf8");
function run(
  t,
  {
    toolFails = "",
    empty = false,
    workflow = good,
    policySilent = false,
    filenames = ["a.yml", "b.yaml"],
  } = {},
) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "workflow-lint-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const bin = path.join(dir, "bin");
  const workflows = path.join(dir, "workflow files");
  fs.mkdirSync(bin);
  fs.mkdirSync(workflows);
  if (!empty) for (const file of filenames) fs.writeFileSync(path.join(workflows, file), workflow);
  fs.writeFileSync(
    path.join(dir, "dependabot.yml"),
    "# SPDX-License-Identifier: MIT\nversion: 2\nupdates: []\n",
  );
  for (const tool of ["actionlint", "zizmor"])
    fs.writeFileSync(
      path.join(bin, tool),
      `#!/bin/sh\nprintf '%s\\n' "$@" > "$LINT_LOG/${tool}.args"\nexit ${tool === toolFails ? 1 : 0}\n`,
      { mode: 0o755 },
    );
  if (policySilent)
    fs.writeFileSync(path.join(bin, "node"), "#!/bin/sh\nexit 0\n", { mode: 0o755 });
  const result = spawnSync("bash", [script, workflows], {
    encoding: "utf8",
    env: { ...process.env, PATH: `${bin}:${process.env.PATH}`, LINT_LOG: dir },
  });
  return { result, dir, workflows };
}

test("both linters receive every workflow; zizmor is offline and fails on medium findings", (t) => {
  const { result, dir, workflows } = run(t);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /^workflow-policy: 2 files OK$/m);
  const files = [path.join(workflows, "a.yml"), path.join(workflows, "b.yaml")];
  const args = (tool) =>
    fs
      .readFileSync(path.join(dir, `${tool}.args`), "utf8")
      .trim()
      .split("\n");
  assert.deepEqual(args("actionlint"), files);
  assert.deepEqual(args("zizmor"), [
    "--no-config",
    "--offline",
    "--strict-collection",
    "--no-ignores",
    "--min-severity=medium",
    ...files,
    `${workflows}/../dependabot.yml`,
  ]);
});
for (const tool of ["actionlint", "zizmor"]) {
  test(`${tool} failure fails workflow lint`, (t) =>
    assert.notEqual(run(t, { toolFails: tool }).result.status, 0));
}
test("an empty workflow directory fails", (t) => {
  const { result } = run(t, { empty: true });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /no workflows/);
});

for (const [name, workflow, error] of [
  ["action tag", good.replace(/(actions\/checkout)@[0-9a-f]{40}/, "$1@v7"), /40-hex/],
  ["missing timeout", good.replace(/    timeout-minutes:.*\n/, ""), /timeout-minutes/],
  ["missing concurrency", good.replace(/concurrency:[\s\S]*?\n\njobs:/, "jobs:"), /concurrency/],
  ["missing permissions", good.replace("permissions: {}\n", ""), /top-level permissions/],
  ["secret context", good + '      - run: echo "${{ toJSON(secrets) }}"\n', /secrets/],
]) {
  test(`policy failure stops lint before external linters: ${name}`, (t) => {
    const { result, dir } = run(t, { workflow });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, error);
    assert.equal(fs.existsSync(path.join(dir, "actionlint.args")), false);
    assert.equal(fs.existsSync(path.join(dir, "zizmor.args")), false);
  });
}

test("a silent policy CLI cannot bypass validation", (t) => {
  const { result, dir } = run(t, { policySilent: true });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /policy did not confirm validation/);
  assert.equal(fs.existsSync(path.join(dir, "actionlint.args")), false);
  assert.equal(fs.existsSync(path.join(dir, "zizmor.args")), false);
});

const scorecard = fs.readFileSync(path.join(workflows, "scorecard.yml"), "utf8");
test("lint CLI preserves the Scorecard file exception", (t) => {
  const { result } = run(t, { workflow: scorecard, filenames: ["scorecard.yml"] });
  assert.equal(result.status, 0, result.stderr);
});
test("lint CLI rejects Scorecard grants in other workflow files before linters", (t) => {
  const { result, dir } = run(t, { workflow: scorecard });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /permissions/);
  assert.equal(fs.existsSync(path.join(dir, "actionlint.args")), false);
});
