// SPDX-License-Identifier: MIT
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { isPublicRemote, scanPush } from "./privacy-ci.mjs";

const SCRIPTS = path.dirname(fileURLToPath(import.meta.url));
const INSTALL = path.join(SCRIPTS, "git-hooks", "install.sh");
const CLI = path.join(SCRIPTS, "privacy-ci.mjs");
// Fake terms only: the fixture inventory fills every private slot.
const TERM = "zqfixtureprivateterm";
const INVENTORY = {
  version: 1,
  rules: [5, 6, 7, 8, 9, 24].map((order) => ({
    id: `fixture-${order}`,
    sev: order === 24 ? "text" : "error",
    kind: "show",
    source: order === 24 ? "zqfixturetextonly" : `${TERM}${order === 5 ? "" : order}`,
    flags: [6, 7, 8].includes(order) ? "gi" : "g",
    order,
    classification: "private-only",
  })),
};
const PUBLIC_URLS = [
  "https://github.com/codesensehq/codesense",
  "https://github.com/CodeSenseHQ/codesense.git",
  "https://github.com/codesensehq/codesense/",
  "https://git@github.com/codesensehq/codesense.git",
  "git@github.com:codesensehq/codesense.git",
  "ssh://git@github.com/codesensehq/codesense.git",
  "ssh://git@github.com:22/codesensehq/codesense",
  "github-alias:codesensehq/codesense.git",
  "https://github.com/%63odesensehq/codesense",
  "https://github.com",
  "https://GitHub.com/only-owner",
  "github.com:weird//",
];
const OTHER_URLS = [
  "https://github.com/example/project.git",
  "git@github.com:example/project.git",
  "ssh://git@github.com/example/project/",
  "https://gitlab.com/example/project.git",
  "/tmp/somewhere.git",
];

// The real clone's hooks directory is shared by every worktree; the suite must
// never write to it. Compare names and contents (link targets for links).
const realHooks = execFileSync(
  "git",
  ["rev-parse", "--path-format=absolute", "--git-path", "hooks"],
  { cwd: SCRIPTS, encoding: "utf8" },
).trim();
const snapshot = (dir) =>
  fs.existsSync(dir)
    ? fs
        .readdirSync(dir, { recursive: true })
        .sort()
        .map((name) => {
          const file = path.join(dir, name);
          const stat = fs.lstatSync(file);
          if (stat.isSymbolicLink()) return `${name} -> ${fs.readlinkSync(file)}`;
          if (stat.isDirectory()) return `${name}/`;
          return `${name} ${stat.mode} ${fs.readFileSync(file, "base64")}`;
        })
    : [];
const realBefore = snapshot(realHooks);
test.after(() => assert.deepEqual(snapshot(realHooks), realBefore));

function sandbox(t, { inventory = true } = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "privacy-hooks-test-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const home = path.join(root, "home");
  const work = path.join(root, "work");
  const bare = path.join(root, "public.git");
  const other = path.join(root, "other.git");
  fs.mkdirSync(home);
  const terms = path.join(root, "terms.json");
  fs.writeFileSync(terms, JSON.stringify(INVENTORY));
  const env = { ...process.env };
  for (const key of Object.keys(env))
    if (key.startsWith("GIT_") || key.startsWith("CODESENSE_PRIVACY")) delete env[key];
  Object.assign(env, {
    HOME: home,
    GIT_CONFIG_GLOBAL: path.join(home, ".gitconfig"),
    GIT_CONFIG_NOSYSTEM: "1",
    GIT_TERMINAL_PROMPT: "0",
    CODESENSE_PRIVACY_PUBLIC_REMOTES: bare,
  });
  if (inventory) env.CODESENSE_PRIVACY_TERMS = terms;
  const run = (args, options = {}) =>
    spawnSync("git", args, { cwd: work, encoding: "utf8", env, ...options });
  const git = (...args) => {
    const result = run(args);
    if (result.status !== 0) throw new Error(`git ${args.join(" ")}: ${result.stderr}`);
    return result.stdout;
  };
  // Pushes and commits start a detached `git maintenance run --auto` that can
  // outlive the test and write into the sandbox while it is removed.
  execFileSync("git", ["config", "--global", "maintenance.auto", "false"], { env });
  execFileSync("git", ["config", "--global", "gc.auto", "0"], { env });
  for (const dir of [bare, other]) execFileSync("git", ["init", "--bare", "-q", dir], { env });
  execFileSync("git", ["init", "-q", "--initial-branch=main", work], { env });
  git("config", "user.name", "Fixture");
  git("config", "user.email", "fixture@example.com");
  git("config", "commit.gpgsign", "false");
  git("config", "tag.gpgsign", "false");
  git("remote", "add", "public", bare);
  git("remote", "add", "other", other);
  // The dispatcher runs the checkout's own scripts/git-hooks, as in a real clone.
  fs.symlinkSync(SCRIPTS, path.join(work, "scripts"));
  fs.appendFileSync(path.join(work, ".git", "info", "exclude"), "/scripts\n/release\n");
  const stage = (file, content) => {
    fs.writeFileSync(path.join(work, file), content);
    git("add", "--", file);
  };
  const commit = (file, content, message = "Fixture change") => {
    stage(file, content);
    // Fixture commits skip commit-msg so the pre-push layer is what is tested.
    git("commit", "-q", "--no-verify", "-m", message);
    return git("rev-parse", "HEAD").trim();
  };
  const install = (extraEnv = {}) =>
    spawnSync("sh", [INSTALL], { cwd: work, encoding: "utf8", env: { ...env, ...extraEnv } });
  const hooks = path.join(work, ".git", "hooks");
  // A push that skips the hooks, to seed remote state.
  const seed = (...args) => git("-c", `core.hooksPath=${os.devNull}`, "push", "-q", ...args);
  const remoteHas = (ref) =>
    spawnSync("git", ["--git-dir", bare, "rev-parse", "--verify", "-q", ref], { env }).status === 0;
  return {
    root,
    env,
    work,
    bare,
    other,
    hooks,
    git,
    run,
    stage,
    commit,
    install,
    seed,
    remoteHas,
    terms,
  };
}

