// SPDX-License-Identifier: MIT
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { dirname, isAbsolute, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const issueRepository = () => process.env.QUARANTINE_ISSUE_REPOSITORY || "codesensehq/codesense";

// The ledger, then an optional second ledger for tests this tree does not carry. When the second
// file exists its entries are merged and pass the same validation, inventory and issue checks.
export const LEDGERS = ["tests/quarantine.json", "tests/quarantine.local.json"];

// Vitest DOM environments can expose a transformed, non-file module URL.
const repoRoot = () =>
  import.meta.url.startsWith("file:")
    ? resolve(dirname(fileURLToPath(import.meta.url)), "../..")
    : process.cwd();

export function loadEntries(root = repoRoot()) {
  const present = (file) => existsSync(resolve(root, file));
  return LEDGERS.filter((file, index) => index === 0 || present(file)).flatMap((file) => {
    const entries = JSON.parse(readFileSync(resolve(root, file), "utf8"));
    assert.ok(Array.isArray(entries), `${file}: quarantine must be an array`);
    return entries;
  });
}

export function quarantineId(runner, file, titles) {
  const path = (isAbsolute(file) ? relative(process.cwd(), file) : file).replaceAll("\\", "/");
  return `${runner}:${path}#${titles.join(" > ")}`;
}

function idCounts(entries) {
  const counts = new Map();
  for (const entry of entries) counts.set(entry?.id, (counts.get(entry?.id) ?? 0) + 1);
  return counts;
}

export function validate(entries, now = Date.now(), repository = issueRepository()) {
  assert.ok(Array.isArray(entries), "quarantine must be an array");
  const counts = idCounts(entries);
  for (const entry of entries) {
    assert.match(entry.id, /^(vitest|playwright|cargo):.+/);
    assert.equal(counts.get(entry.id), 1, `duplicate quarantine: ${entry.id}`);
    assert.ok(
      typeof entry.reason === "string" && entry.reason.trim(),
      `${entry.id}: missing reason`,
    );
    assert.match(
      entry.issue,
      /^https:\/\/github\.com\/[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+\/issues\/[1-9]\d*$/,
      `${entry.id}: expected a GitHub issue URL`,
    );
    assert.ok(
      entry.issue.startsWith(`https://github.com/${repository}/issues/`),
      `${entry.id}: issue must belong to ${repository}`,
    );
    assert.ok(
      !/^playwright:tests\/frame-security\/hostile\.spec\.ts#/.test(entry.id),
      "hostile frame security tests cannot be quarantined",
    );
    assert.match(entry.added, /^\d{4}-\d{2}-\d{2}$/);
    const added = Date.parse(`${entry.added}T00:00:00Z`);
    assert.ok(
      Number.isFinite(added) && new Date(added).toISOString().slice(0, 10) === entry.added,
      `${entry.id}: invalid added date`,
    );
    assert.ok(added <= now, `${entry.id}: future added date`);
    assert.ok(now - added <= 30 * 86400000, `${entry.id}: quarantine is older than 30 days`);
  }
}

/** Invalid entries never authorize a runner skip, even when a separate gate was omitted. */
export function validEntries(entries, now = Date.now(), repository = issueRepository()) {
  if (!Array.isArray(entries)) return [];
  const counts = idCounts(entries);
  return entries.filter((entry) => {
    try {
      validate([entry], now, repository);
      return counts.get(entry.id) === 1;
    } catch {
      return false;
    }
  });
}

export function quarantineReason(entry) {
  return `quarantine: ${entry.reason} (${entry.issue}; added ${entry.added})`;
}
