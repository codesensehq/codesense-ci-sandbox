// SPDX-License-Identifier: MIT
import type {
  EnsureEpicWorkspaceParams,
  EpicWorkspaceHandle,
  AssembleEpicResult,
} from "@/features/viewer/lib/cli-workspace/transport/terminal-launch-source";
import { getErrorMessage } from "@/shared/lib/error-message";
import { formatEpicFailureSummary } from "@/features/shell/stores/epic-assembly-store";

/**
 * SW-EW-2 — "Open epic terminal" orchestration, extracted from the React
 * surface so every branch (already-open, in-flight, empty-dir guard, assembly
 * partial-failure / throw) is pure and unit-testable. Mirrors the injected-deps
 * pattern of `create-canvas.ts`: the view resolves org/team context + the store
 * ops and hands them in; this owns only the flow + guards.
 *
 * SLW-CO-6 (AC-5/AC-6) — the epic path is unified onto the same
 * background-materialize model as the story worktree (SLW-CO-5), retiring the
 * bespoke setup toast entirely: the epic terminal opens READY immediately (its
 * dir is usable at once), and the `EpicInheritance` wave (install + context +
 * dev-env) runs in the background (no wave reaches a chip since SW-XS-10, which
 * keeps the chip for explicit operations). The chip does NOT carry
 * the separate per-repo worktree assembly outcome (a distinct racing source), so
 * only an assembly FAILURE is surfaced explicitly — onto the same chip via
 * `onAssemblyFailed`. Pre-open hard failures (dir unavailable / ensure threw)
 * surface via `onOpenError` (a plain error toast). No progress toast.
 */

export interface OpenEpicTerminalDeps {
  ensureEpicWorkspace: (params: EnsureEpicWorkspaceParams) => Promise<EpicWorkspaceHandle>;
  assembleEpicWorktrees: (params: EnsureEpicWorkspaceParams) => Promise<AssembleEpicResult>;
  /** Shared re-entrancy guard (keyed by `base.epicFilePath`) so a concurrent
   *  re-fire can't race a second assembly against the same epic dir. */
  inFlight: Set<string>;
  /** True when an epic-level integration workspace already exists for this epic. */
  isAlreadyOpen: () => boolean;
  /** Focus the already-open epic integration workspace (no re-sync). */
  focusExisting: () => void;
  /** SLW-FX-27 (Part B, AC-6/AC-7): arm the epic's setup affordance SYNCHRONOUSLY
   *  at open — BEFORE the (~1-2s) `ensureEpicWorkspace` materialize — by creating
   *  a born-"creating" placeholder epic row (empty cwd) with the `needsSyncOverlay`
   *  latch, so the inline `WorkspaceInitOverlay` shows immediately, exactly like a
   *  story workspace (which arms `status:"creating"` before its own heavy work).
   *  The store is designed for this two-phase open: an empty-cwd epic row stays
   *  "creating" until `openIntegration` supplies the resolved dir (viewer-store.ts
   *  `openOrFocusEpicWorkspace` :5721-5722). Idempotent (focus on re-arm). */
  armEpicOpen: () => void;
  /** SLW-FX-27 (Part B): tear down the early-armed placeholder on a hard pre-open
   *  failure (ensure threw / dir unavailable) so a failed open never strands a
   *  stuck "creating" epic tile. A no-op if nothing was armed. */
  cancelEpicOpen: () => void;
  /** SLW-FX-27 (Part B): true iff the placeholder row armed by `armEpicOpen` is
   *  still present. Checked AFTER the materialize await — false means the user
   *  closed the "creating" tile mid-flight, so the resolved open must NOT resurrect
   *  it. (The focus-guard `isAlreadyOpen` can't serve double-duty: it must read
   *  false BEFORE arm and true AFTER, so it's a distinct predicate.) */
  isPlaceholderPresent: () => boolean;
  /** Create + focus a new epic-level integration terminal rooted at `dirPath`.
   *  SW-EW-15: `epicDocPath` is threaded onto the workspace record so repo
   *  read/write resolves from the record, not the (empty) nav tree. */
  openIntegration: (dirPath: string, epicDocPath: string) => void;
  /** SLW-CO-6 (AC-6): per-repo assembly failed — surface it on the epic's
   *  SW-TS-3 chip (keyed by `workspaceDir`), since the assembly outcome is NOT
   *  part of the `EpicInheritance` wave the chip already reflects. */
  onAssemblyFailed: (workspaceDir: string, message: string) => void;
  /** SLW-CO-6 (AC-6): assembly succeeded — clear any prior assembly failure for
   *  this surface (assembly owns its chip slot; nothing else clears it, so a
   *  re-open that now succeeds must clear a stale failure explicitly). */
  onAssemblyResolved: (workspaceDir: string) => void;
  /** SLW-CO-6 (AC-6): a pre-open hard failure (ensure threw / dir unavailable) —
   *  the epic never opened, so surface a plain error toast. */
  onOpenError: (message: string) => void;
  /** SLW-CO-8 (LD-E): begin (reset) the epic-assembly batch for `workspaceDir`
   *  the moment the terminal opens, so the overlay shows before the result. */
  beginEpicAssembly: (workspaceDir: string) => void;
  /** SLW-CO-8 (LD-A): seed the batch with the returned dev-env run_ids + create
   *  failures once `assembleEpicWorktrees` resolves — the overlay awaits these
   *  installs' terminal events (M = `devEnvRuns.length`). */
  seedEpicAssembly: (workspaceDir: string, result: AssembleEpicResult) => void;
  /** SLW-CO-8 (LD-E): the assemble command threw entirely — a TOTAL failure
   *  (the overlay hands off to the in-pane error, not a partial flag). */
  markEpicAssemblyError: (workspaceDir: string, message: string) => void;
}