const blocked = (result) => {
  assert.notEqual(result.status, 0, result.stderr);
  assert.match(result.stderr, /privacy-scan: fixture-\d+ x\d+/);
  assert.ok(!result.stderr.includes(TERM), "output is redacted");
  assert.ok(!result.stdout.includes(TERM), "output is redacted");
};
const passed = (result) => assert.equal(result.status, 0, result.stderr);
// The hook's own stderr lines, without git's.
const hookLines = (stderr) => stderr.split("\n").filter((line) => line.startsWith("privacy-scan:"));
const PUBLIC_NOTICE = /privacy hook: public rules only; no private inventory/;
const UNSCANNED = /privacy hook: skipped the local scan of .*; CI scans this/;
// A fake key, split so this file never holds it whole.
const FAKE_KEY = "AKIA" + "ABCDEFGHIJKLMNOP";

test("public remote detection covers GitHub URL forms and fails closed", () => {
  for (const url of PUBLIC_URLS) assert.equal(isPublicRemote(url), true, url);
  for (const url of OTHER_URLS) assert.equal(isPublicRemote(url), false, url);
  assert.equal(isPublicRemote("/tmp/x.git", "/tmp/y.git /tmp/x.git"), true);
});

test("a clean push to a public remote is allowed", (t) => {
  const s = sandbox(t);
  passed(s.install());
  s.commit("a.txt", "clean\n");
  passed(s.run(["push", "-q", "public", "main"]));
  assert.ok(s.remoteHas("refs/heads/main"));
});

for (const surface of [
  "added line",
  "commit message",
  "branch name",
  "tag message",
  "nested tag",
]) {
  test(`a private term in the ${surface} blocks a public push`, (t) => {
    const s = sandbox(t);
    passed(s.install());
    s.commit(
      "a.txt",
      surface === "added line" ? `x ${TERM} y\n` : "clean\n",
      surface === "commit message" ? `Fix ${TERM.toUpperCase()}7` : "Fixture change",
    );
    let ref = "main";
    let check = "refs/heads/main";
    if (surface === "branch name") {
      ref = `main:refs/heads/topic-${TERM}`;
      check = `refs/heads/topic-${TERM}`;
    }
    if (surface === "tag message") s.git("tag", "-a", "v1", "-m", `Release ${TERM}`);
    if (surface === "nested tag") {
      s.git("tag", "-a", "inner", "-m", `Release ${TERM}`);
      s.git("tag", "-a", "v1", "-m", "Clean outer tag", "inner");
    }
    if (surface.includes("tag")) [ref, check] = ["refs/tags/v1", "refs/tags/v1"];
    blocked(s.run(["push", "-q", "public", ref]));
    assert.ok(!s.remoteHas(check));
  });
}

test("a clean annotated tag push is allowed", (t) => {
  const s = sandbox(t);
  passed(s.install());
  s.commit("a.txt", "clean\n");
  s.git("tag", "-a", "v1", "-m", "Clean release");
  passed(s.run(["push", "-q", "public", "refs/tags/v1"]));
  assert.ok(s.remoteHas("refs/tags/v1"));
});

test("a term added then removed in the same push is still blocked", (t) => {
  const s = sandbox(t);
  passed(s.install());
  s.commit("a.txt", `${TERM}\n`);
  s.commit("a.txt", "clean\n");
  blocked(s.run(["push", "-q", "public", "main"]));
});

