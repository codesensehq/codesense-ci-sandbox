// SPDX-License-Identifier: MIT
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

/**
 * SW-AU-85B AC-5 (UX §3.2/§14): the in-document `Document / Automation activity`
 * toggle the UX spec retired was never built — automation awareness now attaches
 * to the subject via the shared host in the docked toolbar. This is the
 * non-regression guard: no viewer surface may (re)introduce a viewer-specific
 * in-document automation toggle / panel. If one is ever added, this fails loudly.
 */

const ROOTS = ["src/features/viewer", "src/features/dashboard/components"];

// Identifiers a resurrected in-document automation toggle would carry.
const FORBIDDEN = [
  "Document / Automation activity",
  "ContextActivityIndicator",
  "ContextAutomationActivity",
];

function collectSourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) {
      out.push(...collectSourceFiles(full));
      continue;
    }
    if (!/\.tsx?$/.test(entry.name)) continue;
    // Skip only THIS guard (it names the forbidden strings); every other file —
    // production and test — is scanned, so a reintroduction in a test helper or a
    // co-located test-only component is still caught.
    if (entry.name === "no-retired-automation-toggle.test.ts") continue;
    out.push(full);
  }
  return out;
}

describe("SW-AU-85B AC-5: no retired in-document automation toggle", () => {
  it("no viewer surface reintroduces the retired Document/Automation activity toggle", () => {
    const offenders: string[] = [];
    for (const root of ROOTS) {
      for (const file of collectSourceFiles(root)) {
        const text = readFileSync(file, "utf8");
        for (const needle of FORBIDDEN) {
          if (text.includes(needle)) offenders.push(`${file} → "${needle}"`);
        }
      }
    }
    expect(offenders).toEqual([]);
  });
});
