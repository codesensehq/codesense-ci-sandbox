// SPDX-License-Identifier: MIT
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";

import { ESLint } from "eslint";
import { describe, expect, it } from "vitest";

import FIRST_USE_SURFACES from "./first-use-surfaces.json";

/**
 * SW-FU-C1 non-regression guard. The first-use surfaces are built from ONE button,
 * empty state, spinner and inline alert; `eslint.config.js` fails a new ad-hoc
 * variant on them. This proves the rule is wired (a planted variant is flagged),
 * the listed surfaces are clean, and the merged-away empty-state variants stay gone.
 */

/** The repo root, so the guard reads the same files whatever the working directory. */
const ROOT = resolve(__dirname, "../../..");

const RETIRED = [
  "EmptyStateShell",
  "CoachedEmptyState",
  "empty-state-shell",
  "coached-empty-state",
];

function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...sourceFiles(full));
    else if (/\.tsx?$/.test(entry.name) && entry.name !== "first-use-consistency.guard.test.ts")
      out.push(full);
  }
  return out;
}

describe("SW-FU-C1: first-use surfaces use the shared pieces", () => {
  it("every listed surface exists", () => {
    for (const file of FIRST_USE_SURFACES) expect(existsSync(join(ROOT, file)), file).toBe(true);
  });

  it("the merged empty-state variants are gone and imported nowhere", () => {
    const offenders = sourceFiles(join(ROOT, "src")).filter((file) => {
      const text = readFileSync(file, "utf8");
      return RETIRED.some((name) => text.includes(name));
    });
    expect(offenders).toEqual([]);
  });

  it("lint flags a new ad-hoc variant on a first-use surface", async () => {
    const eslint = new ESLint({ cwd: ROOT });
    const planted = [
      'import { Loader2 } from "lucide-react";',
      "export function Bad({ e, err }: { e: unknown; err: Error }) {",
      "  return (",
      "    <div>",
      '      <button className="rounded-md bg-primary px-3 text-primary-foreground">Go</button>',
      "      <button className={`w-full bg-foreground text-background`}>Go</button>",
      '      <span className="animate-spin" />',
      "      <span className={`size-4 animate-spin`} />",
      "      <p>{String(e)}</p>",
      "      <p>{(e as Error).message}</p>",
      "      <p>{err.message}</p>",
      "      <p>Loading...</p>",
      '      <Busy label="Saving..." />',
      "      <p key={String(3)}>fine</p>",
      "    </div>",
      "  );",
      "}",
    ].join("\n");
    const [result] = await eslint.lintText(planted, {
      filePath: "src/features/org/components/org-picker.tsx",
    });
    const rules = result.messages.map((m) => m.ruleId);
    // Two raw fills, two raw spins, String(e), err.message, two sets of three dots.
    // String(3) is not an error and passes; `(e as Error).message` is caught by
    // review, not lint.
    expect(rules.filter((r) => r === "no-restricted-syntax")).toHaveLength(8);
    expect(rules).toContain("no-restricted-imports");
  }, 60_000);

  it("the listed surfaces pass the guard today", async () => {
    const eslint = new ESLint({ cwd: ROOT });
    const results = await eslint.lintFiles(FIRST_USE_SURFACES.map((file) => join(ROOT, file)));
    const guardHits = results.flatMap((r) =>
      r.messages
        .filter((m) => m.ruleId === "no-restricted-syntax" || m.ruleId === "no-restricted-imports")
        .map((m) => `${r.filePath}:${m.line} ${m.message}`),
    );
    expect(guardHits).toEqual([]);
  }, 120_000);

  it("SW-FU-B8/C5: lint flags a raw toast error and retired words on a first-use surface", async () => {
    const eslint = new ESLint({ cwd: ROOT });
    const planted = [
      'import { toast } from "sonner";',
      'import { getErrorMessage } from "@/shared/lib/error-message";',
      "export function Bad({ e, busy }: { e: Error; busy: boolean }) {",
      "  toast.error(`Failed: ${e.message}`);",
      "  toast.error(`Failed: ${String(e)}`);",
      "  toast.error(getErrorMessage(e));",
      '  const action = { label: "Open workspaces" };',
      "  return (",
      "    <div>",
      "      <h1>Welcome to Codesense</h1>",
      '      <Card title="Create a workspace" detail="A two-repo sample epic." />',
      "      <p>Join your team</p>",
      '      <Card title="Set up a team" data-testid="workspace-identity" />',
      "      {action.label}",
      "      <Card title={`Switch workspace (${action.label})`} />",
      '      <Card title={busy ? "Wait" : "Open workspaces"} />',
      "    </div>",
      "  );",
      "}",
      "export function Bad2({ e, busy }: { e: Error; busy: boolean }) {",
      "  toast(`Failed: ${e.message}`);",
      '  return { label: busy ? "…" : "Create a workspace" };',
      "}",
    ].join("\n");
    const [result] = await eslint.lintText(planted, {
      filePath: "src/features/org/components/org-picker.tsx",
    });
    const flagged = result.messages.filter((m) => m.ruleId === "no-restricted-syntax");
    // Three raw toasts; Codesense (text), workspace (title), sample epic (detail),
    // workspaces (label); a template title, a ternary title, a bare toast and a
    // ternary label. A test id and the new words pass.
    expect(flagged.map((m) => m.line).sort((a, b) => a - b)).toEqual([
      4, 5, 6, 7, 10, 11, 11, 15, 16, 21, 22,
    ]);
  }, 60_000);
});