test("a non-public remote passes even with a private term", (t) => {
  const s = sandbox(t, { inventory: false });
  passed(s.install());
  s.commit("a.txt", `${TERM}\n`, `Fix ${TERM}`);
  passed(s.run(["push", "-q", "other", "main"]));
});

test("without an inventory, a clone runs the public rules with a notice", (t) => {
  const s = sandbox(t, { inventory: false });
  passed(s.install());
  // A directory where the user inventory goes is not an inventory.
  fs.mkdirSync(path.join(s.env.HOME, ".config", "codesense", "privacy-terms.json"), {
    recursive: true,
  });
  // A private term passes: no private rules are loaded.
  s.stage("a.txt", `${TERM}\n`);
  let result = s.run(["commit", "-q", "-m", `Fix ${TERM}`]);
  passed(result);
  assert.match(result.stderr, PUBLIC_NOTICE);
  result = s.run(["push", "-q", "public", "main"]);
  passed(result);
  assert.match(result.stderr, PUBLIC_NOTICE);
  // A key still blocks, in a message and in a pushed file.
  const keyBlocked = (r) => {
    assert.notEqual(r.status, 0);
    assert.match(r.stderr, /privacy-scan: token x1/);
    assert.ok(!r.stderr.includes(FAKE_KEY), "output is redacted");
  };
  s.stage("b.txt", `${FAKE_KEY}\n`);
  keyBlocked(s.run(["commit", "-q", "-m", `Add ${FAKE_KEY}`]));
  s.git("commit", "-q", "--no-verify", "-m", "Add key");
  keyBlocked(s.run(["push", "-q", "public", "main"]));
  // A destination that is not public stays silent, required or not.
  for (const extra of [{ CODESENSE_PRIVACY_REQUIRED: "1" }, {}]) {
    s.git("commit", "-q", "--no-verify", "--allow-empty", "-m", "Next");
    const result = s.run(["push", "-q", "other", "main"], { env: { ...s.env, ...extra } });
    passed(result);
    assert.equal(result.stderr, "");
  }
});

test("the dispatcher blocks a required clone without an inventory before the checkout's hooks run", (t) => {
  const s = sandbox(t, { inventory: false });
  passed(s.install());
  s.commit("a.txt", "clean\n");
  // A crafted checkout whose hooks allow everything.
  fs.rmSync(path.join(s.work, "scripts"));
  fs.mkdirSync(path.join(s.work, "scripts", "git-hooks"), { recursive: true });
  for (const name of ["pre-push", "commit-msg"])
    fs.writeFileSync(path.join(s.work, "scripts", "git-hooks", name), "#!/bin/sh\nexit 0\n", {
      mode: 0o755,
    });
  // Its own term list does not count either.
  fs.mkdirSync(path.join(s.work, "release"));
  fs.copyFileSync(s.terms, path.join(s.work, "release", "privacy-terms.json"));
  const blocksBoth = (env = s.env) => {
    for (const result of [
      s.run(["push", "-q", "public", "main"], { env }),
      s.run(["commit", "-q", "--allow-empty", "-m", "Clean"], { env }),
    ]) {
      assert.notEqual(result.status, 0);
      assert.match(result.stderr, /CODESENSE_PRIVACY_TERMS/);
    }
  };
  // Either switch requires it.
  blocksBoth({ ...s.env, CODESENSE_PRIVACY_REQUIRED: "1" });
  s.git("config", "codesense.privacyRequired", "true");
  blocksBoth();
  // A user inventory must be a readable file, and an empty HOME has none.
  const user = path.join(s.env.HOME, ".config", "codesense", "privacy-terms.json");
  fs.mkdirSync(user, { recursive: true });
  blocksBoth();
  blocksBoth({ ...s.env, HOME: "" });
  // With only non-public remotes, a required commit passes silently.
  s.git("remote", "remove", "public");
  const quiet = s.run(["commit", "-q", "--allow-empty", "-m", "Clean"]);
  passed(quiet);
  assert.equal(quiet.stderr, "");
  s.git("remote", "add", "public", s.bare);
  // With an inventory outside the tree, the checkout's hooks decide.
  const env = { ...s.env, CODESENSE_PRIVACY_TERMS: s.terms };
  passed(s.run(["push", "-q", "public", "main"], { env }));
  passed(s.run(["commit", "-q", "--allow-empty", "-m", "Clean"], { env }));
});

