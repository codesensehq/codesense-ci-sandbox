// SPDX-License-Identifier: MIT
// Usage: node scripts/licence-scan.mjs [--check]
//        node scripts/licence-scan.mjs --advisories [--out <file>]
// Licence scan of what the app ships, with deny.toml's [licenses].allow as the one policy:
// - Rust crates: `cargo deny check licenses` (deny.toml) is the gate; `cargo tree --edges normal`
//   lists the crates the app links, for the notices.
// - npm packages: `pnpm licenses list --json --prod`.
// - Vendored and bundled components the package managers do not see:
//   release/third-party.vendored.json.
// Dev and test dependencies are out of scope on purpose: they do not ship (deny.toml
// `exclude-dev`, pnpm `--prod`), so a GPL test dependency passes and a GPL production one fails.
// An npm package or a vendored component passes when its licence expression can be satisfied from
// the allow list, or when the owner recorded a decision for exactly that name and licence in
// scripts/licence-scan.decisions.json. Writes THIRD_PARTY_NOTICES.md (each package, then each
// distinct licence text once). A package directory with no licence file is listed, and the notices
// fall back to the standard text of its licence. --check writes nothing and also fails when the
// committed file (HEAD:THIRD_PARTY_NOTICES.md) is missing or differs from what this run generates,
// so the gate checks what ships, not an uncommitted copy. Exits 1 listing everything that needs a
// decision. --check works offline.
// --advisories is a separate mode that needs the network (the RustSec database and the npm
// registry): `cargo deny --format json check advisories` and `pnpm audit --json --prod`. It writes
// every finding (package, id, severity, fixed-in, reachability) to --out (default
// target/advisories.json) and exits 1 when a finding blocks under the policy in deny.toml
// [advisories]: critical, high or unrated vulnerabilities and unsound crates, unless ignored with a
// reason (Rust: deny.toml [advisories].ignore; npm: scripts/licence-scan.advisories.json).
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { cvssRating, cvssScore } from "./lib/cvss.mjs";

export const NOTICES = "THIRD_PARTY_NOTICES.md";
export const DECISIONS = "scripts/licence-scan.decisions.json";
export const DENY = "deny.toml";
export const VENDORED = "release/third-party.vendored.json";
export const NPM_IGNORES = "scripts/licence-scan.advisories.json";
export const ADVISORIES_OUT = "target/advisories.json";
const ROOT = fileURLToPath(new URL("../", import.meta.url));

