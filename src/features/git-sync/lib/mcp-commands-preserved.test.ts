// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

/**
 * The local viewer invariant: NO git-sync component
 * may call retired repository mutation wrappers.
 * A structural guard (grep the component sources) so a future mutating call landing
 * in a component fails here rather than silently re-introducing a phantom verb.
 */
const componentsDir = join(dirname(fileURLToPath(import.meta.url)), "..", "components");
const MUTATING_WRAPPERS = [
  "publishNode",
  "pullFf",
  "runPull",
  "applyPull",
  "applyPullAuto",
  "approveAndMergePr",
  "sendOutboxSlice",
  "enqueueOutbox",
];

describe("no git-sync component calls a mutating wrapper (read-only surface)", () => {
  const componentFiles = readdirSync(componentsDir).filter(
    (f) => f.endsWith(".tsx") && !f.endsWith(".test.tsx"),
  );

  for (const file of componentFiles) {
    it(`${file} references no mutating command wrapper`, () => {
      const src = readFileSync(join(componentsDir, file), "utf8");
      for (const wrapper of MUTATING_WRAPPERS) {
        expect(src, `${file} must not reference ${wrapper}`).not.toMatch(
          new RegExp(`\\b${wrapper}\\b`),
        );
      }
    });
  }
});