test("a clone that requires the private rules blocks without an inventory", (t) => {
  const s = sandbox(t, { inventory: false });
  passed(s.install());
  s.commit("a.txt", "clean\n");
  const both = (env) => [
    s.run(["push", "-q", "public", "main"], { env }),
    s.run(["commit", "-q", "--allow-empty", "-m", "Clean"], { env }),
  ];
  const blocksBoth = (extra = {}) => {
    for (const result of both({ ...s.env, ...extra })) {
      assert.notEqual(result.status, 0);
      assert.match(result.stderr, /CODESENSE_PRIVACY_TERMS/);
    }
  };
  // The env value is read as a git boolean; one git cannot read requires them.
  blocksBoth({ CODESENSE_PRIVACY_REQUIRED: "1" });
  blocksBoth({ CODESENSE_PRIVACY_REQUIRED: "maybe" });
  for (const value of ["0", "off"])
    for (const result of both({ ...s.env, CODESENSE_PRIVACY_REQUIRED: value })) {
      passed(result);
      assert.match(result.stderr, PUBLIC_NOTICE);
    }
  // So does git config, at any scope, and a value git cannot read as a boolean.
  const global = path.join(s.root, "global.gitconfig");
  fs.writeFileSync(global, "[codesense]\n\tprivacyRequired = true\n");
  blocksBoth({ GIT_CONFIG_GLOBAL: global });
  s.git("config", "codesense.privacyRequired", "maybe");
  blocksBoth();
  s.git("config", "codesense.privacyRequired", "true");
  blocksBoth();
  // A file in the tree never satisfies them.
  fs.mkdirSync(path.join(s.work, "release"));
  fs.copyFileSync(s.terms, path.join(s.work, "release", "privacy-terms.json"));
  blocksBoth();
  // The user inventory does: the private rules run as usual.
  fs.mkdirSync(path.join(s.env.HOME, ".config", "codesense"), { recursive: true });
  fs.copyFileSync(s.terms, path.join(s.env.HOME, ".config", "codesense", "privacy-terms.json"));
  s.stage("b.txt", "two\n");
  blocked(s.run(["commit", "-q", "-m", `Fix ${TERM}`]));
  for (const result of both(s.env)) {
    passed(result);
    assert.doesNotMatch(result.stderr, PUBLIC_NOTICE);
  }
});

test("user and repo inventories are found", (t) => {
  const s = sandbox(t, { inventory: false });
  passed(s.install());
  s.commit("a.txt", "clean\n");
  // The repo's own inventory is the last fallback.
  fs.mkdirSync(path.join(s.work, "release"));
  fs.copyFileSync(s.terms, path.join(s.work, "release", "privacy-terms.json"));
  s.commit("b.txt", `${TERM}\n`);
  blocked(s.run(["push", "-q", "public", "main"]));
  // The user-level inventory comes before it.
  fs.writeFileSync(path.join(s.work, "release", "privacy-terms.json"), "{}");
  fs.mkdirSync(path.join(s.env.HOME, ".config", "codesense"), { recursive: true });
  fs.copyFileSync(s.terms, path.join(s.env.HOME, ".config", "codesense", "privacy-terms.json"));
  blocked(s.run(["push", "-q", "public", "main"]));
  s.git("reset", "-q", "--hard", "HEAD~1");
  const result = s.run(["push", "-q", "public", "main"]);
  passed(result);
  assert.doesNotMatch(result.stderr, PUBLIC_NOTICE);
});

test("an invalid inventory blocks", (t) => {
  const s = sandbox(t);
  fs.writeFileSync(s.terms, "{}");
  passed(s.install());
  s.commit("a.txt", "clean\n");
  const result = s.run(["push", "-q", "public", "main"]);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Invalid or unavailable private rule inventory/);
});

test("a push scans only commits the destination does not have", (t) => {
  const s = sandbox(t);
  passed(s.install());
  // History already on the destination (seeded without hooks) is not rescanned.
  s.commit("a.txt", `${TERM}\n`);
  s.seed("public", "main");
  s.git("switch", "-q", "-c", "topic");
  s.commit("b.txt", "clean\n");
  passed(s.run(["push", "-q", "public", "topic"]));
  // Pushing by URL, with no remote-tracking refs, does the same.
  s.commit("c.txt", "clean\n");
  passed(s.run(["push", "-q", s.bare, "topic"]));
  // An unpublished commit on a new branch is scanned.
  s.git("switch", "-q", "-c", "second");
  s.commit("d.txt", `${TERM}\n`);
  s.commit("e.txt", "clean\n");
  blocked(s.run(["push", "-q", "public", "second"]));
  // An update scans remote..local only.
  s.git("switch", "-q", "topic");
  s.commit("f.txt", "still clean\n");
  passed(s.run(["push", "-q", "public", "topic"]));
});

for (const repoint of ["set-url", "pushurl"]) {
  test(`remote-tracking refs from another URL are not trusted (${repoint})`, (t) => {
    const s = sandbox(t);
    passed(s.install());
    // A private history is on the private remote and its tracking refs.
    s.commit("a.txt", `${TERM}\n`);
    s.seed("other", "main");
    s.git("fetch", "-q", "other");
    assert.ok(s.git("rev-parse", "refs/remotes/other/main"));
    // The same remote name now pushes to the public destination.
    s.git("remote", "set-url", ...(repoint === "pushurl" ? ["--push"] : []), "other", s.bare);
    s.git("switch", "-q", "-c", "topic");
    s.commit("b.txt", "clean\n");
    blocked(s.run(["push", "-q", "other", "topic"]));
    assert.ok(!s.remoteHas("refs/heads/topic"));
  });
}

