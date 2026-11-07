// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

/**
 * AC-2 (no-git invariant — testable). Every `git-sync` source file must
 * depend on the typed contract + the `publish-set-source.ts` seam, NEVER on
 * `invoke` / `@/tauri/commands` directly. The seam is the single swap-point
 * SW-GS-2 repoints at the real wrapper; keeping it the only door preserves
 * the no-new-git/spawn-wrapper guarantee.
 *
 * Chat passthrough (AC-18) goes through `@/shared/lib/send-to-terminal`'s
 * `sendTextToSession`, and the supervised-session terminal lives in
 * `@/shared/components/...` — so no git-sync file needs `commands.ts`.
 */

// Raw source of every TS/TSX file in the git-sync feature (this dir's parent).
const sources = import.meta.glob("../**/*.{ts,tsx}", {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;

describe("git-sync no-direct-invoke guard (AC-2)", () => {
  it("no git-sync file imports from @/tauri/commands", () => {
    const offenders = Object.entries(sources)
      .filter(([path]) => !path.includes(".test."))
      // The data-source seams are the ONLY sanctioned swap-points: the publish
      // set (SW-GS-2), the per-file redline content (SW-GS-3), the out-of-sync
      // behind-count probe (SW-GS-6), the fast-forward pull (SW-GS-5), and the
      // node-level publish command (SW-GS-8).
      .filter(([path]) => !path.endsWith("publish-set-source.ts"))
      .filter(([path]) => !path.endsWith("file-redline-source.ts"))
      .filter(([path]) => !path.endsWith("incoming-status-source.ts"))
      .filter(([path]) => !path.endsWith("pull-source.ts"))
      .filter(([path]) => !path.endsWith("publish-node-source.ts"))
      // SW-GS-19: the PR-review seam (list open PRs + approve&merge).
      .filter(([path]) => !path.endsWith("pr-review-source.ts"))
      // SW-GS-10 — the conflict resolved-vs-ours redline blob reader
      // (`/viewer-baseline?tree=`, port via getHttpApiPort).
      .filter(([path]) => !path.endsWith("conflict-redline-source.ts"))
      // SLW-WD-1 — the source worktree diff seam (source_worktree_changeset +
      // source_file_redline), consumed by the WD-2 review overlay.
      .filter(([path]) => !path.endsWith("source-redline-source.ts"))
      // The context repo's status, card actions and history.
      .filter(([path]) => !path.endsWith("repo-status-source.ts"))
      // Types are the contract, not a door: only a value import reaches a command.
      .filter(([, src]) => /^import\s+(?!type\b)[^;]*from\s+["']@\/tauri\/commands["']/m.test(src))
      .map(([path]) => path);

    expect(offenders).toEqual([]);
  });

  it("no git-sync file calls invoke( directly", () => {
    const offenders = Object.entries(sources)
      .filter(([path]) => !path.includes(".test."))
      .filter(([, src]) => /\binvoke\s*</.test(src) || /\binvoke\s*\(/.test(src))
      .map(([path]) => path);

    expect(offenders).toEqual([]);
  });
});