// deny.toml's [licenses].allow: licence ids, and `X WITH <exception>` pairs.
export function allowList(denyToml) {
  const section = denyToml.match(/^\[licenses\]\s*$([\s\S]*?)(?=^\[|(?![\s\S]))/m)?.[1] ?? "";
  const array = section.match(/^allow\s*=\s*\[([\s\S]*?)\]/m)?.[1];
  if (array === undefined) throw new Error(`${DENY} has no [licenses] allow list`);
  const ids = array
    .split("\n")
    .map((line) => line.replace(/#.*/, ""))
    .join("\n")
    .match(/"[^"]+"/g);
  return new Set((ids ?? []).map((s) => s.slice(1, -1)));
}

function tokens(expr) {
  return expr
    .replace(/\//g, " OR ") // cargo's legacy "MIT/Apache-2.0"
    .replace(/([()])/g, " $1 ")
    .trim()
    .split(/\s+/)
    .filter(Boolean);
}

// True when the SPDX expression can be satisfied from `allowed`: OR needs one side, AND both, and
// `X WITH <exception>` needs that exact pair in the list. Malformed or empty expressions are never
// satisfied.
export function satisfiable(expr, allowed) {
  if (typeof expr !== "string" || !expr.trim()) return false;
  const t = tokens(expr);
  let i = 0;
  const op = (name) => t[i]?.toUpperCase() === name && (i += 1);
  const primary = () => {
    if (t[i] === "(") {
      i += 1;
      const v = or();
      if (t[i] !== ")") throw new Error("unbalanced");
      i += 1;
      return v;
    }
    const id = t[i];
    if (!id || id === ")" || ["AND", "OR", "WITH"].includes(id.toUpperCase()))
      throw new Error("expected a licence");
    i += 1;
    const base = id.replace(/\+$/, "");
    if (op("WITH")) {
      const exception = t[i];
      if (!exception || exception === "(" || exception === ")")
        throw new Error("expected an exception");
      i += 1;
      return allowed.has(`${base} WITH ${exception}`);
    }
    return allowed.has(base);
  };
  const and = () => {
    let v = primary();
    while (op("AND")) v = primary() && v;
    return v;
  };
  const or = () => {
    let v = and();
    while (op("OR")) v = and() || v;
    return v;
  };
  try {
    const v = or();
    return i === t.length && v;
  } catch {
    return false;
  }
}

// An author's name for a copyright line, without the "<email>" and "(url)" parts.
const authorOf = (a) =>
  (typeof a === "string" ? a : (a?.name ?? ""))
    .replace(/<[^>]*>|\([^)]*\)/g, "")
    .replace(/\s+/g, " ")
    .trim();

// pnpm 9+ groups by licence: { "<licence>": [{ name, versions: [], paths: [], homepage, author }] }.
export function npmPackages(json) {
  const out = [];
  for (const [licence, entries] of Object.entries(json)) {
    for (const e of entries) {
      const versions = e.versions ?? [e.version];
      const paths = e.paths ?? [e.path];
      versions.forEach((version, k) =>
        out.push({
          ecosystem: "npm",
          name: e.name,
          version,
          licence: e.license ?? licence,
          url: e.homepage ?? "",
          dir: paths[k] ?? paths[0],
          author: authorOf(e.author),
        }),
      );
    }
  }
  return out;
}

// The crates `cargo tree --prefix none --format {p}` printed, workspace members (local paths)
// excluded, with their licence, source and directory from `cargo metadata`.
export function cargoCrates(tree, meta) {
  const byKey = new Map(meta.packages.map((p) => [`${p.name}@${p.version}`, p]));
  const keys = new Set();
  for (const line of tree.split("\n")) {
    const m = line.match(/^(\S+) v(\S+)(.*)$/);
    if (m && !/\((?:\/|[A-Za-z]:\\)/.test(m[3])) keys.add(`${m[1]}@${m[2]}`);
  }
  return [...keys].map((key) => {
    const p = byKey.get(key);
    if (!p) throw new Error(`cargo tree listed ${key}, which cargo metadata does not have`);
    return {
      ecosystem: "cargo",
      name: p.name,
      version: p.version,
      licence: p.license ?? "",
      url: p.repository ?? p.homepage ?? "",
      dir: dirname(p.manifest_path),
      author: (p.authors ?? []).map(authorOf).filter(Boolean).join(", "),
    };
  });
}

const FIELDS = ["name", "version", "licence", "source", "shippedAs", "licenceFile"];

// The manifest's components, and every problem with it: a missing field or a licenceFile that
// does not exist. Any problem fails the run.
export function vendoredComponents(manifest, exists) {
  const errors = [];
  const components = (manifest?.components ?? []).map((c, k) => {
    for (const f of FIELDS)
      if (typeof c[f] !== "string" || !c[f].trim())
        errors.push(`${VENDORED} component ${k} (${c.name ?? "?"}) has no ${f}`);
    if (typeof c.licenceFile === "string" && c.licenceFile && !exists(c.licenceFile))
      errors.push(`${VENDORED} ${c.name}: licenceFile ${c.licenceFile} does not exist`);
    return {
      ecosystem: "vendored",
      name: c.name,
      version: c.version,
      licence: c.licence,
      url: c.source,
      shippedAs: c.shippedAs,
      licenceFile: c.licenceFile,
    };
  });
  if (!components.length) errors.push(`${VENDORED} lists no components`);
  return { components, errors };
}

const decisionKey = (p) => `${p.ecosystem}:${p.name}`;

// Packages whose licence is not satisfiable and has no matching owner decision. A decision
// covers one package and the exact licence string it was made for, so a licence change asks again.
export function needsDecision(packages, allowed, decisions = {}) {
  return packages.filter(
    (p) => !satisfiable(p.licence, allowed) && decisions[decisionKey(p)]?.licence !== p.licence,
  );
}

const LICENCE_FILE = /^(?:licen[cs]e|copying|notice|unlicense)(?:[-_.].*)?$/i;

export function licenceTexts(dir, read = readFileSync, list = readdirSync) {
  let names;
  try {
    names = list(dir)
      .filter((n) => LICENCE_FILE.test(n))
      .sort();
  } catch {
    return [];
  }
  return names.map((n) => read(join(dir, n), "utf8").replace(/\r\n/g, "\n").trim());
}

const DISCLAIMER_MIT = `THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.`;

// SPDX standard texts for the licences that carry their terms in a short notice.
const STANDARD = {
  MIT: (holder) => `MIT License

Copyright (c) ${holder}

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

${DISCLAIMER_MIT}`,
  ISC: (holder) => `ISC License

Copyright (c) ${holder}

Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted, provided that the above
copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.`,
};

// The text for a package that ships no licence file: the SPDX standard text of the first licence
// in its expression that has one here, naming the package's author, or else a link to each
// licence's SPDX text.
export function fallbackText(p) {
  const ids = tokens(p.licence || "").filter(
    (t) => !["AND", "OR", "WITH", "(", ")"].includes(t.toUpperCase()),
  );
  const id = ids.map((t) => t.replace(/\+$/, "")).find((t) => STANDARD[t]);
  if (id) return STANDARD[id](p.author || `the ${p.name} authors`);
  const links = ids.map((t) => `https://spdx.org/licenses/${t.replace(/\+$/, "")}.html`);
  return `${p.name} ${p.version} ships no licence file. Licence: ${p.licence || "none declared"}.${links.length ? `\nStandard text: ${links.join(" ")}` : ""}`;
}

const byName = (a, b) =>
  a.ecosystem.localeCompare(b.ecosystem) ||
  a.name.localeCompare(b.name) ||
  a.version.localeCompare(b.version, "en", { numeric: true });
const cell = (s) => String(s).replace(/\|/g, "\\|").replace(/\s+/g, " ");
// A code fence longer than any backtick run in the text, so the text is printed byte for byte.
const fence = (text) =>
  "`".repeat(Math.max(3, ...(text.match(/`+/g) ?? []).map((r) => r.length + 1)));

export function renderNotices(packages, textsOf) {
  const sorted = [...packages].sort(byName);
  const texts = new Map(); // text -> package labels
  for (const p of sorted)
    for (const t of textsOf(p)) {
      if (!texts.has(t)) texts.set(t, []);
      texts.get(t).push(`${p.name} ${p.version}`);
    }
  const lines = [
    "# Third-party notices",
    "",
    `CodeSense includes the third-party software listed below. Generated by \`node scripts/licence-scan.mjs\` from \`pnpm licenses list --prod\`, \`cargo tree --edges normal\` and \`${VENDORED}\`; do not edit.`,
    "",
  ];
  for (const [eco, title] of [
    ["npm", "npm packages"],
    ["cargo", "Rust crates"],
  ]) {
    lines.push(
      `## ${title}`,
      "",
      "| Package | Version | Licence | Source |",
      "| --- | --- | --- | --- |",
    );
    for (const p of sorted.filter((q) => q.ecosystem === eco))
      lines.push(
        `| ${cell(p.name)} | ${cell(p.version)} | ${cell(p.licence || "none declared")} | ${cell(p.url)} |`,
      );
    lines.push("");
  }
  lines.push(
    "## Bundled and vendored components",
    "",
    "| Component | Version | Licence | Shipped as | Source |",
    "| --- | --- | --- | --- | --- |",
  );
  for (const p of sorted.filter((q) => q.ecosystem === "vendored"))
    lines.push(
      `| ${cell(p.name)} | ${cell(p.version)} | ${cell(p.licence)} | ${cell(p.shippedAs)} | ${cell(p.url)} |`,
    );
  lines.push("", "## Licence texts", "");
  for (const [text, users] of texts) {
    const f = fence(text);
    lines.push(`### ${users.join(", ")}`, "", `${f}text`, text, f, "");
  }
  return lines.join("\n");
}

// Why the committed notices are stale, or null when they match: `committed` is the file at HEAD,
// null when HEAD has none.
export function staleNotices(committed, notices) {
  if (committed === null) return `${NOTICES} is not committed`;
  return committed === notices ? null : `${NOTICES} at HEAD differs from the generated notices`;
}

// deny.toml [advisories]: unsound and unmaintained must be checked for every crate ("all"; cargo
// deny's default scope for unsound is the workspace, which would leave transitive crates
// unchecked), and every ignore is a table with a reason: { id, reason } for an advisory,
// { crate, reason } for a yanked version. A bare-string ignore has no reason and is refused.
export function advisoryConfig(denyToml) {
  const raw = denyToml.match(/^\[advisories\]\s*$([\s\S]*?)(?=^\[|(?![\s\S]))/m)?.[1];
  if (raw === undefined) throw new Error(`${DENY} has no [advisories] section`);
  const section = raw
    .split("\n")
    .filter((line) => !/^\s*#/.test(line))
    .join("\n");
  for (const key of ["unsound", "unmaintained"]) {
    const scope = section.match(new RegExp(`^${key}\\s*=\\s*"([^"]*)"`, "m"))?.[1];
    if (scope !== "all")
      throw new Error(`${DENY} [advisories] needs ${key} = "all" so transitive crates are checked`);
  }
  const body = section.match(/^ignore\s*=\s*\[([\s\S]*?)\]\s*$/m)?.[1] ?? "";
  if (body.replace(/\{[^}]*\}/g, "").replace(/[\s,]/g, ""))
    throw new Error(`${DENY} [advisories] ignores need { id, reason } or { crate, reason }`);
  const ignores = [...body.matchAll(/\{([^}]*)\}/g)].map(([, entry]) => {
    const field = (name) => entry.match(new RegExp(`\\b${name}\\s*=\\s*"([^"]*)"`))?.[1]?.trim();
    const [id, crate, reason] = [field("id"), field("crate"), field("reason")];
    if (!(id || crate) || !reason)
      throw new Error(`${DENY} [advisories] ignores need { id, reason } or { crate, reason }`);
    return id ? { id, reason } : { crate, reason };
  });
  return { ignores };
}

const AUDIT_SEVERITIES = ["info", "low", "moderate", "high", "critical"];

// scripts/licence-scan.advisories.json: { "npm:<id>": { reason, by, date } }; <id> is the
// advisory's GHSA id, CVE or npm id.
export function npmIgnores(json) {
  return Object.entries(json ?? {}).map(([key, v]) => {
    const id = key.replace(/^npm:/, "");
    if (!key.startsWith("npm:") || typeof v?.reason !== "string" || !v.reason.trim())
      throw new Error(`${NPM_IGNORES} ${key}: an entry is "npm:<id>" with a reason`);
    return { id, reason: v.reason.trim() };
  });
}

const PNPM_IGNORE_HINT = `pnpm drops the advisories it names before printing its report, so the scan cannot see or re-check them; ignore each in ${NPM_IGNORES} with a reason instead`;
const IGNORE_KEYS = ["ignoreGhsas", "ignoreCves"];
const lines = (text) => text.replace(/\r\n?/g, "\n").split("\n");

// Problems with pnpm's own audit ignores: auditConfig in a root package.json (`manifests`) or the
// workspace's pnpm-workspace.yaml, or an ignore key in .npmrc (each { source, text }). Only an
// absent or empty auditConfig passes; anything else, including a form the scan cannot read for
// certain, is a problem.
export function pnpmAuditConfigProblems({ manifests = [], workspaceYaml = null, npmrcs = [] }) {
  const problems = [];
  for (const { source, text } of manifests) {
    let audit;
    try {
      audit = JSON.parse(text)?.pnpm?.auditConfig;
    } catch {
      problems.push(`${source}: cannot read it`);
      continue;
    }
    const empty =
      audit === undefined ||
      (audit !== null &&
        typeof audit === "object" &&
        !Array.isArray(audit) &&
        Object.entries(audit).every(
          ([k, v]) => IGNORE_KEYS.includes(k) && Array.isArray(v) && !v.length,
        ));
    if (!empty) problems.push(`${source} pnpm.auditConfig: ${PNPM_IGNORE_HINT}`);
  }
  if (workspaceYaml) {
    const mentions = lines(workspaceYaml.text).filter(
      (l) => !/^\s*#/.test(l) && /audit[-_]?config/i.test(l),
    );
    if (
      mentions.length &&
      !(mentions.length === 1 && /^auditConfig:\s*\{\s*\}\s*(?:#.*)?$/.test(mentions[0]))
    )
      problems.push(`${workspaceYaml.source} auditConfig: ${PNPM_IGNORE_HINT}`);
  }
  for (const { source, text } of npmrcs)
    for (const line of lines(text)) {
      if (!line.trim() || /^\s*[#;]/.test(line)) continue;
      const key = line
        .split("=")[0]
        .trim()
        .replace(/^(["'])(.*)\1$/, "$2")
        .replace(/\[\]$/, "");
      if (/audit[-_.]?config/i.test(key) || (/ignore/i.test(key) && /cve|ghsa|audit/i.test(key)))
        problems.push(`${source} ${key}: ${PNPM_IGNORE_HINT}`);
    }
  return problems;
}

const NPM_SEVERITY = { critical: "critical", high: "high", moderate: "medium", low: "low" };
// Findings from `pnpm audit --json --prod` (npm's audit report: advisories keyed by id, each with
// the installed versions and their dependency paths), one per advisory and version.
export function npmFindings(report) {
  if (!report || typeof report.advisories !== "object")
    throw new Error(
      `pnpm audit printed no advisories report: ${JSON.stringify(report?.error ?? report).slice(0, 200)}`,
    );
  const out = [];
  for (const a of Object.values(report.advisories)) {
    const patched =
      a.patched_versions && a.patched_versions !== "<0.0.0" ? a.patched_versions : null;
    for (const finding of a.findings?.length ? a.findings : [{ version: "", paths: [] }])
      out.push({
        ecosystem: "npm",
        package: a.module_name,
        version: finding.version ?? "",
        id: a.github_advisory_id ?? String(a.id),
        aliases: [...(a.cves ?? []), ...(a.github_advisory_id ? [String(a.id)] : [])],
        kind: "vulnerability",
        severity: NPM_SEVERITY[a.severity] ?? (a.severity === "info" ? "none" : "unrated"),
        cvss: a.cvss?.vectorString ?? null,
        title: a.title ?? "",
        fixedIn: patched,
        url: a.url ?? "",
        paths: finding.paths ?? [],
      });
  }
  return out;
}

// Severities where the registry's totals (metadata.vulnerabilities) exceed what the report's
// advisories show, counting each dependency path: the excess is advisories something hid. The
// registry's counting unit is not pinned, and a path count is the most it can be, so only an excess
// is a gap.
export function npmMetadataGaps(report) {
  const shown = Object.fromEntries(AUDIT_SEVERITIES.map((s) => [s, 0]));
  for (const a of Object.values(report.advisories ?? {}))
    for (const f of a.findings?.length ? a.findings : [{ paths: [] }])
      shown[a.severity] = (shown[a.severity] ?? 0) + Math.max(1, f.paths?.length ?? 0);
  const totals = report.metadata?.vulnerabilities ?? {};
  return AUDIT_SEVERITIES.filter((s) => (totals[s] ?? 0) > shown[s]).map(
    (s) => `${s}: the registry counts ${totals[s]}, the advisories show ${shown[s]}`,
  );
}

// The npm ignores that match no advisory in the report (by GHSA id, CVE or npm id): stale, since
// nothing is left for them to address.
export function staleNpmIgnores(report, ignores) {
  const ids = new Set(
    Object.values(report.advisories ?? {}).flatMap((a) => [
      String(a.id),
      ...(a.github_advisory_id ? [a.github_advisory_id] : []),
      ...(a.cves ?? []),
    ]),
  );
  return ignores.filter((i) => !ids.has(i.id));
}

const BLOCKING = new Set(["critical", "high", "unrated"]);

// Marks each finding ignored (with the reason) and blocking when it is not ignored and is a
// critical, high or unrated vulnerability, or unsound. A Rust ignore matches the RustSec id only,
// as cargo deny does (it does not honour an alias), or a crate's yanked version; an npm ignore
// matches the advisory id or an alias. Reachability is not assessed by the scan: every finding is
// in the shipped graph (dev dependencies are excluded), and whether the vulnerable code is called
// is the reviewer's call, recorded as an ignore reason.
const ignoreMatches = (f, i) =>
  f.ecosystem === "cargo"
    ? i.id
      ? i.id === f.id
      : f.kind === "yanked" && [f.package, `${f.package}@${f.version}`].includes(i.crate)
    : [f.id, ...f.aliases].includes(i.id);

export function classifyAdvisories(findings, ignores) {
  return findings.map((f) => {
    const ignored = (ignores[f.ecosystem] ?? []).find((i) => ignoreMatches(f, i))?.reason ?? null;
    const blocking =
      !ignored &&
      (f.kind === "unsound" || (f.kind === "vulnerability" && BLOCKING.has(f.severity)));
    return { ...f, reachable: "in shipped graph; call path not assessed", ignored, blocking };
  });
}

// A row for an ignore the scan could not re-check, because the tool dropped the advisory (cargo
// deny for deny.toml ignores, pnpm for its auditConfig).
export const notRechecked = (ecosystem, i, by) => ({
  ecosystem,
  package: i.crate ?? "",
  version: "",
  id: i.id ?? "yanked",
  aliases: [],
  kind: i.id ? "advisory" : "yanked",
  severity: "ignored",
  cvss: null,
  title: `ignored by ${by} (not re-checked)`,
  fixedIn: null,
  url: "",
  paths: [],
  reachable: "not assessed",
  ignored: i.reason,
  blocking: false,
});

const ADVISORY_CODES = new Set(["vulnerability", "unsound", "unmaintained", "notice", "yanked"]);

// The JSON diagnostics `cargo deny --format json` prints, one per stderr line (other lines are
// skipped).
function cargoDiagnostics(stderr) {
  const out = [];
  for (const line of stderr.split("\n")) {
    if (!line.startsWith("{")) continue;
    try {
      const d = JSON.parse(line);
      if (d?.type === "diagnostic" && d.fields) out.push(d.fields);
    } catch {
      // not a diagnostic
    }
  }
  return out;
}

// Findings from `cargo deny --format json check advisories`. Severity comes from the advisory's
// CVSS 3.x or 4.0 vector; fixed-in from the "Solution: Upgrade to ..." note.
export function cargoFindings(stderr) {
  return cargoDiagnostics(stderr)
    .filter((f) => ADVISORY_CODES.has(f.code))
    .map((f) => {
      const notes = f.notes ?? [];
      const adv = f.advisory ?? {};
      const krate = f.graphs?.[0]?.Krate ?? {};
      const solution = notes.find((n) => n.startsWith("Solution: "))?.slice(10);
      const upgrade = solution?.match(/^Upgrade to (.+?)(?: \(try .*\))?$/)?.[1];
      return {
        ecosystem: "cargo",
        package: krate.name ?? adv.package ?? "?",
        version: krate.version ?? "",
        id: adv.id ?? notes.find((n) => n.startsWith("ID: "))?.slice(4) ?? f.code,
        aliases: adv.aliases ?? [],
        kind: f.code,
        severity: f.code === "vulnerability" ? cvssRating(cvssScore(adv.cvss)) : f.code,
        cvss: adv.cvss ?? null,
        title: adv.title ?? f.message ?? "",
        fixedIn: upgrade ?? null,
        url: adv.url ?? (adv.id ? `https://rustsec.org/advisories/${adv.id}` : ""),
        paths: [],
      };
    });
}

// Why cargo deny's exit status is unexpected, or null. It exits 1 when an advisory is an error, so
// 1 is expected only when every error diagnostic is an advisory finding; any other exit, or an
// error that is not an advisory (no database, a bad config), fails the run.
export function cargoExitProblem(status, stderr) {
  if (status === 0) return null;
  const errors = cargoDiagnostics(stderr).filter((f) => f.severity === "error");
  const other = errors.filter((f) => !ADVISORY_CODES.has(f.code));
  if (status === 1 && errors.length && !other.length) return null;
  const detail = other.length
    ? other.map((f) => `${f.code ?? "error"}: ${f.message ?? ""}`).join("; ")
    : stderr.trim().slice(-400);
  return `cargo deny check advisories exited ${status}: ${detail}`;
}

function committedFile(root, rel, spawn) {
  const r = spawn("git", ["show", `HEAD:${rel}`], {
    cwd: root,
    encoding: "utf8",
    maxBuffer: 1 << 30,
  });
  return r.status === 0 ? r.stdout : null;
}

function run(cmd, args, cwd) {
  return execFileSync(cmd, args, {
    cwd,
    encoding: "utf8",
    maxBuffer: 1 << 30,
    stdio: ["ignore", "pipe", "inherit"],
  });
}

const count = (findings, pred) => findings.filter(pred).length;
const readIf = (path) => (existsSync(path) ? readFileSync(path, "utf8") : undefined);

// The nearest `name` in `dir` or a parent directory, as pnpm finds its workspace file, or null.
function findUp(dir, name) {
  for (let d = resolve(dir); ; d = dirname(d)) {
    if (existsSync(join(d, name))) return join(d, name);
    if (dirname(d) === d) return null;
  }
}

// --advisories: needs the network. Writes the findings to `out` (relative to the root) and returns
// the exit code.
function advisories(root, out, spawn) {
  const errors = [];
  const { ignores: cargoIgnores } = advisoryConfig(readFileSync(join(root, DENY), "utf8"));
  const npmIgnoreList = npmIgnores(JSON.parse(readIf(join(root, NPM_IGNORES)) ?? "{}"));
  const ignores = { cargo: cargoIgnores, npm: npmIgnoreList };
  const workspaceYamlPath = findUp(root, "pnpm-workspace.yaml");
  const dirs = [...new Set([root, workspaceYamlPath ? dirname(workspaceYamlPath) : root])];
  const at = (dir, name) => {
    const text = readIf(join(dir, name));
    return text === undefined ? [] : [{ source: join(dir, name), text }];
  };
  errors.push(
    ...pnpmAuditConfigProblems({
      manifests: dirs.flatMap((d) => at(d, "package.json")),
      workspaceYaml: workspaceYamlPath
        ? { source: workspaceYamlPath, text: readFileSync(workspaceYamlPath, "utf8") }
        : null,
      npmrcs: dirs.flatMap((d) => at(d, ".npmrc")),
    }),
  );

  const deny = spawn("cargo", ["deny", "--format", "json", "check", "advisories"], {
    cwd: root,
    encoding: "utf8",
    maxBuffer: 1 << 30,
  });
  const cargo = cargoFindings(deny.stderr ?? "");
  const exitProblem = cargoExitProblem(deny.status, deny.stderr ?? "");
  if (exitProblem) errors.push(exitProblem);
  // pnpm audit exits 1 when it finds anything, so its report is read either way.
  const audit = spawn("pnpm", ["audit", "--json", "--prod"], {
    cwd: root,
    encoding: "utf8",
    maxBuffer: 1 << 30,
  });
  let npm = [];
  try {
    const report = JSON.parse(audit.stdout ?? "");
    npm = npmFindings(report);
    const gaps = npmMetadataGaps(report);
    if (gaps.length) errors.push(`pnpm audit hides advisories (${gaps.join("; ")})`);
    for (const i of staleNpmIgnores(report, npmIgnoreList))
      errors.push(
        `${NPM_IGNORES} npm:${i.id} matches no advisory in the report: remove the stale ignore`,
      );
  } catch (e) {
    errors.push(`pnpm audit --json --prod: ${e.message}`);
  }
  const findings = classifyAdvisories([...cargo, ...npm], ignores);
  // cargo deny drops what deny.toml ignores: list each anyway.
  for (const i of cargoIgnores)
    if (!findings.some((f) => ignoreMatches(f, i) && f.ecosystem === "cargo"))
      findings.push(notRechecked("cargo", i, `${DENY} [advisories]`));
  findings.sort(
    (a, b) =>
      Number(b.blocking) - Number(a.blocking) ||
      a.ecosystem.localeCompare(b.ecosystem) ||
      a.package.localeCompare(b.package) ||
      a.id.localeCompare(b.id),
  );
  const head = spawn("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8" }).stdout?.trim();
  const summary = Object.fromEntries(
    [
      "critical",
      "high",
      "medium",
      "low",
      "unrated",
      "unsound",
      "unmaintained",
      "notice",
      "yanked",
    ].map((s) => [s, count(findings, (f) => f.severity === s)]),
  );
  summary.ignored = count(findings, (f) => f.ignored);
  summary.blocking = count(findings, (f) => f.blocking);
  const report = {
    head: head || null,
    sources: [
      "cargo deny --format json check advisories (RustSec)",
      "pnpm audit --json --prod (GitHub advisory database)",
    ],
    policy: `blocking: critical, high or unrated vulnerabilities and unsound crates, unless ignored with a reason (Rust: ${DENY} [advisories].ignore; npm: ${NPM_IGNORES}); everything else is listed for review`,
    errors,
    summary,
    findings,
  };
  const outPath = resolve(root, out);
  mkdirSync(dirname(outPath), { recursive: true });
  writeFileSync(outPath, `${JSON.stringify(report, null, 2)}\n`);
  for (const e of errors) console.error(`licence-scan: ${e}`);
  for (const f of findings.filter((x) => x.blocking))
    console.error(
      `licence-scan: blocking ${f.ecosystem} ${f.package} ${f.version}: ${f.id} (${f.severity}) ${f.title}; fixed in ${f.fixedIn ?? "no fixed version"}`,
    );
  console.log(
    `licence-scan: ${findings.length} advisory findings (${summary.blocking} blocking, ${summary.ignored} ignored with a reason); wrote ${out}`,
  );
  return errors.length || summary.blocking ? 1 : 0;
}

const USAGE = "usage: node scripts/licence-scan.mjs [--check] | --advisories [--out <file>]";

export function main(argv, { root = ROOT, run: exec = run, spawn = spawnSync } = {}) {
  if (argv.includes("--advisories")) {
    const at = argv.indexOf("--out");
    const out = at >= 0 ? argv[at + 1] : ADVISORIES_OUT;
    if (!out || out.startsWith("--")) {
      console.error(`licence-scan: --out needs a file\n${USAGE}`);
      return 2;
    }
    return advisories(root, out, spawn);
  }
  const check = argv.includes("--check");
  let failed = false;

  const deny = spawn("cargo", ["deny", "check", "licenses"], { cwd: root, stdio: "inherit" });
  if (deny.status !== 0) {
    console.error(`licence-scan: cargo deny check licenses failed (${DENY})`);
    failed = true;
  }

  const allowed = allowList(readFileSync(join(root, DENY), "utf8"));
  const meta = JSON.parse(exec("cargo", ["metadata", "--format-version", "1", "--locked"], root));
  const tree = exec(
    "cargo",
    [
      "tree",
      "--locked",
      "--workspace",
      "--edges",
      "normal",
      "--target",
      "all",
      "--prefix",
      "none",
      "--format",
      "{p}",
    ],
    root,
  );
  const { components, errors } = vendoredComponents(
    JSON.parse(readFileSync(join(root, VENDORED), "utf8")),
    (rel) => existsSync(join(root, rel)),
  );
  for (const e of errors) console.error(`licence-scan: ${e}`);
  if (errors.length) failed = true;
  const npm = npmPackages(JSON.parse(exec("pnpm", ["licenses", "list", "--json", "--prod"], root)));
  const packages = [...npm, ...cargoCrates(tree, meta), ...components];

  const noFile = [];
  const textsOf = (p) => {
    if (p.ecosystem === "vendored")
      return existsSync(join(root, p.licenceFile))
        ? [readFileSync(join(root, p.licenceFile), "utf8").replace(/\r\n/g, "\n").trim()]
        : [];
    const texts = licenceTexts(p.dir);
    if (texts.length) return texts;
    noFile.push(p);
    return [fallbackText(p)];
  };
  const notices = renderNotices(packages, textsOf);
  if (check) {
    const stale = staleNotices(committedFile(root, NOTICES, spawn), notices);
    if (stale) {
      console.error(
        `licence-scan: ${stale}; run node scripts/licence-scan.mjs and commit ${NOTICES}`,
      );
      failed = true;
    }
  } else {
    writeFileSync(join(root, NOTICES), notices);
  }

  if (noFile.length) {
    console.log(
      `licence-scan: ${noFile.length} packages ship no licence file; ${NOTICES} uses the standard text of their licence:`,
    );
    for (const p of noFile.sort(byName))
      console.log(`  ${p.ecosystem} ${p.name} ${p.version}: ${p.licence || "no licence declared"}`);
  }
  const decisionsPath = join(root, DECISIONS);
  const decisions = existsSync(decisionsPath)
    ? JSON.parse(readFileSync(decisionsPath, "utf8"))
    : {};
  const open = needsDecision([...npm, ...components], allowed, decisions).sort(byName);
  if (open.length) {
    console.error(
      `licence-scan: ${open.length} packages need an owner decision (record it in ${DECISIONS}):`,
    );
    for (const p of open)
      console.error(
        `  ${p.ecosystem} ${p.name} ${p.version}: ${p.licence || "no licence declared"}`,
      );
    failed = true;
  }
  const counts = ["npm", "cargo", "vendored"].map(
    (e) => `${packages.filter((p) => p.ecosystem === e).length} ${e}`,
  );
  console.log(
    `licence-scan: ${counts.join(", ")}; ${open.length} need a decision; ${noFile.length} without a licence file`,
  );
  return failed ? 1 : 0;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  process.exitCode = main(process.argv.slice(2));
}