// A fast-import stream of `count` commits on main, starting from `base`; `salt`
// keeps two streams from the same base apart.
function commitStream(base, count, salt = "") {
  let stream = "";
  for (let i = 1; i <= count; i++) {
    const data = `${salt}${i}\n`;
    stream +=
      `commit refs/heads/main\ncommitter Fixture <fixture@example.com> ${1700000000 + i} +0000\n` +
      `data 2\nc\n${i === 1 ? `from ${base}\n` : ""}M 644 inline n.txt\ndata ${data.length}\n${data}\n`;
  }
  return stream;
}

// Adds `count` commits on top of HEAD (on main) in one fast-import run.
function addCommits(s, count) {
  const stream = commitStream(s.git("rev-parse", "HEAD").trim(), count);
  const result = s.run(["fast-import", "--quiet", "--force"], { input: stream });
  assert.equal(result.status, 0, result.stderr);
  s.git("reset", "-q", "--hard", "main");
}

test("a push with too many new commits is refused at once, with counts only", (t) => {
  const s = sandbox(t);
  passed(s.install());
  s.commit("a.txt", "clean\n");
  addCommits(s, 5000);
  const started = Date.now();
  const result = s.run(["push", "-q", "public", "main"]);
  // The missing progress line proves nothing was scanned; the time bound is loose.
  assert.ok(Date.now() - started < 10_000, `took ${Date.now() - started} ms`);
  assert.notEqual(result.status, 0);
  assert.match(
    result.stderr,
    /this push carries more than 200 commits or tags the public repo does not have/,
  );
  assert.doesNotMatch(hookLines(result.stderr).join("\n"), /[0-9a-f]{40}|scanned/);
  assert.ok(!s.remoteHas("main"));
  // The limit can be raised or lowered; an invalid one blocks.
  const limit = (value) => ({ ...s.env, CODESENSE_PRIVACY_MAX_NEW_COMMITS: value });
  for (const value of ["-1", "many", "1e9", "2147483647", "99999999999999999999"]) {
    const invalid = s.run(["push", "-q", "public", "main"], { env: limit(value) });
    assert.notEqual(invalid.status, 0);
    assert.match(invalid.stderr, /invalid CODESENSE_PRIVACY_MAX_NEW_COMMITS/);
  }
  s.git("reset", "-q", "--hard", "HEAD~4997");
  assert.notEqual(s.run(["push", "-q", "public", "main"], { env: limit("3") }).status, 0);
  passed(s.run(["push", "-q", "public", "main"], { env: limit("4") }));
  // Tag objects new to the destination count too.
  s.git("tag", "-a", "-m", "Release", "v1");
  const tag = (value) => s.run(["push", "-q", "public", "v1"], { env: limit(value) });
  assert.match(tag("0").stderr, /carries more than 0 commits or tags/);
  passed(tag("1"));
});

test("a clone behind the destination is told to fetch first", (t) => {
  const s = sandbox(t);
  passed(s.install());
  s.commit("a.txt", "clean\n");
  addCommits(s, 30);
  const held = s.git("rev-parse", "HEAD").trim();
  s.seed("public", "main");
  // The destination moves on by more than the limit, with commits this clone lacks.
  const ahead = s.run(["--git-dir", s.bare, "fast-import", "--quiet"], {
    input: commitStream(held, 25, "remote"),
  });
  assert.equal(ahead.status, 0, ahead.stderr);
  const env = { ...s.env, CODESENSE_PRIVACY_MAX_NEW_COMMITS: "20" };
  const push = (ref) => s.run(["push", "-q", "public", `HEAD:refs/heads/${ref}`], { env });
  s.git("checkout", "-q", "-b", "small", held);
  s.commit("b.txt", "clean\n");
  const small = s.git("rev-parse", "HEAD").trim();
  s.git("checkout", "-q", "-b", "large", held);
  addCommits(s, 25);
  // Behind, both are refused at once with the reason, never naming the destination.
  for (const ref of ["small", "large"]) {
    s.git("checkout", "-q", ref === "small" ? small : "large");
    const result = push(ref);
    assert.notEqual(result.status, 0);
    // git itself names the destination when a push fails; the hook never does.
    assert.deepEqual(hookLines(result.stderr), [
      "privacy-scan: this clone is behind the destination: fetch and retry; if it is still refused, the push carries private history.",
    ]);
    assert.ok(!hookLines(result.stderr).join("\n").includes(s.bare));
  }
  // After a fetch, the small push is counted as one and scanned; the large one is refused.
  s.git("fetch", "-q", "public");
  s.git("checkout", "-q", small);
  passed(push("small"));
  s.git("checkout", "-q", "large");
  assert.match(push("large").stderr, /this push carries more than 20 commits or tags/);
});

