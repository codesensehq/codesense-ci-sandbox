// SPDX-License-Identifier: MIT
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

/**
 * SW-MC-6 invariants as executable guards. These pin the story's load-bearing
 * seams so a later change can't silently regress them.
 */

// Vitest runs from the package root; resolve `src/` from there.
const SRC = join(process.cwd(), "src");

/** Recursively collect `.ts`/`.tsx` files under `dir`, skipping test files. */
function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) {
      out.push(...sourceFiles(full));
    } else if (/\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name)) {
      out.push(full);
    }
  }
  return out;
}

describe("SW-MC-6 invariants", () => {
  it("AC-3: the source bar is rendered from exactly one mount point (no fork)", () => {
    const mounts = sourceFiles(SRC).filter((f) =>
      readFileSync(f, "utf8").includes("<SourceRepoList"),
    );
    // Settings workspace-section team-tile — the sole mount (the Home-Base
    // repos-panel was retired with the hub in SW-OC-7).
    expect(mounts.map((f) => f.slice(SRC.length + 1)).sort()).toEqual([
      "features/settings/components/workspace-section/team-tile.tsx",
    ]);
  });

  it("AC-11: the pill adds no new per-row Tauri command beyond the existing checkPathExists probe", () => {
    const src = readFileSync(
      join(SRC, "features/attachment/components/source-repo-pill.tsx"),
      "utf8",
    );
    // The single command import block: only the pre-existing attach/detach/probe
    // set. If a new per-row command creeps in, this guard fails.
    const importedCommands = src.match(/from "@\/tauri\/commands"/g) ?? [];
    expect(importedCommands.length).toBe(1);
    expect(src).toContain("checkPathExists");
    // No direct `invoke(` calls in the component (all IPC goes through the typed
    // command wrappers, and none is added in a render path by MC-6).
    expect(src).not.toMatch(/\binvoke\(/);
  });
});
