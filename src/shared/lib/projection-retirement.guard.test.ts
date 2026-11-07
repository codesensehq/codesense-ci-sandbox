// SPDX-License-Identifier: MIT
/**
 * SW-XS-14: the team projection model's staleness and view overrides are
 * retired end to end. These source-level guards fail if any of it returns.
 */
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { relative, resolve } from "node:path";
import { describe, expect, it } from "vitest";

const src = resolve(__dirname, "../..");
const read = (rel: string) => readFileSync(resolve(src, rel), "utf8");

/** Every .ts/.tsx file under src/, as src-relative paths. */
function sourceFiles(dir: string = src): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const full = resolve(dir, entry.name);
    if (entry.isDirectory()) return sourceFiles(full);
    return /\.tsx?$/.test(entry.name) ? [relative(src, full)] : [];
  });
}

describe("SW-XS-14 projection retirement guard", () => {
  it("has no staleness-overrides hook", () => {
    expect(existsSync(resolve(src, "shared/hooks/use-staleness-overrides.ts"))).toBe(false);
  });

  it("the nav menu has no Include/Remove-from-view items", () => {
    const menu = read("features/dashboard/components/status-nav/nav-context-menu.tsx");
    expect(menu).not.toMatch(/Include in view/);
    expect(menu).not.toMatch(/Remove from view/);
  });

  it("no view-override command is invoked", () => {
    const commands = read("tauri/commands.ts");
    for (const name of ["add_view_override", "remove_view_override", "load_view_overrides"]) {
      expect(commands).not.toContain(`"${name}"`);
    }
  });

  it("no per-repo staleness threshold remains", () => {
    expect(read("shared/stores/config-store.ts")).not.toMatch(/viewStalenessMonths/);
    expect(read("features/settings/components/general-section.tsx")).not.toMatch(/ViewStaleness/);
  });

  it("SW-XS-15: the install-tier composite overlay is gone", () => {
    const commands = read("tauri/commands.ts");
    expect(commands).not.toContain('"resolve_composite_for_repo"');
    expect(commands).not.toMatch(/resolveCompositeForRepo/);
    expect(existsSync(resolve(src, "shared/lib/pipeline-graph/use-installed-nav-tree.ts"))).toBe(
      false,
    );
    expect(read("shared/lib/pipeline-graph/composite-to-repo-scan.ts")).not.toMatch(
      /export function compositeToRepoScan/,
    );
    expect(read("shared/lib/pipeline-graph/index.ts")).not.toMatch(/compositeToRepoScan/);
  });

  it("SW-XS-18: the dead override-glob prefix is gone from src/", () => {
    // Built by concatenation so this guard does not match itself.
    const symbols = [
      "override" + "GlobPrefix",
      "override_glob" + "_prefix",
      "DEFAULT_OVERRIDE" + "_GLOB_PREFIX",
    ];
    const offenders = sourceFiles().filter((rel) => {
      const text = read(rel);
      return symbols.some((symbol) => text.includes(symbol));
    });
    expect(offenders).toEqual([]);
  });

  it("SW-XS-19: the dead composite provenance chain is gone from src/", () => {
    // Built by concatenation so this guard does not match itself.
    const patterns = [
      new RegExp("Composite" + "Provenance"),
      // The retired `composite` field (DocRef.composite and its nav copies),
      // on code lines only: comment prose such as "as the composite:" in
      // SW-VE-14's use-session-cwd-revision.ts has nothing to do with it.
      new RegExp("^(?!\\s*(?:\\*|//)).*\\bcomposite" + "\\??:", "m"),
      new RegExp("shadowsTeam" + "Workspace"),
      new RegExp("shadowedTeam" + "Workspace"),
      new RegExp("[\"'`]Team" + "(Repo|Workspace)[\"'`]"),
    ];
    const offenders = sourceFiles().filter((rel) => {
      const text = read(rel);
      return patterns.some((pattern) => pattern.test(text));
    });
    expect(offenders).toEqual([]);
  });
});