test("a long push scan prints progress, counts only", (t) => {
  const s = sandbox(t);
  passed(s.install());
  s.commit("a.txt", "clean\n");
  addCommits(s, 100);
  const result = s.run(["push", "-q", "public", "main"]);
  passed(result);
  assert.deepEqual(result.stderr.trim().split("\n"), ["privacy-scan: scanned 100/101 commits"]);
});

test("commit-msg blocks with a reason when the remotes cannot be listed", (t) => {
  const s = sandbox(t);
  const message = path.join(s.root, "message.txt");
  fs.writeFileSync(message, "Clean\n");
  const result = spawnSync("node", [CLI, "--hook", "commit-msg", message], {
    cwd: s.root,
    encoding: "utf8",
    env: { ...s.env, GIT_CEILING_DIRECTORIES: s.root },
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /privacy-scan: cannot list remotes/);
});

test("an unreachable destination fails closed", (t) => {
  const s = sandbox(t);
  const head = s.commit("a.txt", "clean\n");
  const missing = path.join(s.root, "missing.git");
  const result = spawnSync("node", [CLI, "--hook", "pre-push", "public", missing], {
    cwd: s.work,
    encoding: "utf8",
    env: { ...s.env, CODESENSE_PRIVACY_PUBLIC_REMOTES: missing },
    input: `refs/heads/main ${head} refs/heads/main ${"0".repeat(40)}\n`,
  });
  assert.notEqual(result.status, 0);
});

test("listing the destination never prompts and times out closed", (t) => {
  const s = sandbox(t);
  const head = s.commit("a.txt", "clean\n");
  // An SSH transport that records the prompt settings, then hangs.
  const seen = path.join(s.root, "ssh-env");
  const saved = process.env.GIT_SSH_COMMAND;
  process.env.GIT_SSH_COMMAND = `sh -c 'echo "$GIT_TERMINAL_PROMPT $GCM_INTERACTIVE" >"${seen}"; sleep 5'`;
  t.after(() =>
    saved === undefined
      ? delete process.env.GIT_SSH_COMMAND
      : (process.env.GIT_SSH_COMMAND = saved),
  );
  const started = Date.now();
  assert.throws(() =>
    scanPush({
      cwd: s.work,
      url: "ssh://git@hang.invalid/project.git",
      updates: `refs/heads/main ${head} refs/heads/main ${"0".repeat(40)}\n`,
      privateRulesFile: s.terms,
      remoteTimeoutMs: 1000,
    }),
  );
  assert.ok(Date.now() - started < 4000);
  assert.equal(fs.readFileSync(seen, "utf8").trim(), "0 never");
});

test("a deletion push is allowed", (t) => {
  const s = sandbox(t);
  passed(s.install());
  s.commit("a.txt", "clean\n");
  s.seed("public", `main:refs/heads/topic-${TERM}`);
  passed(s.run(["push", "-q", "public", "--delete", `topic-${TERM}`]));
  assert.ok(!s.remoteHas(`refs/heads/topic-${TERM}`));
});

test("commit-msg blocks a private term only when a remote is public", (t) => {
  const s = sandbox(t);
  passed(s.install());
  s.stage("a.txt", "one\n");
  blocked(s.run(["commit", "-q", "-m", `Fix ${TERM}`]));
  // Every message line counts, comments too; only the verbose diff below the scissors does not.
  const file = path.join(s.root, "message.txt");
  fs.writeFileSync(file, `Clean subject\n# ${TERM}\n`);
  blocked(s.run(["commit", "-q", "--cleanup=strip", "-F", file]));
  fs.writeFileSync(
    file,
    `Clean subject\n# ------------------------ >8 ------------------------\n+${TERM}\n`,
  );
  passed(s.run(["commit", "-q", "--cleanup=strip", "-F", file]));
  // Without the extra-URL env, the shell prefilter and node still find a public URL.
  const env = { ...s.env, CODESENSE_PRIVACY_PUBLIC_REMOTES: "" };
  s.git("remote", "remove", "public");
  s.git("remote", "set-url", "other", "https://github.com/codesensehq/project.git");
  s.stage("a.txt", "two\n");
  blocked(s.run(["commit", "-q", "-m", `Fix ${TERM}`], { env }));
  // No public remote: no scan.
  s.git("remote", "set-url", "other", "https://example.com/project.git");
  passed(s.run(["commit", "-q", "-m", `Fix ${TERM}`], { env }));
});

test("commit-msg drops comment lines only when cleanup strips them", (t) => {
  const s = sandbox(t);
  passed(s.install());
  const file = path.join(s.root, "message.txt");
  s.git("config", "commit.cleanup", "strip");
  s.git("config", "core.commentChar", ";");
  s.stage("a.txt", "one\n");
  // The configured comment char's lines go; other lines stay.
  fs.writeFileSync(file, `Clean subject\n; ${TERM}\n`);
  passed(s.run(["commit", "-q", "-F", file]));
  s.stage("a.txt", "two\n");
  fs.writeFileSync(file, `Clean subject\n# ${TERM}\n`);
  blocked(s.run(["commit", "-q", "-F", file]));
  // Without a stripping cleanup mode, comment lines are published and scanned.
  s.git("config", "commit.cleanup", "whitespace");
  fs.writeFileSync(file, `Clean subject\n; ${TERM}\n`);
  blocked(s.run(["commit", "-q", "-F", file]));
});

test("the installer is idempotent and chains a git-ai style hook under its own name", (t) => {
  const s = sandbox(t);
  // As git-ai sets it up: an absolute local core.hooksPath and a hook that is a
  // relative symlink to a program that dispatches on its own name.
  const hooks = path.join(s.root, "managed-hooks");
  fs.mkdirSync(hooks);
  s.git("config", "core.hooksPath", hooks);
  const log = path.join(s.root, "chained.log");
  const program = path.join(s.root, "dispatcher");
  fs.writeFileSync(
    program,
    `#!/bin/sh\n{ basename "$0"; cat; } >>'${log}'\n[ ! -e '${log}.fail' ]\n`,
  );
  fs.chmodSync(program, 0o755);
  fs.symlinkSync(path.join("..", "dispatcher"), path.join(hooks, "pre-push"));
  fs.symlinkSync(path.join("..", "dispatcher"), path.join(hooks, "commit-msg"));

  const first = s.install();
  passed(first);
  assert.match(first.stderr, /moved the existing pre-push hook/);
  assert.match(first.stderr, /moved the existing commit-msg hook/);
  const hook = fs.readFileSync(path.join(hooks, "pre-push"), "utf8");
  assert.equal(fs.realpathSync(path.join(hooks, "chained", "pre-push")), fs.realpathSync(program));
  const again = s.install();
  passed(again);
  assert.doesNotMatch(again.stderr, /moved/);
  assert.equal(fs.readFileSync(path.join(hooks, "pre-push"), "utf8"), hook);
  assert.deepEqual(fs.readdirSync(path.join(hooks, "chained")).sort(), ["commit-msg", "pre-push"]);
  assert.ok(!fs.existsSync(path.join(s.work, ".git", "hooks", "pre-push")));

  const head = s.commit("a.txt", "clean\n");
  passed(s.run(["push", "-q", "public", "main"]));
  const [name, line] = fs.readFileSync(log, "utf8").split("\n");
  assert.equal(name, "pre-push");
  assert.match(line, new RegExp(`^refs/heads/main ${head} refs/heads/main 0+$`));
  s.stage("b.txt", "clean\n");
  passed(s.run(["commit", "-q", "-m", "Clean"]));
  assert.match(fs.readFileSync(log, "utf8"), /^commit-msg$/m);

  // A failing chained hook still blocks; a term is still blocked after it passes.
  fs.writeFileSync(`${log}.fail`, "");
  s.stage("c.txt", "clean\n");
  assert.notEqual(s.run(["commit", "-q", "-m", "Clean"]).status, 0);
  s.commit("c.txt", "clean\n");
  assert.notEqual(s.run(["push", "-q", "public", "main"]).status, 0);
  fs.rmSync(`${log}.fail`);
  s.commit("d.txt", `${TERM}\n`);
  blocked(s.run(["push", "-q", "public", "main"]));

  // A new foreign hook when one is already chained is never clobbered.
  fs.rmSync(path.join(hooks, "pre-push"));
  fs.writeFileSync(path.join(hooks, "pre-push"), "#!/bin/sh\nexit 0\n");
  const conflict = s.install();
  assert.notEqual(conflict.status, 0);
  assert.match(conflict.stderr, /merge them by hand/);
  assert.equal(fs.readFileSync(path.join(hooks, "pre-push"), "utf8"), "#!/bin/sh\nexit 0\n");
});

test("the installer warns when a chained hook is not executable", (t) => {
  const s = sandbox(t);
  fs.mkdirSync(s.hooks, { recursive: true });
  fs.writeFileSync(path.join(s.hooks, "pre-push"), "#!/bin/sh\nexit 1\n", { mode: 0o644 });
  const result = s.install();
  passed(result);
  assert.match(result.stderr, /is not executable/);
});

test("the installer never touches a global or system hooks path", (t) => {
  const s = sandbox(t);
  const shared = path.join(s.root, "global-hooks");
  const config = path.join(s.root, "global-config");
  fs.writeFileSync(config, `[core]\n\thooksPath = ${shared}\n`);
  const result = s.install({ GIT_CONFIG_GLOBAL: config });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /NOT installed/);
  assert.ok(!fs.existsSync(shared));
});

