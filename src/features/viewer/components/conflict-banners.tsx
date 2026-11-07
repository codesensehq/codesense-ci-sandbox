// SPDX-License-Identifier: MIT
// ---------------------------------------------------------------------------
// SW-VE-4 — the shared concurrent-write conflict UI, rendered identically by the
// source-worktree editor (`source-editor-inset.tsx`) and the context editor
// (`context-editor-inset.tsx`). Both drive the same `useFileEditor` reconcile
// state machine; keeping the banners here too means the two surfaces can't drift
// (the epic guardrail: "the reconcile logic must not diverge"). Each caller
// passes its own `testIdPrefix` (`worktree` | `context-editor`) so the existing
// per-surface test hooks are preserved.
// ---------------------------------------------------------------------------

const AMBER_BAR =
  "flex flex-wrap items-center gap-x-2 gap-y-1 border-b border-amber-500/40 bg-amber-500/10 px-2.5 py-1 text-[0.7rem] text-amber-700 dark:text-amber-400";
const PRIMARY_BTN =
  "shrink-0 rounded border border-amber-500/60 bg-amber-500/20 px-1.5 py-0.5 font-medium hover:bg-amber-500/30";
const AMBER_BTN =
  "shrink-0 rounded border border-amber-500/50 px-1.5 py-0.5 font-medium hover:bg-amber-500/20";
const NEUTRAL_BTN =
  "shrink-0 rounded border border-border px-1.5 py-0.5 font-medium hover:bg-accent";

interface StaleConflictBannerProps {
  testIdPrefix: string;
  /** Where the other copy lives, as the copy says it: "on disk" (default) or
   *  "on the machine" for a remote worktree. */
  where?: string;
  onResolve: () => void;
  onReload: () => void;
  onKeepEditing: () => void;
}

/** Disk changed under a dirty buffer — the two-writers conflict. Non-destructive:
 *  "Resolve…" opens the merge view, "Reload" takes disk, "Keep editing" dismisses
 *  the warning while the save-guard stays armed. Hidden while the merge view is up. */
export function StaleConflictBanner({
  testIdPrefix,
  where = "on disk",
  onResolve,
  onReload,
  onKeepEditing,
}: StaleConflictBannerProps) {
  return (
    <div role="alert" data-testid={`${testIdPrefix}-stale-warning`} className={AMBER_BAR}>
      <span className="min-w-[12rem] flex-1">
        This file changed {where} while you were editing. Your edits aren’t saved yet.
      </span>
      <button
        type="button"
        data-testid={`${testIdPrefix}-stale-resolve`}
        onClick={onResolve}
        className={PRIMARY_BTN}
      >
        Compare and merge…
      </button>
      <button
        type="button"
        data-testid={`${testIdPrefix}-stale-reload`}
        onClick={onReload}
        className={AMBER_BTN}
      >
        Reload (lose my edits)
      </button>
      <button
        type="button"
        data-testid={`${testIdPrefix}-stale-keep`}
        onClick={onKeepEditing}
        className={NEUTRAL_BTN}
      >
        Keep editing
      </button>
    </div>
  );
}

interface ReconcileBarProps {
  testIdPrefix: string;
  /** As {@link StaleConflictBannerProps.where}. */
  where?: string;
  onTakeTheirs: () => void;
  onKeepMine: () => void;
  onCancel: () => void;
  /** The file changed again since the merge opened (the left side is frozen
   *  until refreshed). */
  diskMoved?: boolean;
  /** Load the latest version into the left side. */
  onRefresh?: () => void;
}

/** The action bar over the `CodeMergeView`: disk (left) vs your edits (right).
 *  Resolve by taking disk, keeping the buffer (the sole sanctioned clobber), or
 *  hand-merging in the view then ⌘S. */
export function ReconcileBar({
  testIdPrefix,
  where = "on disk",
  onTakeTheirs,
  onKeepMine,
  onCancel,
  diskMoved = false,
  onRefresh,
}: ReconcileBarProps) {
  return (
    <div
      role="group"
      aria-label="Resolve conflict"
      data-testid={`${testIdPrefix}-reconcile-bar`}
      className={AMBER_BAR}
    >
      <span className="min-w-[12rem] flex-1">
        Left: the file {where} now. Right: your edits — change them as needed, then ⌘S to save the
        merge.
        {diskMoved && " The file changed again since — refresh to see the latest."}
      </span>
      {diskMoved && onRefresh && (
        <button
          type="button"
          data-testid={`${testIdPrefix}-reconcile-refresh`}
          onClick={onRefresh}
          className={NEUTRAL_BTN}
        >
          Refresh
        </button>
      )}
      <button
        type="button"
        data-testid={`${testIdPrefix}-reconcile-take-theirs`}
        onClick={onTakeTheirs}
        className={AMBER_BTN}
      >
        Use the file {where}
      </button>
      <button
        type="button"
        data-testid={`${testIdPrefix}-reconcile-keep-mine`}
        onClick={onKeepMine}
        className={PRIMARY_BTN}
      >
        Save mine over it
      </button>
      <button
        type="button"
        data-testid={`${testIdPrefix}-reconcile-cancel`}
        onClick={onCancel}
        className={NEUTRAL_BTN}
      >
        Back to editing
      </button>
    </div>
  );
}

interface ReconcileErrorBannerProps {
  testIdPrefix: string;
  /** The raw failure reason. Retained on the API (callers still pass it) but
   *  SW-FX-80 no longer surfaces it as a native `title` — the banner shows only
   *  the reassuring line, keeping the copy non-alarming. */
  message?: string;
}

/** A failed fresh-disk read at reconcile entry, an unviewable disk, or a failed
 *  resolution write — surfaced inline; the buffer/edits are never dropped. */
export function ReconcileErrorBanner({ testIdPrefix }: ReconcileErrorBannerProps) {
  return (
    <div
      role="alert"
      data-testid={`${testIdPrefix}-reconcile-error`}
      className="flex items-center gap-2 border-b border-destructive/40 bg-destructive/10 px-2.5 py-1 text-[0.7rem] text-destructive"
    >
      <span className="min-w-0 flex-1 truncate">Couldn’t complete that — your edits are safe.</span>
    </div>
  );
}
