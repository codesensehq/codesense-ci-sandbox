// SPDX-License-Identifier: MIT
/**
 * SW-XS-17 (AC-1/AC-5): the frontend holds no worktree cap, idle-threshold, pin
 * or reap wiring. The command wrappers and events are gone, and outside tests the
 * only place the retired setting names may appear is the schema migration that
 * drops them.
 */
import { describe, expect, it } from "vitest";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";

import * as commands from "@/tauri/commands";
import * as events from "@/tauri/events";

const SRC = join(__dirname, "..", "..");

function sourceFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return sourceFiles(path);
    return /\.(ts|tsx)$/.test(name) && !/\.test\.(ts|tsx)$/.test(name) ? [path] : [];
  });
}

describe("no worktree cap or idle reaper (SW-XS-17)", () => {
  it("exposes no cap, admission, idle-threshold, pin or reap-now command", () => {
    for (const name of [
      "setWorktreeCap",
      "ensureWorktreeCapacity",
      "setWorktreeIdleThreshold",
      "setWorktreePins",
      "reapWorktreesNow",
    ]) {
      expect(commands).not.toHaveProperty(name);
    }
  });

  it("listens for no reap events", () => {
    expect(events).not.toHaveProperty("WORKTREE_REAPED_EVENT");
    expect(events).not.toHaveProperty("WORKTREE_REAPING_EVENT");
  });

  it("keeps the retired names only in the schema migration", () => {
    const tokens = [
      "maxWorktrees",
      "idleThresholdMinutes",
      "worktreePins",
      "worktree_cap",
      "worktreeCapacityHandoffMessage",
      "worktree-reaping-store",
      "use-worktree-reap-notification",
      "use-worktree-cap-setting",
      "use-worktree-idle-threshold-setting",
    ];
    const allowed = new Set(["shared/stores/viewer-schema-migrations.ts"]);
    const hits: string[] = [];
    for (const file of sourceFiles(SRC)) {
      const rel = relative(SRC, file);
      if (allowed.has(rel)) continue;
      const text = readFileSync(file, "utf8");
      for (const token of tokens) if (text.includes(token)) hits.push(`${rel}: ${token}`);
    }
    expect(hits).toEqual([]);
  });
});