test("the installer refuses to run concurrently", (t) => {
  const s = sandbox(t);
  fs.mkdirSync(path.join(s.hooks, ".privacy-hooks-install.lock"), { recursive: true });
  const result = s.install();
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /another install is running/);
  assert.ok(!fs.existsSync(path.join(s.hooks, "pre-push")));
});

test("without node or the checkout's hooks, a public commit or push gets a notice unless required", (t) => {
  const s = sandbox(t);
  passed(s.install());
  const head = s.commit("a.txt", "clean\n");
  // A PATH with git and the system tools, but no node.
  const bin = path.join(s.root, "bin");
  fs.mkdirSync(bin);
  const gitPath = execFileSync("sh", ["-c", "command -v git"], { encoding: "utf8" }).trim();
  fs.symlinkSync(gitPath, path.join(bin, "git"));
  const noNode = { ...s.env, PATH: `${bin}:/usr/bin:/bin`, CODESENSE_PRIVACY_PUBLIC_REMOTES: "" };
  assert.notEqual(spawnSync("sh", ["-c", "command -v node"], { env: noNode }).status, 0);
  const message = path.join(s.root, "message.txt");
  fs.writeFileSync(message, "Clean\n");
  const global = path.join(s.root, "global.gitconfig");
  fs.writeFileSync(global, "[codesense]\n\tprivacyRequired = true\n");
  // The installed dispatcher, or the checkout's own hooks chained by hand.
  const dispatcher = s.hooks;
  const chained = path.join(SCRIPTS, "git-hooks");
  const runHook = (dir, name, args, env, input) =>
    spawnSync("sh", [path.join(dir, name), ...args], { cwd: s.work, encoding: "utf8", env, input });
  const push = (dir, url) => (env) =>
    runHook(
      dir,
      "pre-push",
      ["public", url],
      env,
      `refs/heads/main ${head} refs/heads/main ${"0".repeat(40)}\n`,
    );
  const commitMsg = (dir) => (env) => runHook(dir, "commit-msg", [message], env);
  // A public destination is allowed with a notice (CI scans it), and blocked when the
  // clone requires the private rules (env, local or global config). Any other
  // destination passes silently, required or not.
  // `sources` adds the other ways to require them: global and local config.
  const check = (run, isPublic, env, sources = false) => {
    const result = run(env);
    passed(result);
    if (isPublic) assert.match(result.stderr, UNSCANNED);
    else assert.equal(result.stderr, "");
    const strict = (e) => {
      const r = run(e);
      assert.equal(r.status !== 0, isPublic, r.stderr);
    };
    strict({ ...env, CODESENSE_PRIVACY_REQUIRED: "1" });
    if (!sources) return;
    strict({ ...env, GIT_CONFIG_GLOBAL: global });
    s.git("config", "codesense.privacyRequired", "true");
    strict(env);
    s.git("config", "--unset", "codesense.privacyRequired");
  };
  for (const url of [...PUBLIC_URLS, ...OTHER_URLS])
    check(push(dispatcher, url), isPublicRemote(url), noNode);
  // Extra public URLs count too.
  check(push(dispatcher, "/tmp/x.git"), true, {
    ...noNode,
    CODESENSE_PRIVACY_PUBLIC_REMOTES: "/tmp/x.git",
  });
  for (const dir of [dispatcher, chained]) {
    // The switches are read as git booleans here too.
    const run = push(dir, PUBLIC_URLS[0]);
    assert.notEqual(run({ ...noNode, CODESENSE_PRIVACY_REQUIRED: "maybe" }).status, 0);
    passed(run({ ...noNode, CODESENSE_PRIVACY_REQUIRED: "off" }));
    s.git("config", "codesense.privacyRequired", "maybe");
    assert.notEqual(run(noNode).status, 0);
    s.git("config", "--unset", "codesense.privacyRequired");
    check(push(dir, PUBLIC_URLS[0]), true, noNode, true);
    check(push(dir, OTHER_URLS[0]), false, noNode, true);
    // commit-msg looks at every remote.
    s.git("remote", "set-url", "public", s.bare);
    check(commitMsg(dir), false, noNode, true);
    s.git("remote", "set-url", "public", "https://github.com/codesensehq/project.git");
    check(commitMsg(dir), true, noNode, true);
  }
  // A checkout without the hook scripts falls back the same way, with node too.
  fs.rmSync(path.join(s.work, "scripts"));
  const withNode = { ...s.env, CODESENSE_PRIVACY_PUBLIC_REMOTES: "" };
  check(push(dispatcher, "git@github.com:example/project.git"), false, withNode);
  check(push(dispatcher, "git@github.com:CodeSenseHQ/project.git"), true, withNode);
  check(commitMsg(dispatcher), true, withNode);
});
