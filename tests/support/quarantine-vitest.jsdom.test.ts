// SPDX-License-Identifier: MIT
// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, it } from "vitest";
import "./quarantine-vitest";
import { LEDGERS, loadEntries } from "../../scripts/lib/quarantine.mjs";

it("imports the quarantine adapter and loads the real ledgers in jsdom", () => {
  expect(document.createElement("div").ownerDocument).toBe(document);
  // the jsdom module URL resolves the same root, and the loader returns at least the main ledger
  const entries = loadEntries();
  expect(entries).toEqual(loadEntries(process.cwd()));
  const ledger = JSON.parse(readFileSync(resolve(process.cwd(), LEDGERS[0]), "utf8"));
  expect(entries.slice(0, ledger.length)).toEqual(ledger);
});
