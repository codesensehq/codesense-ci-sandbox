// SPDX-License-Identifier: MIT
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync, spawnSync } from "node:child_process";
import { scanChange, addedLines, pairCommitSections, addedLineSelection } from "./privacy-ci.mjs";
import { scan, allowLines } from "./privacy-engine.mjs";

const token = () => "gh" + "p_" + "aB3".repeat(12);
const bad = (findings) => findings.some((f) => f.sev === "error" && !f.allowed);

function fixture(t, initial = "clean\n") {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "privacy-ci-test-"));
  const inputs = fs.mkdtempSync(path.join(os.tmpdir(), "privacy-ci-input-"));
  t.after(() => {
    fs.rmSync(dir, { recursive: true, force: true });
    fs.rmSync(inputs, { recursive: true, force: true });
  });
  const git = (...args) =>
    execFileSync("git", ["--literal-pathspecs", ...args], {
      cwd: dir,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
      env: { ...process.env, GIT_CONFIG_GLOBAL: os.devNull, GIT_CONFIG_NOSYSTEM: "1" },
    });
  const put = (file, value) => {
    fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
    fs.writeFileSync(path.join(dir, file), value);
  };
  const head = () => git("rev-parse", "HEAD").trim();
  const commit = (files, message = "Fixture change") => {
    git("add", "--", ...files);
    git("commit", "-m", message);
    return head();
  };
  git("init", "--initial-branch=main");
  git("config", "user.name", "Fixture");
  git("config", "user.email", "fixture@example.com");
  git("config", "commit.gpgsign", "false");
  put("a.txt", initial);
  const base = commit(["a.txt"], "Initial fixture");
  const metadata = path.join(inputs, "metadata.txt");
  const allowFile = path.join(inputs, "allow");
  fs.writeFileSync(metadata, "Clean request");
  fs.writeFileSync(allowFile, "");
  const change = (options = {}) =>
    scanChange({
      cwd: dir,
      base,
      head: head(),
      branch: "fixture-topic",
      metadata,
      allowFile,
      defaultBranch: "main",
      ...options,
    });
  const cli = (...flags) =>
    spawnSync(
      process.execPath,
      [new URL("./privacy-ci.mjs", import.meta.url).pathname, "--allow", allowFile, ...flags],
      {
        cwd: dir,
        encoding: "utf8",
        env: {
          ...process.env,
          SCAN_BASE: base,
          SCAN_HEAD: head(),
          SCAN_BRANCH: "fixture-topic",
          PR_METADATA_FILE: metadata,
        },
      },
    );
  return { dir, inputs, git, put, head, commit, base, metadata, allowFile, change, cli };
}

test("added-lines parser keeps content resembling a diff header", () => {
  assert.equal(addedLines("diff --git a/a b/a\n+++ b/a\n@@ -0,0 +1 @@\n++++value\n"), "+++value");
});

for (const surface of [
  "tree",
  "added-then-removed",
  "message",
  "early-message",
  "title",
  "body",
  "branch",
]) {
  test(`change scan rejects a planted key in ${surface}`, (t) => {
    const f = fixture(t);
    f.put("a.txt", surface === "added-then-removed" ? token() : "changed\n");
    const planted = f.commit(
      ["a.txt"],
      ["message", "early-message"].includes(surface) ? token() : "Fixture change",
    );
    if (["added-then-removed", "early-message"].includes(surface)) {
      f.put("a.txt", "clean again\n");
      f.commit(["a.txt"], "Later fixture");
    }
    if (surface === "tree") f.put("untracked.txt", token());
    fs.writeFileSync(
      f.metadata,
      surface === "title"
        ? `${token()}\nClean body`
        : surface === "body"
          ? `Clean title\n${token()}`
          : "Clean request",
    );
    const findings = f.change({ branch: surface === "branch" ? token() : "fixture-topic" });
    assert.ok(bad(findings));
    if (surface === "added-then-removed") {
      assert.ok(
        findings.some(
          (x) => x.commit === planted && x.file === "a.txt" && x.line === 1 && x.rule === "token",
        ),
      );
    }
  });
}

