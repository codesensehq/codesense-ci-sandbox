// SPDX-License-Identifier: MIT
/**
 * The terminal architecture guard (scripts/terminal-arch-guard.ts): the tree
 * is clean, each clause fires on its violating fixture, and the enforcing
 * script exits non-zero when pointed at a violation.
 */
import { spawnSync } from "node:child_process";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { scan, type Clause } from "../../../scripts/terminal-arch-guard";

const repo = resolve(__dirname, "../../..");
const fixture = (clause: Clause) => resolve(__dirname, "__guard_fixtures__", clause);

describe("terminal architecture guard", () => {
  it("the production tree is clean", () => {
    expect(scan(repo)).toEqual([]);
  });

  it.each<[Clause, string]>([
    ["a", "src/features/x/resume.ts"],
    ["b", "src/features/x/make-terminal.ts"],
    ["c", "src/shared/hooks/use-x.ts"],
    ["d", "src/features/x/write.ts"],
    ["e", "package.json"],
  ])("clause (%s) fires on its fixture %s", (clause, file) => {
    const found = scan(fixture(clause));
    expect(found.some((v) => v.clause === clause && v.file === file)).toBe(true);
  });

  it("a type-only xterm import outside the terminal module passes", () => {
    expect(scan(fixture("d")).filter((v) => v.clause === "c")).toEqual([]);
  });

  it("the enforcing script exits non-zero on a violating tree", () => {
    const result = spawnSync(
      process.execPath,
      ["scripts/check-terminal-arch.mjs", "--root", fixture("c"), "--skip-rust"],
      { cwd: repo, encoding: "utf8" },
    );
    expect(result.status).not.toBe(0);
    expect(result.stderr).toContain("terminal-arch (c) src/shared/hooks/use-x.ts");
  }, 30_000);
});
