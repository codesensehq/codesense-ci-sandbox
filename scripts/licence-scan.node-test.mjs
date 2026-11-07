// SPDX-License-Identifier: MIT
import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";

import {
  DECISIONS,
  DENY,
  NOTICES,
  NPM_IGNORES,
  VENDORED,
  allowList,
  cargoCrates,
  cargoFindings,
  advisoryConfig,
  cargoExitProblem,
  classifyAdvisories,
  fallbackText,
  licenceTexts,
  main,
  needsDecision,
  npmFindings,
  npmIgnores,
  npmMetadataGaps,
  npmPackages,
  pnpmAuditConfigProblems,
  staleNpmIgnores,
  renderNotices,
  satisfiable,
  staleNotices,
  vendoredComponents,
} from "./licence-scan.mjs";
import { cvss3Score, cvss4Score, cvssRating, cvssScore } from "./lib/cvss.mjs";

const root = new URL("../", import.meta.url);
const read = (rel) => readFileSync(new URL(rel, root), "utf8");
const ALLOWED = allowList(read(DENY));

test("the allow list is deny.toml's [licenses].allow, the list cargo deny checks crates against", () => {
  for (const id of ["MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "ISC", "Unicode-3.0"])
    assert.ok(ALLOWED.has(id), id);
  assert.ok(!ALLOWED.has("GPL-3.0"));
  const toml =
    '[graph]\nallow = ["GPL-3.0"]\n\n[licenses]\n# "LGPL-2.1" is not allowed\nallow = [\n  "MIT", # the default\n  "ISC",\n]\n\n[licenses.private]\nignore = false\n';
  assert.deepEqual([...allowList(toml)], ["MIT", "ISC"]);
  assert.throws(() => allowList("[graph]\nexclude-dev = true\n"), /no \[licenses\] allow list/);
});

test("licence expressions: OR needs one allowed side, AND needs both, WITH needs the allowed pair", () => {
  for (const ok of [
    "MIT",
    "(MIT OR Apache-2.0)",
    "MIT/Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "Apache-2.0 WITH LLVM-exception OR MIT",
    "MIT AND ISC",
    "(MIT OR CC0-1.0) AND Unicode-3.0",
    "MIT or GPL-3.0",
    "ISC AND MIT",
  ])
    assert.equal(satisfiable(ok, ALLOWED), true, ok);
  for (const bad of [
    "GPL-3.0",
    "MIT AND GPL-3.0",
    "CC-BY-4.0",
    "OFL-1.1",
    "LGPL-2.1 WITH LLVM-exception",
    "Apache-2.0 WITH Other-exception",
    "MIT WITH",
    "SEE LICENSE IN LICENSE.md",
    "",
    undefined,
    "(MIT",
    "MIT OR",
    "UNLICENSED",
    "MIT AND LGPL-2.0-or-later",
  ])
    assert.equal(satisfiable(bad, ALLOWED), false, String(bad));
});

test("npm packages come from pnpm's licence-grouped JSON, one per version", () => {
  const json = {
    MIT: [
      {
        name: "a",
        versions: ["1.0.0", "2.0.0"],
        paths: ["/p/a1", "/p/a2"],
        license: "MIT",
        homepage: "https://a",
        author: "Ann <ann@example.org> (https://ann)",
      },
    ],
  };
  assert.deepEqual(npmPackages(json), [
    {
      ecosystem: "npm",
      name: "a",
      version: "1.0.0",
      licence: "MIT",
      url: "https://a",
      dir: "/p/a1",
      author: "Ann",
    },
    {
      ecosystem: "npm",
      name: "a",
      version: "2.0.0",
      licence: "MIT",
      url: "https://a",
      dir: "/p/a2",
      author: "Ann",
    },
  ]);
  assert.equal(
    npmPackages({ ISC: [{ name: "b", version: "1.0.0", path: "/p/b", author: { name: "Bo" } }] })[0]
      .author,
    "Bo",
  );
});

test("cargo crates are the ones cargo tree printed, once each, workspace members excluded", () => {
  const pkg = (name, extra = {}) => ({
    name,
    version: "1.0.0",
    license: "MIT",
    manifest_path: `/r/${name}/Cargo.toml`,
    authors: ["A <a@x>"],
    ...extra,
  });
  const meta = {
    packages: [
      pkg("app"),
      pkg("normal", { repository: "https://n" }),
      pkg("macro"),
      pkg("unlisted"),
    ],
  };
  const tree = [
    "app v1.0.0 (/w/app)",
    "normal v1.0.0",
    "macro v1.0.0 (proc-macro)",
    "normal v1.0.0 (*)",
    "",
  ].join("\n");
  assert.deepEqual(
    cargoCrates(tree, meta).map((p) => [p.name, p.licence, p.url, p.dir, p.author]),
    [
      ["normal", "MIT", "https://n", "/r/normal", "A"],
      ["macro", "MIT", "", "/r/macro", "A"],
    ],
  );
  assert.throws(() => cargoCrates("ghost v9.9.9\n", meta), /ghost@9\.9\.9/);
});

test("vendored components: every field and an existing licence file, else the run fails", () => {
  const good = {
    name: "x",
    version: "1",
    licence: "MIT",
    source: "https://x",
    shippedAs: "resources/x",
    licenceFile: "x.LICENSE",
  };
  const ok = vendoredComponents({ components: [good] }, () => true);
  assert.deepEqual(ok.errors, []);
  assert.deepEqual(ok.components[0], {
    ecosystem: "vendored",
    name: "x",
    version: "1",
    licence: "MIT",
    url: "https://x",
    shippedAs: "resources/x",
    licenceFile: "x.LICENSE",
  });
  const bad = vendoredComponents(
    { components: [{ ...good, licence: "" }, good] },
    (rel) => rel !== "x.LICENSE",
  );
  assert.equal(bad.errors.length, 3);
  assert.match(bad.errors[0], /has no licence/);
  assert.match(bad.errors[1], /x\.LICENSE does not exist/);
  assert.deepEqual(vendoredComponents({}, () => true).errors, [`${VENDORED} lists no components`]);
});

test("the checked-in vendored manifest is complete; its tracked licence files exist", () => {
  const { components, errors } = vendoredComponents(
    JSON.parse(read(VENDORED)),
    (rel) => rel.startsWith("node_modules/") || existsSync(new URL(rel, root)),
  );
  assert.deepEqual(errors, []);
  const names = components.map((c) => c.name);
  for (const n of [
    "Bun",
    "@xterm/headless",
    "emojibase-data",
    "starship",
    "git-ai",
    "@xterm/xterm",
    "Lucide icons",
    "Geist",
    "Universal UI kit (Excalidraw library)",
  ])
    assert.ok(names.includes(n), n);
  const licence = (n) => components.find((c) => c.name === n).licence;
  assert.equal(licence("starship"), "ISC");
  assert.equal(licence("Geist"), "OFL-1.1");
});

test("an owner decision covers one package and the exact licence it was made for", () => {
  const p = { ecosystem: "npm", name: "x", version: "1.0.0", licence: "CC-BY-4.0" };
  assert.deepEqual(needsDecision([p], ALLOWED), [p]);
  assert.deepEqual(
    needsDecision([p], ALLOWED, { "npm:x": { licence: "CC-BY-4.0", reason: "data only" } }),
    [],
  );
  assert.equal(
    needsDecision([{ ...p, licence: "GPL-3.0" }], ALLOWED, { "npm:x": { licence: "CC-BY-4.0" } })
      .length,
    1,
  );
  assert.deepEqual(needsDecision([{ ...p, licence: "MIT" }], ALLOWED), []);
  const v = { ecosystem: "vendored", name: "Geist", version: "5", licence: "OFL-1.1" };
  assert.deepEqual(needsDecision([v], ALLOWED, { "npm:Geist": { licence: "OFL-1.1" } }), [v]);
  assert.deepEqual(needsDecision([v], ALLOWED, { "vendored:Geist": { licence: "OFL-1.1" } }), []);
});

test("licence files are found by name, sorted, and normalised", () => {
  const files = {
    "LICENSE-MIT": "mit\r\n",
    "LICENSE-APACHE": "apache\n",
    "README.md": "no",
    COPYING: " c ",
  };
  const texts = licenceTexts(
    "/d",
    (p) => files[p.slice(3)],
    () => Object.keys(files),
  );
  assert.deepEqual(texts, ["c", "apache", "mit"]);
  assert.deepEqual(
    licenceTexts("/missing", undefined, () => {
      throw new Error("ENOENT");
    }),
    [],
  );
});

test("a package without a licence file falls back to its licence's standard text", () => {
  const mit = fallbackText({
    name: "a",
    version: "1.0.0",
    licence: "(Apache-2.0 OR MIT)",
    author: "Ann",
  });
  assert.match(mit, /^MIT License\n\nCopyright \(c\) Ann\n\nPermission is hereby granted/);
  assert.match(
    fallbackText({ name: "b", version: "1.0.0", licence: "ISC", author: "" }),
    /Copyright \(c\) the b authors/,
  );
  const other = fallbackText({ name: "c", version: "2.0.0", licence: "Apache-2.0", author: "" });
  assert.match(other, /c 2\.0\.0 ships no licence file\. Licence: Apache-2\.0\./);
  assert.match(other, /https:\/\/spdx\.org\/licenses\/Apache-2\.0\.html/);
  assert.match(
    fallbackText({ name: "d", version: "1", licence: "", author: "" }),
    /Licence: none declared\.$/,
  );
});

test("notices are deterministic, list vendored components, and print each licence text once, verbatim", () => {
  const ps = [
    { ecosystem: "cargo", name: "b", version: "1.0.0", licence: "MIT", url: "u|v", dir: "/b" },
    { ecosystem: "npm", name: "a", version: "1.0.0", licence: "", url: "", dir: "/a" },
    { ecosystem: "npm", name: "c", version: "2.0.0", licence: "ISC", url: "", dir: "/c" },
    {
      ecosystem: "vendored",
      name: "starship",
      version: "1.24.2",
      licence: "ISC",
      url: "https://s",
      shippedAs: "resources/starship",
      licenceFile: "s",
    },
  ];
  const notice = "# NOTICE\n\n```\nfenced licence\n```";
  const text = (p) => (p.ecosystem === "vendored" ? [notice] : ["same text"]);
  const out = renderNotices(ps, text);
  assert.equal(out, renderNotices([...ps].reverse(), text));
  assert.match(out, /\| a \| 1\.0\.0 \| none declared \|  \|/);
  assert.match(out, /\| b \| 1\.0\.0 \| MIT \| u\\\|v \|/);
  assert.match(
    out,
    /## Bundled and vendored components\n\n.*\n.*\n\| starship \| 1\.24\.2 \| ISC \| resources\/starship \| https:\/\/s \|/,
  );
  assert.equal(out.match(/same text/g).length, 1);
  assert.match(out, /### b 1\.0\.0, a 1\.0\.0, c 2\.0\.0/);
  assert.ok(out.includes(`### starship 1.24.2\n\n\`\`\`\`text\n${notice}\n\`\`\`\`\n`));
});

test("--check fails unless the notices at HEAD match the generated ones exactly", () => {
  assert.equal(staleNotices("# n\n", "# n\n"), null);
  assert.match(staleNotices(null, "# n\n"), /is not committed/);
  assert.match(staleNotices("# old\n", "# n\n"), /at HEAD differs/);
});

// A throwaway repo root for main(): the real deny.toml, a one-component vendored manifest, and the
// given files. Returns the root and a remover.
function fixtureRoot(files = {}) {
  const root = mkdtempSync(join(tmpdir(), "licence-scan-"));
  mkdirSync(join(root, "scripts"));
  const all = {
    [DENY]: read(DENY),
    [VENDORED]: JSON.stringify({
      components: [
        {
          name: "vend",
          version: "1",
          licence: "MIT",
          source: "https://vend",
          shippedAs: "resources/vend",
          licenceFile: "vend.LICENSE",
        },
      ],
    }),
    "vend.LICENSE": "MIT vend",
    ...files,
  };
  for (const [rel, text] of Object.entries(all)) {
    mkdirSync(dirname(join(root, rel)), { recursive: true });
    writeFileSync(join(root, rel), text);
  }
  return { root, done: () => rmSync(root, { recursive: true, force: true }) };
}

const spawnResult = (stdout = "", stderr = "", status = 0) => ({ status, stdout, stderr });

// Runs main() with stubbed cargo, pnpm and git: `npm` is pnpm's licence-grouped JSON for the
// production graph, and HEAD's notices are whatever the last non-check run wrote.
function runScan(root, argv, npm, t) {
  const calls = [];
  const errors = [];
  t.mock.method(console, "error", (m) => errors.push(m));
  t.mock.method(console, "log", () => {});
  const exec = (cmd, args) => {
    calls.push(`${cmd} ${args.join(" ")}`);
    if (cmd === "pnpm") return JSON.stringify(npm);
    if (args[0] === "metadata")
      return JSON.stringify({
        packages: [
          {
            name: "crate",
            version: "1.0.0",
            license: "MIT",
            manifest_path: `${root}/c/Cargo.toml`,
          },
        ],
      });
    return "crate v1.0.0\n";
  };
  const spawn = (cmd, args) => {
    calls.push(`${cmd} ${args.join(" ")}`);
    if (cmd === "git") {
      const file = join(root, NOTICES);
      return existsSync(file) ? spawnResult(readFileSync(file, "utf8")) : spawnResult("", "", 128);
    }
    return spawnResult();
  };
  const code = main(argv, { root, run: exec, spawn });
  t.mock.restoreAll();
  return { code, calls, errors };
}

test("a GPL production dependency fails --check; dev and test dependencies are out of scope by design", (t) => {
  const { root, done } = fixtureRoot();
  t.after(done);
  const mit = { MIT: [{ name: "ok-dep", versions: ["1.0.0"], paths: [`${root}/ok`] }] };
  const gpl = {
    ...mit,
    "GPL-3.0-only": [{ name: "planted-gpl", versions: ["2.0.0"], paths: [`${root}/gpl`] }],
  };

  assert.equal(runScan(root, [], mit, t).code, 0);
  const clean = runScan(root, ["--check"], mit, t);
  assert.equal(clean.code, 0, clean.errors.join("\n"));
  // Only the production graph is scanned: pnpm --prod and cargo's normal edges.
  assert.ok(clean.calls.includes("pnpm licenses list --json --prod"));
  assert.ok(clean.calls.some((c) => c.startsWith("cargo tree") && c.includes("--edges normal")));
  assert.match(read(DENY), /^exclude-dev = true$/m);

  // Plant the GPL dependency with notices regenerated, so the licence is the only failure.
  runScan(root, [], gpl, t);
  const planted = runScan(root, ["--check"], gpl, t);
  assert.equal(planted.code, 1);
  assert.ok(planted.errors.some((e) => /1 packages need an owner decision/.test(e)));
  assert.ok(planted.errors.includes("  npm planted-gpl 2.0.0: GPL-3.0-only"));
  assert.ok(!planted.errors.some((e) => /THIRD_PARTY_NOTICES/.test(e)));

  // Only an owner decision for exactly that package and licence clears it.
  writeFileSync(
    join(root, DECISIONS),
    JSON.stringify({ "npm:planted-gpl": { licence: "GPL-3.0-only", reason: "test" } }),
  );
  assert.equal(runScan(root, ["--check"], gpl, t).code, 0);
});

test("deny.toml [advisories]: unsound and unmaintained cover every crate; each ignore is a table with a reason", () => {
  assert.deepEqual(advisoryConfig(read(DENY)), { ignores: [] });
  const toml = (body, scopes = 'unsound = "all"\nunmaintained = "all"') =>
    `[licenses]\nallow = []\n\n[advisories]\nversion = 2\n${scopes}\n${body}\n[x]\n`;
  assert.deepEqual(
    advisoryConfig(
      toml(
        '# ignore = [{ id = "RUSTSEC-1", reason = "commented" }]\nignore = [\n  { id = "RUSTSEC-2024-0001", reason = "not reachable; T 2026-11-05" },\n  { crate = "yank@0.2.0", reason = "yanked for a typo" },\n]',
      ),
    ).ignores,
    [
      { id: "RUSTSEC-2024-0001", reason: "not reachable; T 2026-11-05" },
      { crate: "yank@0.2.0", reason: "yanked for a typo" },
    ],
  );
  const refused = /ignores need \{ id, reason \} or \{ crate, reason \}/;
  assert.throws(() => advisoryConfig(toml('ignore = ["RUSTSEC-2024-0001"]')), refused);
  assert.throws(
    () =>
      advisoryConfig(toml('ignore = [\n  "RUSTSEC-1",\n  { id = "RUSTSEC-2", reason = "r" },\n]')),
    refused,
  );
  assert.throws(() => advisoryConfig(toml('ignore = [{ id = "RUSTSEC-1" }]')), refused);
  assert.throws(
    () => advisoryConfig(toml('ignore = [{ id = "RUSTSEC-1", reason = " " }]')),
    refused,
  );
  // cargo deny 0.20 scopes unsound to the workspace by default, which skips transitive crates.
  for (const scopes of ['unmaintained = "all"', 'unsound = "workspace"\nunmaintained = "all"'])
    assert.throws(() => advisoryConfig(toml("ignore = []", scopes)), /unsound = "all"/);
  assert.throws(
    () => advisoryConfig(toml("ignore = []", 'unsound = "all"\nunmaintained = "transitive"')),
    /unmaintained = "all"/,
  );
  assert.throws(() => advisoryConfig("[licenses]\nallow = []\n"), /no \[advisories\] section/);
});

test("npm ignores: scripts/licence-scan.advisories.json entries need a reason", () => {
  assert.deepEqual(JSON.parse(read(NPM_IGNORES)), {});
  assert.deepEqual(npmIgnores({ "npm:GHSA-x": { reason: " not reachable " } }), [
    { id: "GHSA-x", reason: "not reachable" },
  ]);
  assert.throws(() => npmIgnores({ "npm:GHSA-x": {} }), /with a reason/);
  assert.throws(() => npmIgnores({ "GHSA-x": { reason: "r" } }), /with a reason/);
});

test("pnpm's own audit ignores are refused in every form; only an absent or empty auditConfig passes", () => {
  const yaml = (text) => pnpmAuditConfigProblems({ workspaceYaml: { source: "ws.yaml", text } });
  const pkg = (pnpm) =>
    pnpmAuditConfigProblems({
      manifests: [{ source: "package.json", text: JSON.stringify({ pnpm }) }],
    });
  const npmrc = (text) => pnpmAuditConfigProblems({ npmrcs: [{ source: ".npmrc", text }] });
  for (const ok of [
    "packages:\n  - .\n",
    "packages:\r\n  - .\r\n# auditConfig: { ignoreCves: [CVE-2024-0001] }\r\n",
    "auditConfig: {}\n",
    "auditConfig: {}\r\npackages: [.]\r\n",
  ])
    assert.deepEqual(yaml(ok), [], JSON.stringify(ok));
  for (const refused of [
    "auditConfig:\n  ignoreCves:\n    - CVE-2024-0001\n",
    "auditConfig:\r\n  ignoreGhsas: [GHSA-aaaa-2222-3333]\r\n",
    "auditConfig: { ignoreCves: [CVE-2024-0001] }\n",
    "auditConfig: {\n  ignoreCves: [CVE-2024-0001] }\n",
    "auditConfig:\n  ignoreCves: []\n",
    '"auditConfig":\n  ignoreCves: [CVE-2024-0001]\n',
    "'auditConfig': { ignoreCves: [CVE-2024-0001] }\n",
    "{ auditConfig: { ignoreCves: [CVE-2024-0001] }, packages: [.] }\n",
    "? auditConfig\n: ignoreCves: [CVE-2024-0001]\n",
    "auditConfig: &a\n  ignoreCves: [CVE-2024-0001]\n",
    "x:\n  auditConfig: {}\n",
    "auditConfig: {}\nauditConfig: {}\n",
    "audit-config:\n  ignore-cves: [CVE-2024-0001]\n",
  ])
    assert.match(yaml(refused).join(), /ws\.yaml auditConfig: pnpm drops/, JSON.stringify(refused));
  assert.deepEqual(pkg(undefined), []);
  assert.deepEqual(pkg({ auditConfig: {} }), []);
  assert.deepEqual(pkg({ auditConfig: { ignoreCves: [], ignoreGhsas: [] } }), []);
  for (const auditConfig of [
    { ignoreGhsas: ["GHSA-aaaa-2222-3333"] },
    { ignoreCves: "CVE-2024-0001" },
    { ignoreAll: [] },
    null,
    [],
  ])
    assert.equal(pkg({ auditConfig }).length, 1, JSON.stringify(auditConfig));
  assert.match(
    pnpmAuditConfigProblems({ manifests: [{ source: "p.json", text: "{" }] }).join(),
    /p\.json: cannot read it/,
  );
  assert.deepEqual(npmrc("engine-strict=true\r\nnode-linker=hoisted\r\n; ignore-cves=x\r\n"), []);
  for (const refused of [
    "ignore-ghsas=GHSA-aaaa-2222-3333\n",
    "ignore-ghsas[]=GHSA-aaaa-2222-3333\n",
    '"ignore-cves"=CVE-2024-0001\n',
    "engine-strict=true\r\nignore-cves=CVE-2024-0001\r\n",
    "auditConfig.ignoreGhsas=GHSA-aaaa-2222-3333\n",
    "audit-config={}\n",
    "audit-ignore-list=CVE-2024-0001\n",
  ])
    assert.equal(npmrc(refused).length, 1, JSON.stringify(refused));
  // The repo itself hides nothing from pnpm audit.
  const repoFile = (name) =>
    existsSync(new URL(name, root)) ? [{ source: name, text: read(name) }] : [];
  assert.deepEqual(
    pnpmAuditConfigProblems({
      manifests: repoFile("package.json"),
      workspaceYaml: repoFile("pnpm-workspace.yaml")[0] ?? null,
      npmrcs: repoFile(".npmrc"),
    }),
    [],
  );
});

test("CVSS 3.x and 4.0 base scores and ratings; anything else is unrated", () => {
  const v3 = (rest) => `CVSS:3.1/${rest}`;
  assert.equal(cvss3Score(v3("AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H")), 9.8);
  assert.equal(cvss3Score(v3("AV:N/AC:L/PR:N/UI:N/S:C/C:H/I:H/A:H")), 10);
  assert.equal(cvss3Score(v3("AV:N/AC:H/PR:N/UI:N/S:U/C:H/I:N/A:N")), 5.9);
  assert.equal(cvss3Score(v3("AV:L/AC:L/PR:L/UI:N/S:U/C:H/I:H/A:H")), 7.8);
  assert.equal(cvss3Score(v3("AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:N")), 0);
  assert.equal(cvss3Score(v3("AV:N/AC:L")), null);
  // FIRST reference values; CVE-2025-47273 (7.7) is the RustSec cvss crate's own test. The port
  // was also checked against Red Hat's cvss 3.6 on 5,000 random vectors, environmental included.
  const v4 = (rest) => `CVSS:4.0/${rest}`;
  for (const [vector, score] of [
    [v4("AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:N/SI:N/SA:N"), 9.3],
    [v4("AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:H/SI:H/SA:H"), 10],
    [v4("AV:N/AC:L/AT:N/PR:N/UI:N/VC:N/VI:N/VA:H/SC:N/SI:N/SA:N"), 8.7],
    [v4("AV:N/AC:L/AT:N/PR:N/UI:N/VC:N/VI:N/VA:L/SC:N/SI:N/SA:N"), 6.9],
    [v4("AV:L/AC:L/AT:N/PR:L/UI:N/VC:H/VI:H/VA:H/SC:N/SI:N/SA:N"), 8.5],
    [v4("AV:N/AC:L/AT:N/PR:N/UI:N/VC:N/VI:H/VA:N/SC:N/SI:N/SA:N/E:P/CR:X/IR:X/AR:X"), 7.7],
    [v4("AV:N/AC:L/AT:N/PR:N/UI:N/VC:N/VI:N/VA:N/SC:N/SI:N/SA:N"), 0],
  ])
    assert.equal(cvss4Score(vector), score, vector);
  assert.equal(cvss4Score(v4("AV:N/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:N/SI:N")), null);
  assert.equal(cvss4Score(v4("AV:Q/AC:L/AT:N/PR:N/UI:N/VC:H/VI:H/VA:H/SC:N/SI:N/SA:N")), null);
  assert.equal(cvssScore(v3("AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H")), 9.8);
  assert.equal(cvssScore(v4("AV:N/AC:L/AT:N/PR:N/UI:N/VC:N/VI:N/VA:H/SC:N/SI:N/SA:N")), 8.7);
  for (const other of ["CVSS:2.0/AV:N", "AV:N/AC:L", "", undefined, null])
    assert.equal(cvssScore(other), null, String(other));
  assert.deepEqual([9, 8.9, 7, 6.9, 4, 3.9, 0, null].map(cvssRating), [
    "critical",
    "high",
    "high",
    "medium",
    "medium",
    "low",
    "none",
    "unrated",
  ]);
});

const diag = (fields) => JSON.stringify({ type: "diagnostic", fields });
const vuln = (id, name, cvss, extra = {}) =>
  diag({
    code: "vulnerability",
    severity: "error",
    message: `${name} advisory`,
    notes: [`ID: ${id}`, `Solution: Upgrade to >=1.2.3 (try \`cargo update -p ${name}\`)`],
    graphs: [{ Krate: { name, version: "1.0.0" }, parents: [] }],
    advisory: { id, package: name, title: `${name} advisory`, aliases: [], cvss, ...extra },
  });
const CARGO_STDERR = [
  "2026-11-05 INFO fetching advisory database",
  vuln("RUSTSEC-2024-0001", "vulncrate", "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H", {
    aliases: ["CVE-2024-1", "GHSA-aaaa"],
    url: "https://example.org/a",
  }),
  vuln(
    "RUSTSEC-2024-0003",
    "v4crate",
    "CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:N/VI:N/VA:H/SC:N/SI:N/SA:N",
  ),
  diag({
    code: "unmaintained",
    severity: "error",
    message: "old is unmaintained",
    notes: ["ID: RUSTSEC-2024-0002", "Solution: No safe upgrade is available!"],
    graphs: [{ Krate: { name: "old", version: "0.1.0" } }],
    advisory: { id: "RUSTSEC-2024-0002", title: "old is unmaintained" },
  }),
  diag({
    code: "yanked",
    severity: "warning",
    message: "detected yanked crate",
    graphs: [{ Krate: { name: "yank", version: "0.2.0" } }],
  }),
  diag({ code: "advisory-not-detected", severity: "warning", message: "x" }),
  "{not json",
].join("\n");

test("cargo deny's JSON diagnostics become findings with id, severity (CVSS 3.x or 4.0) and fixed-in", () => {
  const [v3, v4, old, yank, ...rest] = cargoFindings(CARGO_STDERR);
  assert.equal(rest.length, 0);
  assert.deepEqual(
    [v3.package, v3.version, v3.id, v3.kind, v3.severity, v3.fixedIn, v3.url],
    [
      "vulncrate",
      "1.0.0",
      "RUSTSEC-2024-0001",
      "vulnerability",
      "critical",
      ">=1.2.3",
      "https://example.org/a",
    ],
  );
  assert.deepEqual(v3.aliases, ["CVE-2024-1", "GHSA-aaaa"]);
  assert.deepEqual([v4.package, v4.severity], ["v4crate", "high"]);
  assert.deepEqual(
    [old.package, old.id, old.severity, old.fixedIn, old.url],
    [
      "old",
      "RUSTSEC-2024-0002",
      "unmaintained",
      null,
      "https://rustsec.org/advisories/RUSTSEC-2024-0002",
    ],
  );
  assert.deepEqual(
    [yank.package, yank.version, yank.id, yank.severity],
    ["yank", "0.2.0", "yanked", "yanked"],
  );
});

test("cargo deny's exit: 0, or 1 when every error is an advisory finding; anything else fails", () => {
  assert.equal(cargoExitProblem(0, ""), null);
  assert.equal(cargoExitProblem(1, CARGO_STDERR), null);
  const fetchFailed = diag({ code: "index-failure", severity: "error", message: "no database" });
  assert.match(
    cargoExitProblem(1, `${CARGO_STDERR}\n${fetchFailed}`),
    /exited 1: index-failure: no database/,
  );
  const warningsOnly = diag({ code: "yanked", severity: "warning", message: "y" });
  assert.match(cargoExitProblem(1, warningsOnly), /exited 1/);
  assert.match(cargoExitProblem(2, CARGO_STDERR), /exited 2/);
  assert.match(cargoExitProblem(101, "error: failed to parse deny.toml"), /deny\.toml/);
});

const AUDIT = {
  advisories: {
    1100: {
      id: 1100,
      github_advisory_id: "GHSA-hhhh",
      module_name: "leftpad",
      severity: "high",
      title: "ReDoS",
      url: "https://github.com/advisories/GHSA-hhhh",
      patched_versions: ">=2.0.1",
      cves: ["CVE-2025-9"],
      findings: [
        { version: "2.0.0", paths: ["dep>leftpad"] },
        { version: "1.0.0", paths: ["other>leftpad"] },
      ],
    },
    1101: {
      id: 1101,
      module_name: "nofix",
      severity: "moderate",
      title: "Prototype pollution",
      patched_versions: "<0.0.0",
      findings: [{ version: "3.0.0", paths: ["nofix"] }],
    },
  },
  metadata: { vulnerabilities: { info: 0, low: 0, moderate: 1, high: 2, critical: 0 } },
};

test("pnpm audit's report becomes one finding per advisory and installed version", () => {
  const f = npmFindings(AUDIT);
  assert.deepEqual(
    f.map((x) => [x.package, x.version, x.id, x.severity, x.fixedIn, x.paths.join()]),
    [
      ["leftpad", "2.0.0", "GHSA-hhhh", "high", ">=2.0.1", "dep>leftpad"],
      ["leftpad", "1.0.0", "GHSA-hhhh", "high", ">=2.0.1", "other>leftpad"],
      ["nofix", "3.0.0", "1101", "medium", null, "nofix"],
    ],
  );
  assert.deepEqual(f[0].aliases, ["CVE-2025-9", "1100"]);
  assert.throws(
    () => npmFindings({ error: { code: "ENOTFOUND" } }),
    /no advisories report.*ENOTFOUND/,
  );
});

test("the registry's totals cross-check the advisories pnpm printed", () => {
  assert.deepEqual(npmMetadataGaps(AUDIT), []);
  // Fewer counted than shown is not a gap: the registry may count advisories, not paths.
  assert.deepEqual(
    npmMetadataGaps({ ...AUDIT, metadata: { vulnerabilities: { high: 1, moderate: 1 } } }),
    [],
  );
  assert.deepEqual(
    npmMetadataGaps({
      ...AUDIT,
      metadata: { vulnerabilities: { info: 0, low: 0, moderate: 1, high: 2, critical: 1 } },
    }),
    ["critical: the registry counts 1, the advisories show 0"],
  );
});

test("an npm ignore that matches no advisory in the report is stale; GHSA, CVE and npm ids all match", () => {
  const i = (id) => ({ id, reason: "r" });
  assert.deepEqual(
    staleNpmIgnores(AUDIT, [
      i("GHSA-hhhh"),
      i("CVE-2025-9"),
      i("1101"),
      i("GHSA-gone"),
      i("CVE-1"),
    ]),
    [i("GHSA-gone"), i("CVE-1")],
  );
  assert.deepEqual(staleNpmIgnores({ advisories: {} }, [i("GHSA-hhhh")]), [i("GHSA-hhhh")]);
});

test("blocking: critical, high, unrated and unsound unless ignored; Rust ignores match the RustSec id", () => {
  const base = {
    ecosystem: "cargo",
    package: "p",
    version: "1",
    aliases: [],
    kind: "vulnerability",
  };
  const f = (over) => ({ ...base, id: over.severity ?? over.kind, ...over });
  const out = classifyAdvisories(
    [
      f({ severity: "critical" }),
      f({ severity: "high" }),
      f({ severity: "unrated" }),
      f({ severity: "medium" }),
      f({ severity: "low" }),
      f({ kind: "unsound", severity: "unsound" }),
      f({ kind: "unmaintained", severity: "unmaintained" }),
      f({ kind: "yanked", package: "yank", version: "0.2.0", severity: "yanked" }),
      f({ id: "RUSTSEC-9", aliases: ["CVE-9"], severity: "high" }),
      f({ id: "RUSTSEC-8", aliases: ["CVE-8"], severity: "high" }),
      f({ ecosystem: "npm", id: "GHSA-9", aliases: ["CVE-9"], severity: "high" }),
      f({ package: "yank", version: "0.2.0", id: "RUSTSEC-7", severity: "high" }),
    ],
    {
      cargo: [
        { id: "RUSTSEC-9", reason: "not reachable" },
        // cargo deny does not honour an alias in deny.toml, so neither does the scan.
        { id: "CVE-8", reason: "alias" },
        { crate: "yank@0.2.0", reason: "typo release" },
      ],
      npm: [{ id: "CVE-9", reason: "npm alias" }],
    },
  );
  assert.deepEqual(
    out.map((x) => x.blocking),
    [true, true, true, false, false, true, false, false, false, true, false, true],
  );
  assert.deepEqual(out.map((x) => x.ignored).slice(7), [
    "typo release",
    "not reachable",
    null,
    "npm alias",
    null,
  ]);
  assert.match(out[0].reachable, /shipped graph/);
});

test("--advisories writes the findings file and fails only on unaddressed blocking findings", (t) => {
  const { root, done } = fixtureRoot();
  t.after(done);
  const scan = ({
    audit = AUDIT,
    denyStatus = 1,
    denyErr = CARGO_STDERR,
    argv = ["--advisories", "--out", "out/adv.json"],
  } = {}) => {
    const calls = [];
    const errors = [];
    t.mock.method(console, "error", (m) => errors.push(m));
    t.mock.method(console, "log", () => {});
    const spawn = (cmd, args) => {
      calls.push(`${cmd} ${args.join(" ")}`);
      if (cmd === "cargo") return spawnResult("", denyErr, denyStatus);
      if (cmd === "pnpm")
        return spawnResult(typeof audit === "string" ? audit : JSON.stringify(audit), "", 1);
      return spawnResult("abc123\n");
    };
    const code = main(argv, { root, spawn });
    t.mock.restoreAll();
    const out = join(root, "out/adv.json");
    const report = existsSync(out) ? JSON.parse(readFileSync(out, "utf8")) : null;
    rmSync(out, { force: true });
    return { code, calls, errors, report };
  };
  const setIgnores = (cargo, npm) => {
    writeFileSync(
      join(root, DENY),
      read(DENY).replace(/^ignore = \[\]$/m, `ignore = [\n${cargo.join(",\n")}\n]`),
    );
    writeFileSync(join(root, NPM_IGNORES), JSON.stringify(npm));
  };

  const first = scan();
  assert.equal(first.code, 1);
  assert.deepEqual(first.calls.slice(0, 2), [
    "cargo deny --format json check advisories",
    "pnpm audit --json --prod",
  ]);
  assert.equal(first.report.head, "abc123");
  assert.deepEqual(first.report.errors, []);
  assert.equal(first.report.summary.blocking, 4); // vulncrate critical, v4crate high, leftpad high x2
  assert.equal(first.report.findings.length, 7);
  assert.ok(first.report.findings.slice(0, 4).every((f) => f.blocking));

  // cargo deny drops ignored advisories from its output; the findings file still lists each.
  setIgnores(
    [
      '{ id = "RUSTSEC-2024-0001", reason = "fixture: not reachable" }',
      '{ id = "RUSTSEC-2024-0003", reason = "fixture: replaced" }',
      '{ id = "RUSTSEC-2023-0099", reason = "fixture: gone" }',
    ],
    { "npm:GHSA-hhhh": { reason: "fixture" } },
  );
  const addressed = scan();
  assert.equal(addressed.code, 0, addressed.errors.join("\n"));
  assert.equal(addressed.report.summary.blocking, 0);
  const gone = addressed.report.findings.find((f) => f.id === "RUSTSEC-2023-0099");
  assert.deepEqual(
    [gone.ecosystem, gone.ignored, gone.blocking, gone.title],
    ["cargo", "fixture: gone", false, "ignored by deny.toml [advisories] (not re-checked)"],
  );
  assert.equal(addressed.report.findings.filter((f) => f.ignored).length, 5);

  // pnpm's own auditConfig hides advisories from the report, so any pnpm-level ignore fails, in
  // the root package.json or a pnpm-workspace.yaml in a parent directory.
  writeFileSync(
    join(root, "package.json"),
    JSON.stringify({ pnpm: { auditConfig: { ignoreGhsas: ["GHSA-zzzz-2222-3333"] } } }),
  );
  const pkgIgnore = scan();
  assert.equal(pkgIgnore.code, 1);
  assert.match(pkgIgnore.report.errors.join(), /package\.json pnpm\.auditConfig: pnpm drops/);
  rmSync(join(root, "package.json"));
  const nested = join(root, "app");
  mkdirSync(join(nested, "scripts"), { recursive: true });
  writeFileSync(join(nested, DENY), readFileSync(join(root, DENY), "utf8"));
  writeFileSync(join(nested, NPM_IGNORES), "{}");
  writeFileSync(
    join(root, "pnpm-workspace.yaml"),
    "auditConfig:\r\n  ignoreCves: [CVE-2024-0001]\r\n",
  );
  writeFileSync(
    join(root, "package.json"),
    JSON.stringify({ pnpm: { auditConfig: { ignoreCves: ["CVE-2024-0002"] } } }),
  );
  t.mock.method(console, "error", () => {});
  t.mock.method(console, "log", () => {});
  const clean = JSON.stringify({ advisories: {}, metadata: { vulnerabilities: {} } });
  const code = main(["--advisories", "--out", "adv.json"], {
    root: nested,
    spawn: (cmd) => spawnResult(cmd === "pnpm" ? clean : "", "", 0),
  });
  t.mock.restoreAll();
  const parent = JSON.parse(readFileSync(join(nested, "adv.json"), "utf8"));
  assert.equal(code, 1);
  assert.equal(parent.errors.length, 2, parent.errors.join("\n"));
  assert.match(parent.errors[0], /package\.json pnpm\.auditConfig: pnpm drops/);
  assert.match(parent.errors[1], /pnpm-workspace\.yaml auditConfig: pnpm drops/);
  rmSync(join(root, "pnpm-workspace.yaml"));
  rmSync(join(root, "package.json"));

  // A reasoned ignore whose advisory is gone from the report is stale and fails.
  setIgnores(
    ['{ id = "RUSTSEC-2024-0001", reason = "r" }', '{ id = "RUSTSEC-2024-0003", reason = "r" }'],
    { "npm:GHSA-hhhh": { reason: "fixture" }, "npm:GHSA-gone-2222-3333": { reason: "old" } },
  );
  const stale = scan();
  assert.equal(stale.code, 1);
  assert.deepEqual(stale.report.errors, [
    `${NPM_IGNORES} npm:GHSA-gone-2222-3333 matches no advisory in the report: remove the stale ignore`,
  ]);
  setIgnores([], {});

  const hiddenAudit = {
    ...AUDIT,
    metadata: { vulnerabilities: { ...AUDIT.metadata.vulnerabilities, critical: 1 } },
  };
  // Without a configured ignore, totals above what pnpm printed mean something hid advisories.
  const gap = scan({ audit: hiddenAudit });
  assert.equal(gap.code, 1);
  assert.match(gap.report.errors.join(), /pnpm audit hides advisories \(critical/);

  // A run that could not read a database, or an unexpected cargo deny exit, fails.
  assert.equal(
    scan({ denyStatus: 2, denyErr: "error: failed to fetch advisory database" }).code,
    1,
  );
  assert.equal(scan({ denyStatus: 1, denyErr: "" }).code, 1);
  const offline = scan({ audit: '{"error":{"code":"ENOTFOUND"}}', denyStatus: 0, denyErr: "" });
  assert.equal(offline.code, 1);
  assert.match(offline.report.errors[0], /pnpm audit/);

  // --out needs a value.
  for (const argv of [
    ["--advisories", "--out"],
    ["--advisories", "--out", "--check"],
  ]) {
    const r = scan({ argv });
    assert.equal(r.code, 2);
    assert.equal(r.calls.length, 0);
    assert.match(r.errors.join(), /--out needs a file\nusage:/);
  }
});