test("clean history passes", (t) => {
  const f = fixture(t);
  f.put("a.txt", "Clean update");
  f.commit(["a.txt"]);
  assert.equal(bad(f.change()), false);
});

test("historical additions retain real paths, tree severities and generated-notice exemptions", (t) => {
  const f = fixture(t);
  const text = ["SV", "PBF", "999"].join("-");
  f.put("src/a name.ts", `// ${text}\n`);
  f.put("THIRD_PARTY_NOTICES.md", ["author", "package-vendor.com"].join("@") + "\n");
  const commit = f.commit(["src/a name.ts", "THIRD_PARTY_NOTICES.md"]);
  const findings = f.change();
  assert.equal(bad(findings), false);
  assert.ok(
    findings.some((x) => x.commit === commit && x.file === "src/a name.ts" && x.sev === "warn"),
  );
  assert.ok(
    findings.some(
      (x) => x.commit === commit && x.file === "THIRD_PARTY_NOTICES.md" && x.allowed && x.exempt,
    ),
  );
  f.put("note.md", text);
  const doc = f.commit(["note.md"]);
  assert.ok(
    f
      .change()
      .some((x) => x.commit === doc && x.file === "note.md" && x.sev === "error" && !x.allowed),
  );
});

test("historical additions use real-path fingerprints without staling unused allowances", (t) => {
  const f = fixture(t);
  f.put("src/fixture.ts", `const value = "${token()}";\n`);
  const commit = f.commit(["src/fixture.ts"]);
  const candidates = scan(f.dir, new Map(), { skipRoot: [".git"] }).filter((x) => x.fp);
  fs.writeFileSync(f.allowFile, allowLines(candidates, () => "Reviewed fixture").join("\n"));
  const findings = f.change();
  assert.equal(bad(findings), false);
  assert.ok(findings.some((x) => x.commit === commit && x.file === "src/fixture.ts" && x.allowed));
});

test("a stale allow entry fails by default and is a counted warning with --stale-warn", (t) => {
  const f = fixture(t);
  f.put("src/fixture.ts", `const value = "${token()}";\n`);
  const candidates = scan(f.dir, new Map(), { skipRoot: [".git"] }).filter((x) => x.fp);
  fs.writeFileSync(f.allowFile, allowLines(candidates, () => "Reviewed fixture").join("\n"));
  fs.rmSync(path.join(f.dir, "src/fixture.ts"));
  const stale = (findings) => findings.filter((x) => x.rule === "allow-stale");
  const plain = f.change();
  const warned = f.change({ staleWarn: true });
  assert.equal(stale(plain).length, 1);
  assert.equal(bad(plain), true);
  assert.equal(bad(warned), false);
  assert.deepEqual(
    stale(warned).map((x) => x.sev),
    ["warn"],
  );
  const result = f.cli("--stale-warn");
  assert.equal(result.status, 0);
  assert.match(result.stdout, /^privacy-scan: stale allow entries=1 \(warnings\)$/m);
  assert.doesNotMatch(result.stdout, /^STALE /m);
  const shown = f.cli("--stale-warn", "--show");
  assert.match(shown.stdout, /^STALE src\/fixture\.ts \S+ \| fp:[0-9a-f]{20} used 0 of 1$/m);
  const strict = f.cli();
  assert.equal(strict.status, 1);
  assert.doesNotMatch(strict.stdout, /stale allow entries/);
  // A real finding next to the stale entry stays an error.
  f.put("src/other.ts", `const value = "${token()}";\n`);
  assert.equal(bad(f.change({ staleWarn: true })), true);
  assert.equal(f.cli("--stale-warn").status, 1);
});

