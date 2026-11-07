// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";
import { AlertTriangle, ArrowDown, ArrowUp, Check, CircleSlash } from "lucide-react";
import { toast } from "sonner";

import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { getErrorMessage } from "@/shared/lib/error-message";
import { isRemoteWorkspace } from "@/shared/lib/workspace-location";
import { selectTabBySessionId, useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import { cancelContextOperation, type ContextOperationTarget } from "@/tauri/commands";
import {
  KeptModifiedNote,
  workspaceOperationTarget,
} from "@/features/viewer/components/kept-modified-note";

import {
  describeOperation,
  OPERATION_FADE_MS,
  operationFacts,
  publishConflicts,
  selectChipOperation,
  useContextOperationsStore,
  workspaceIdentityKey,
  type ContextOperation,
  type OperationView,
} from "../stores/context-operations-store";
import { useSurfaceSyncStatus } from "../stores/surface-sync-store";

/**
 * SW-XS-10 (SPEC §8.2): the workspace pill's operation chip. It shows ONLY
 * explicit operations an agent (or a person) ran on this workspace — running,
 * or just finished: `↓ Hydrating 18 files`, `✓ Hydrated 18 files · 0.6 s`,
 * `↑ Publishing 3 changes`. A finished chip fades after 4 s; a failure or a
 * partial result (a publish with conflicts) sticks until dismissed. Clicking it
 * opens the operation's details: who called it, files, bytes, duration and what
 * was kept because it was modified — with Stop while it runs.
 *
 * Replaces the wave-driven surface-sync chip: nothing syncs in the background,
 * so nothing else lights a workspace. (An epic's assembly failure still shows
 * here, as a sticky warning.)
 */

/** The chip's clock: one timer to the next fade boundary of this workspace's
 *  finished operations (the fade starting, the chip going quiet). Read only in
 *  the effect, so an operation newer than the last tick reads as "just now". */
function useChipClock(byId: Record<string, ContextOperation>, identities: string[]): number {
  const [now, setNow] = useState(() => Date.now());
  const key = identities.join("\u0000");
  useEffect(() => {
    const t = Date.now();
    const keys = new Set(key.split("\u0000").filter(Boolean).map(workspaceIdentityKey));
    let next: number | null = null;
    for (const op of Object.values(byId)) {
      if (op.settledAt === undefined || describeOperation(op).sticky) continue;
      if (!keys.has(workspaceIdentityKey(op.workspace))) continue;
      for (const at of [
        op.settledAt + OPERATION_FADE_MS - FADE_OUT_MS,
        op.settledAt + OPERATION_FADE_MS,
      ]) {
        if (at > t && (next === null || at < next)) next = at;
      }
    }
    if (next === null) return;
    const timer = setTimeout(() => setNow(Date.now()), next - t + 1);
    return () => clearTimeout(timer);
  }, [byId, key, now]);
  return now;
}

/** How long the chip takes to fade out. */
const FADE_OUT_MS = 300;

/** The identities an operation event names this workspace by. */
function identitiesOf(ws: Workspace): string[] {
  return isRemoteWorkspace(ws) ? [ws.location.handle ?? ""] : [ws.cwd];
}

const TONE_COLOR: Record<OperationView["tone"], string> = {
  busy: "text-[var(--accent)]",
  done: "text-[var(--cs-status-done)]",
  partial: "text-[var(--cs-status-needs)]",
  failed: "text-[var(--cs-status-failed)]",
  stopped: "text-[var(--muted-foreground)]",
};

function OperationIcon({ view }: { view: OperationView }) {
  const cls = `size-[11px] shrink-0 ${TONE_COLOR[view.tone]}`;
  if (view.tone === "busy")
    return view.direction === "up" ? (
      <ArrowUp className={cls} aria-hidden />
    ) : (
      <ArrowDown className={cls} aria-hidden />
    );
  if (view.tone === "done") return <Check className={cls} strokeWidth={2.5} aria-hidden />;
  if (view.tone === "stopped") return <CircleSlash className={cls} aria-hidden />;
  return <AlertTriangle className={cls} aria-hidden />;
}

/** Who called it, in a person's words. */
export function useCallerName(caller: string): string {
  const tabName = useViewerStore((s) => selectTabBySessionId(s, caller)?.name);
  if (tabName) return tabName;
  if (caller === "explicit") return "An agent on the machine";
  return caller;
}

/** Where "Discard my edits" for this operation runs: the same workspace —
 *  its machine handle, or (This Mac) its worktree through the session that
 *  called it, else any live tab there. */
function operationTarget(
  op: ContextOperation,
  workspace: Workspace | undefined,
): ContextOperationTarget | null {
  if (workspace) {
    const teamKey = Object.entries(useViewerStore.getState().workspaces).find(([, list]) =>
      list.some((w) => w.id === workspace.id),
    )?.[0];
    const viaWorkspace = teamKey ? workspaceOperationTarget(workspace, teamKey, op.caller) : null;
    if (viaWorkspace) return viaWorkspace;
  }
  return op.caller && op.caller !== "explicit"
    ? { kind: "local", codeRepoPath: op.workspace, sessionId: op.caller }
    : null;
}

/** The details an operation chip opens. Exported for the harness and tests. */
export function OperationDetails({
  op,
  workspace,
}: {
  op: ContextOperation;
  /** The workspace it ran on, for "Discard my edits". */
  workspace?: Workspace;
}) {
  const caller = useCallerName(op.caller);
  const view = describeOperation(op);
  const s = op.state.summary;
  const conflicts = publishConflicts(op);
  const running = op.settledAt === undefined;
  const [stopping, setStopping] = useState(false);
  const stop = () => {
    setStopping(true);
    useContextOperationsStore.getState().markStopping(op.id);
    cancelContextOperation(op.id)
      .then((cancelling) => {
        if (!cancelling) {
          // It ended (or isn't this app's): no terminal event will follow, so
          // it leaves the chip rather than spinning on.
          useContextOperationsStore.getState().remove(op.id);
          toast("That operation had already finished");
        }
      })
      .catch((e: unknown) => toast.error(`Couldn't stop it: ${getErrorMessage(e)}`))
      .finally(() => setStopping(false));
  };
  const lists: Array<[string, string[], number]> = s
    ? [
        ["Kept: CodeSense didn't copy them", s.keptUnmanaged, s.keptUnmanagedCount],
        ["Skipped: tracked by the repo", s.skippedTracked, s.skippedTrackedCount],
      ]
    : [];
  return (
    <div className="flex flex-col gap-2 text-xs" data-testid={`operation-details-${op.id}`}>
      <div className="flex items-center gap-1.5 font-medium text-[var(--foreground)]">
        <OperationIcon view={view} />
        <span className="min-w-0 flex-1">{view.text}</span>
      </div>
      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
        <dt className="text-[var(--muted-foreground)]">Called by</dt>
        <dd className="min-w-0 truncate" title={caller}>
          {caller}
        </dd>
        {operationFacts(op).map(([k, v]) => (
          <div key={k} className="contents">
            <dt className="text-[var(--muted-foreground)]">{k}</dt>
            <dd className="tabular-nums">{v}</dd>
          </div>
        ))}
        {conflicts > 0 && (
          <>
            <dt className="text-[var(--muted-foreground)]">Conflicts</dt>
            <dd className="tabular-nums">
              {conflicts} not written — team content changed under them; they need a person
            </dd>
          </>
        )}
      </dl>
      {lists
        .filter(([, , n]) => n > 0)
        .map(([title, paths, n]) => (
          <section key={title} aria-label={title}>
            <h4 className="text-[var(--muted-foreground)]">
              {title} <span className="tabular-nums">({n})</span>
            </h4>
            <ul className="mt-0.5 max-h-24 overflow-auto font-mono text-[11px]">
              {paths.map((p) => (
                <li key={p} className="truncate select-text" title={p}>
                  {p}
                </li>
              ))}
              {n > paths.length && (
                <li className="font-sans text-[var(--muted-foreground)]">
                  and {n - paths.length} more
                </li>
              )}
            </ul>
          </section>
        ))}
      {s && s.keptModifiedCount > 0 && (
        // SW-XS-10 (§11.2): what it kept because it was edited, and the
        // scoped recovery for exactly those files.
        <KeptModifiedNote
          variant="popover"
          paths={s.keptModified}
          count={s.keptModifiedCount}
          target={operationTarget(op, workspace)}
          onDiscarded={() => useContextOperationsStore.getState().resolveKept(op.id)}
        />
      )}
      {s && s.failed.length > 0 && (
        <section aria-label="Not written">
          <h4 className="text-[var(--cs-status-failed)]">
            Not written <span className="tabular-nums">({s.failedCount})</span>
          </h4>
          <ul className="mt-0.5 max-h-24 overflow-auto text-[11px]">
            {s.failed.map(([p, why]) => (
              <li key={p} className="break-words select-text">
                <span className="font-mono">{p}</span>
                <span className="text-[var(--muted-foreground)]"> — {why}</span>
              </li>
            ))}
          </ul>
        </section>
      )}
      {/* A publish's note repeats its counts above; a hydrate's adds the startup line. */}
      {op.state.note && op.state.settled && op.state.operationKind !== "publish" && (
        <p className="text-[var(--muted-foreground)] select-text">{op.state.note}</p>
      )}
      <div className="flex justify-end gap-1.5">
        {running ? (
          <button
            type="button"
            disabled={stopping || op.stopping}
            onClick={stop}
            className="inline-flex min-h-6 items-center rounded border border-[var(--border)] px-2 text-[11px] text-[var(--foreground)] hover:bg-[color-mix(in_srgb,var(--accent)_14%,transparent)] focus-visible:outline focus-visible:outline-2 focus-visible:outline-[var(--accent)] disabled:opacity-50"
            data-testid="operation-stop"
          >
            {stopping || op.stopping ? "Stopping…" : "Stop"}
          </button>
        ) : view.sticky ? (
          <button
            type="button"
            onClick={() => useContextOperationsStore.getState().dismiss(op.id)}
            className="inline-flex min-h-6 items-center rounded border border-[var(--border)] px-2 text-[11px] text-[var(--muted-foreground)] hover:text-[var(--foreground)] focus-visible:outline focus-visible:outline-2 focus-visible:outline-[var(--accent)]"
            data-testid="operation-dismiss"
          >
            Dismiss
          </button>
        ) : null}
      </div>
    </div>
  );
}

/** The chip itself, presentational. `open`/`onOpenChange` are lifted so the
 *  chip can pin the operation it was opened for. */
export function OperationChipView({
  op,
  now,
  workspace,
  open: openProp,
  onOpenChange,
}: {
  op: ContextOperation;
  now: number;
  workspace?: Workspace;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
}) {
  const view = describeOperation(op);
  const [ownOpen, setOwnOpen] = useState(false);
  const open = openProp ?? ownOpen;
  const setOpen = onOpenChange ?? setOwnOpen;
  // Never fades while its details are open.
  const fading =
    !open &&
    !view.sticky &&
    op.settledAt !== undefined &&
    now >= op.settledAt + OPERATION_FADE_MS - FADE_OUT_MS;
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          type="button"
          aria-haspopup="dialog"
          aria-expanded={open}
          aria-label={`${view.text}. Show details`}
          data-testid="operation-chip"
          data-tone={view.tone}
          className={`inline-flex min-h-6 min-w-0 max-w-[15rem] shrink items-center gap-1 rounded px-1.5 text-[0.68rem] leading-4 transition-opacity duration-300 ease-out motion-reduce:transition-none hover:bg-[color-mix(in_srgb,var(--accent)_12%,transparent)] focus-visible:outline focus-visible:outline-1 focus-visible:outline-[var(--accent)] ${
            fading ? "opacity-0" : "opacity-100"
          } ${view.tone === "failed" ? "text-[var(--foreground)]" : "text-[var(--muted-foreground)]"}`}
        >
          <OperationIcon view={view} />
          <span className="truncate tabular-nums" data-testid="operation-chip-text">
            {view.text}
          </span>
        </button>
      </PopoverTrigger>
      <PopoverContent side="top" align="start" className="w-80 p-3">
        <OperationDetails op={op} workspace={workspace} />
      </PopoverContent>
    </Popover>
  );
}