/**
 * Open (or focus) an epic's integration terminal.
 *
 * - Already open → just focus it; skip `ensureEpicWorkspace`/`assembleEpicWorktrees`
 *   so re-clicking an open epic doesn't redo git work.
 * - Concurrent re-fire (same epic still in flight) → no-op.
 * - Otherwise: ensure the epic dir, open ONE terminal there (usable immediately
 *   — non-blocking, born ready), then best-effort assemble the configured repos'
 *   worktrees (a draft epic with no `repos:` is a clean no-op). Materialization
 *   readiness rides the SW-TS-3 chip (`EpicInheritance` wave); an assembly
 *   FAILURE is surfaced on that chip. The in-flight flag is held across the
 *   awaited assembly and released in `finally` on every exit.
 */
export async function openEpicTerminal(
  base: EnsureEpicWorkspaceParams,
  deps: OpenEpicTerminalDeps,
): Promise<void> {
  // Already open → focus only. Avoids redundant assembly on an already-open epic.
  if (deps.isAlreadyOpen()) {
    deps.focusExisting();
    return;
  }
  // SW-EW-15: the epic DOC PATH is the unique re-entrancy key — the FE never
  // needs the backend's derived dir slug for this.
  const id = base.epicFilePath;
  if (deps.inFlight.has(id)) return;
  deps.inFlight.add(id);
  // SLW-FX-27 (Part B, AC-6/AC-7): arm the setup overlay NOW — before the awaited
  // materialize below — so the epic shows its "setting up…" affordance at the same
  // instant a story workspace does. The placeholder reconciles to the ready row in
  // `openIntegration` once the dir resolves; the error paths below tear it down.
  deps.armEpicOpen();
  try {
    // 1. Ensure + sync the epic workspace dir (exists even with no repos).
    const handle = await deps.ensureEpicWorkspace(base);
    // SLW-FX-27 (Part B): if the user closed the "creating" placeholder while the
    // materialize was in flight, the row is gone — DON'T resurrect it as a ready
    // terminal the user already dismissed. Mirrors the story open's post-await
    // abort check; guards the exact "phantom tabs to a workspace that no longer
    // exists" race `closeWorkspace` warns about (viewer-store.ts:5893-5902). No
    // cancel needed — the row is already gone.
    if (!deps.isPlaceholderPresent()) return;
    if (!handle.dirPath) {
      // Defensive: the backend contracts a non-empty absolute dir, but an empty
      // path would strand the epic-level row in "creating" forever (no per-row
      // orchestrator flips it ready). Tear the placeholder down + surface.
      deps.cancelEpicOpen();
      deps.onOpenError("Couldn't open epic terminal: workspace dir unavailable.");
      return;
    }
    // 2. Open ONE epic-level terminal in that dir — the planning vantage point
    //    that holds each configured repo's worktree. Born READY, usable at once;
    //    SLW-CO-8: the `WorkspaceInitOverlay` covers it (the record set
    //    `needsSyncOverlay`) and rides the whole batch below.
    deps.openIntegration(handle.dirPath, base.epicFilePath);
    // SLW-CO-8 (LD-E): begin the batch NOW (before the result) so the overlay is
    // up immediately — the aggregate stays "assembling" until seeded + settled.
    deps.beginEpicAssembly(handle.dirPath);
    // 3. Assemble the configured repos' worktrees + fan out their dev-env
    //    installs. The terminal is already usable; this is awaited only to hold
    //    the in-flight guard, surface create failures on the chip, and SEED the
    //    batch (install completions then arrive async via `worktree-run-completed`
    //    and drive the overlay's aggregate readiness). A draft epic (no `repos:`)
    //    is a clean no-op (empty `devEnvRuns`).
    try {
      const result = await deps.assembleEpicWorktrees(base);
      // Seed the aggregate: the run_id set (M) the overlay awaits + create
      // failures. Install completions fold in via the terminal-event listener.
      deps.seedEpicAssembly(handle.dirPath, result);
      if (result.failures.length > 0) {
        // Eager create-failure echo (the listener supersedes it with the combined
        // create+install set once installs settle) — one shared formatter/verb so
        // the chip never flip-flops wording.
        deps.onAssemblyFailed(handle.dirPath, formatEpicFailureSummary(result.failures));
      } else {
        // Clean assembly clears any stale failure from a prior open (assembly
        // owns its chip slot — a racing inheritance wave never clears it). The
        // install-failure echo is applied later by the batch listener on settle.
        deps.onAssemblyResolved(handle.dirPath);
      }
    } catch (err) {
      // A total assembly failure — no result, no worktrees. The overlay hands
      // off to the in-pane error (close + reopen), not a partial flag.
      const message = getErrorMessage(err);
      deps.markEpicAssemblyError(handle.dirPath, message);
      deps.onAssemblyFailed(handle.dirPath, `Repos couldn't set up: ${message}`);
    }
  } catch (err) {
    // Ensure threw (dir never resolved) — tear down the early-armed placeholder so
    // the failed open doesn't leave a stuck "creating" epic tile (SLW-FX-27 Part B).
    deps.cancelEpicOpen();
    deps.onOpenError(`Couldn't open epic terminal: ${getErrorMessage(err)}`);
  } finally {
    deps.inFlight.delete(base.epicFilePath);
  }
}