test("--stale-warn downgrades only the engine's stale entries, not a rule that shares the name", (t) => {
  const f = fixture(t);
  const inventory = path.join(f.inputs, "terms.json");
  const layout = [
    [5, "error", "g"],
    [6, "error", "gi"],
    [7, "error", "gi"],
    [8, "error", "gi"],
    [9, "error", "g"],
    [24, "text", "g"],
  ];
  const rules = layout.map(([order, sev, flags], i) => ({
    id: i ? `fixture-${i}` : "allow-stale",
    sev,
    kind: "show",
    source: `qqstale${i}qq`,
    flags,
    order,
    classification: "private-only",
  }));
  fs.writeFileSync(inventory, JSON.stringify({ version: 1, rules }));
  f.put("notes.txt", "qqstale0qq\n");
  const findings = f.change({ staleWarn: true, privateRulesFile: inventory });
  assert.ok(findings.some((x) => x.rule === "allow-stale" && x.sev === "error" && !x.allowed));
  assert.equal(bad(findings), true);
});

test("a merge-only addition remains blocked after a later removal", (t) => {
  const f = fixture(t);
  f.git("checkout", "-b", "topic");
  f.put("topic.txt", "Topic change");
  f.commit(["topic.txt"]);
  f.git("checkout", "main");
  f.put("main.txt", "Main change");
  f.commit(["main.txt"]);
  f.git("merge", "--no-ff", "--no-commit", "topic");
  f.put("merge.txt", token());
  const merge = f.commit(["merge.txt"], "Merge fixture");
  f.put("merge.txt", "Clean again");
  f.commit(["merge.txt"]);
  assert.ok(
    f
      .change()
      .some(
        (x) => x.commit === merge && x.file === "merge.txt" && x.rule === "token" && !x.allowed,
      ),
  );
});

test("zero before with the fetched default tip scans through the root commit", (t) => {
  const f = fixture(t, token());
  f.put("a.txt", "Clean now");
  f.commit(["a.txt"]);
  f.git("update-ref", "refs/remotes/origin/main", f.head());
  assert.ok(
    f
      .change({ base: "0".repeat(40) })
      .some((x) => x.commit === f.base && x.rule === "token" && !x.allowed),
  );
});

test("a non-ancestor before uses the default-branch merge-base", (t) => {
  const f = fixture(t);
  f.put("a.txt", "Old tip");
  const old = f.commit(["a.txt"]);
  f.git("checkout", "-b", "rewritten", f.base);
  f.put("a.txt", token());
  const planted = f.commit(["a.txt"]);
  f.put("a.txt", "Clean now");
  f.commit(["a.txt"]);
  f.git("update-ref", "refs/remotes/origin/main", old);
  assert.ok(
    f.change({ base: old }).some((x) => x.commit === planted && x.rule === "token" && !x.allowed),
  );
});

test("no resolvable default-branch base fails closed", (t) => {
  const f = fixture(t);
  assert.throws(() => f.change({ base: "0".repeat(40) }));
  f.git("checkout", "--orphan", "unrelated");
  f.put("a.txt", "Unrelated root");
  f.commit(["a.txt"]);
  f.git("update-ref", "refs/remotes/origin/main", f.base);
  assert.throws(() => f.change({ base: f.base }));
});

test("public CLI never logs planted content or filenames", (t) => {
  const f = fixture(t);
  f.put(`${token()}.txt`, token());
  const result = spawnSync(
    process.execPath,
    [new URL("./privacy-scan.mjs", import.meta.url).pathname, f.dir],
    { encoding: "utf8" },
  );
  assert.equal(result.status, 1);
  assert.ok(!(result.stdout + result.stderr).includes(token()));
});

test("root checkout metadata can be skipped but nested metadata always fails", (t) => {
  const f = fixture(t);
  assert.equal(bad(scan(f.dir, new Map(), { skipRoot: [".git"] })), false);
  fs.mkdirSync(path.join(f.dir, "nested", ".git"), { recursive: true });
  assert.equal(bad(scan(f.dir, new Map(), { skipRoot: [".git"] })), true);
});