/** The chip for one workspace: the operation to show now, if any. While its
 *  details are open the chip stays on the operation it was opened for — a
 *  newer operation (a discard it started) or a fade never swaps it out. */
export function OperationChip({ workspace }: { workspace: Workspace }) {
  const byId = useContextOperationsStore((s) => s.byId);
  const identities = identitiesOf(workspace);
  const now = useChipClock(byId, identities);
  const [pinned, setPinned] = useState<string | null>(null);
  const selected = selectChipOperation(byId, identities, now);
  // A dismissed operation lets go of the pin (and its details close).
  const pinnedOp = pinned ? byId[pinned] : undefined;
  const op = pinnedOp && !pinnedOp.dismissed ? pinnedOp : selected;
  // An epic's worktree assembly failure keeps its sticky warning here.
  const assembly = useSurfaceSyncStatus(workspace.kind === "epic" ? workspace.cwd : undefined);
  if (op)
    return (
      <OperationChipView
        op={op}
        now={now}
        workspace={workspace}
        open={pinned !== null && pinned === op.id}
        onOpenChange={(open) => setPinned(open ? op.id : null)}
      />
    );
  if (assembly?.state === "failed") {
    return (
      <span
        className="ml-0.5 inline-flex shrink-0 items-center"
        data-testid="operation-chip-assembly-failed"
        title={assembly.errorMessage ?? "Setup failed"}
      >
        <AlertTriangle
          className="size-[11px] text-[var(--cs-status-failed)]"
          aria-label={assembly.errorMessage ?? "Setup failed"}
        />
      </span>
    );
  }
  return null;
}

/** What the workspace's status region says about its operation now: the verb
 *  while it runs (never its counts), its line once it settled. */
export function useOperationAnnouncement(workspace: Workspace): string | undefined {
  const byId = useContextOperationsStore((s) => s.byId);
  const identities = identitiesOf(workspace);
  const now = useChipClock(byId, identities);
  const op = selectChipOperation(byId, identities, now);
  if (!op) return undefined;
  const view = describeOperation(op);
  if (op.settledAt === undefined) {
    const publish = op.state.operationKind === "publish";
    return op.stopping ? "Stopping" : publish ? "Publishing" : "Hydrating";
  }
  return view.text;
}