test("local --show is the explicit opt-in for public scanner details", (t) => {
  const f = fixture(t);
  f.put(`${token()}.txt`, token());
  const result = spawnSync(
    process.execPath,
    [new URL("./privacy-scan.mjs", import.meta.url).pathname, f.dir, "--show"],
    { encoding: "utf8" },
  );
  assert.equal(result.status, 1);
  assert.ok(result.stdout.includes(token()));
});

test("batched commit patches retain literal paths across multiple files", (t) => {
  const f = fixture(t);
  const files = ["src/[draft].ts", "src/d.ts", "src/a name.ts"];
  f.put(files[0], token());
  f.put(files[1], "Clean ordinary file");
  f.put(files[2], "Clean spaced file");
  const planted = f.commit(files);
  for (const file of files) f.put(file, "Clean again");
  f.commit(files);
  assert.ok(
    f
      .change()
      .some(
        (finding) =>
          finding.commit === planted &&
          finding.file === files[0] &&
          finding.rule === "token" &&
          !finding.allowed,
      ),
  );
});

test("gitlink entries fail explicitly instead of being read as blobs", (t) => {
  const f = fixture(t);
  f.git("update-index", "--add", "--cacheinfo", "160000", f.base, "vendor/component");
  f.git("commit", "-m", "Fixture gitlink");
  assert.throws(() => f.change(), /gitlink entries are unsupported/);
  const result = f.cli();
  assert.equal(result.status, 1);
  assert.ok(result.stderr.includes("gitlink entries are unsupported"));
});

test("binary attributes cannot hide an added-then-removed text key", (t) => {
  const f = fixture(t);
  f.put(".gitattributes", "* -diff\n");
  f.commit([".gitattributes"]);
  f.put("hidden.txt", token());
  const planted = f.commit(["hidden.txt"]);
  f.git("rm", "--", "hidden.txt");
  f.git("commit", "-m", "Remove fixture");
  assert.ok(
    f
      .change()
      .some(
        (finding) =>
          finding.commit === planted &&
          finding.file === "hidden.txt" &&
          finding.rule === "token" &&
          !finding.allowed,
      ),
  );
});

for (const blocked of [false, true]) {
  test(`file-to-symlink sections pair by path (${blocked ? "planted" : "clean"} target)`, (t) => {
    const f = fixture(t);
    f.put("link.txt", "Original regular file");
    f.put("target.txt", "Clean target");
    f.commit(["link.txt", "target.txt"]);
    fs.unlinkSync(path.join(f.dir, "link.txt"));
    fs.symlinkSync(blocked ? token() : "target.txt", path.join(f.dir, "link.txt"));
    const changed = f.commit(["link.txt"]);
    const findings = f.change();
    if (blocked) {
      assert.ok(
        findings.some(
          (finding) =>
            finding.commit === changed &&
            finding.file === "link.txt" &&
            finding.rule === "token" &&
            !finding.allowed,
        ),
      );
    } else assert.equal(bad(findings), false);
  });
}

test("hunkless content changes select the full blob; pure mode changes do not", () => {
  const sections = ["diff --git a/file.txt b/file.txt\nBinary files differ\n"];
  const change = { oldMode: "100644", mode: "100644", oldOid: "a".repeat(40), oid: "b".repeat(40) };
  assert.equal(addedLineSelection(change, sections), null);
  assert.equal(
    addedLineSelection(change, [
      ...sections,
      "diff --git a/file.txt b/file.txt\n@@ -1 +1 @@\n-old\n+new\n",
    ]),
    null,
  );
  assert.deepEqual(
    addedLineSelection({ ...change, mode: "100755", oid: change.oldOid }, sections),
    new Set(),
  );
});

test("diff sections whose header path is absent from the raw records fail closed", () => {
  assert.throws(() =>
    pairCommitSections([{ file: "expected.txt" }], "diff --git a/other.txt b/other.txt\n"),
  );
});
