// SPDX-License-Identifier: MIT
import { useState } from "react";
import { PencilLine, X } from "lucide-react";

import { keptModifiedNote } from "@/features/agents/lib/agent-prepare-progress";
import { getErrorMessage } from "@/shared/lib/error-message";
import { isRemoteWorkspace } from "@/shared/lib/workspace-location";
import { selectWorkspaceTabs, useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import {
  discardContextEdits,
  type ContextOperationResult,
  type ContextOperationTarget,
} from "@/tauri/commands";

import { ICON_BUTTON, PRIMARY_BUTTON, SECONDARY_BUTTON } from "./setup-line";

/**
 * SW-XS-10 (SPEC §11.2): the persistent "N files kept (modified)" note a
 * preparation or hydrate leaves when it kept files someone edited, with
 * "Discard my edits" — `hydrate_worktree {paths, overwriteModified:true}` for
 * exactly those files (never unscoped), behind an inline confirm, and an honest
 * result: how many were replaced, and which were not and why.
 */

/** The target an explicit operation on this workspace names: its machine
 *  handle, or (on This Mac) its worktree with a live tab session as the
 *  admission. Null when a local workspace has no live tab to admit through. */
export function workspaceOperationTarget(
  ws: Workspace,
  teamKey: string,
  sessionHint?: string,
): ContextOperationTarget | null {
  if (isRemoteWorkspace(ws)) {
    return ws.location.handle
      ? { kind: "remote", machineId: ws.location.machineId, workspaceHandle: ws.location.handle }
      : null;
  }
  const tabs = selectWorkspaceTabs(useViewerStore.getState(), teamKey, ws.id);
  const live = tabs.find((t) => t.sessionId === sessionHint) ?? tabs.find((t) => !!t.sessionId);
  return ws.cwd && live ? { kind: "local", codeRepoPath: ws.cwd, sessionId: live.sessionId } : null;
}

/** The plain-language result of a discard, and the files still kept (the
 *  note's new list). A file already identical to the team's counts as done; a
 *  file the team no longer has is said so, and leaves the list (there is
 *  nothing to replace it with). */
export function discardOutcome(
  asked: readonly string[],
  result: ContextOperationResult,
): { ok: boolean; text: string; remaining: string[] } {
  const written = result.written ?? 0;
  const unchanged = result.unchanged ?? 0;
  const failed = result.failed ?? [];
  const failedCount = result.failedCount ?? failed.length;
  const stillKept = result.keptModified ?? [];
  const gone = (result.notInTeam ?? []).filter((p) => asked.includes(p));
  const replaceable = asked.filter((p) => !gone.includes(p));
  const files = (n: number) => `${n} ${n === 1 ? "file" : "files"}`;
  const goneText =
    gone.length === 0
      ? ""
      : gone.length === 1
        ? `${gone[0]} is no longer in team content`
        : `${files(gone.length)} are no longer in team content`;
  const remaining = replaceable.filter(
    (p) => stillKept.includes(p) || failed.some(([path]) => path === p),
  );
  if (result.status === "cancelled")
    return { ok: false, text: "Stopped — nothing was replaced", remaining: [...asked] };
  const done = Math.min(replaceable.length, written + unchanged);
  if (done >= replaceable.length && failedCount === 0 && (result.keptModifiedCount ?? 0) === 0) {
    const replaced =
      replaceable.length > 0 ? `Replaced ${files(replaceable.length)} with the team's version` : "";
    return { ok: true, text: [replaced, goneText].filter(Boolean).join("; "), remaining: [] };
  }
  const why = failed[0] ? ` (${failed[0][0]}: ${failed[0][1]})` : "";
  const notDone = Math.max(remaining.length, replaceable.length - done, 1);
  return {
    ok: false,
    text:
      `Replaced ${done} of ${replaceable.length}; ${files(notDone)} still kept${why}` +
      (goneText ? `; ${goneText}` : ""),
    remaining: remaining.length ? remaining : replaceable.filter((_, i) => i >= done),
  };
}

export interface KeptModifiedNoteProps {
  paths: string[];
  count: number;
  target: ContextOperationTarget | null;
  /** The run replaced every file: the note can go. */
  onDiscarded?: () => void;
  /** A partial run: the files still kept (the note's new list). */
  onRemaining?: (paths: string[]) => void;
  onDismiss?: () => void;
  /** `corner`: over the terminal (dark set); `popover`: in operation details. */
  variant?: "corner" | "popover";
}

export function KeptModifiedNote({
  paths,
  count,
  target,
  onDiscarded,
  onRemaining,
  onDismiss,
  variant = "corner",
}: KeptModifiedNoteProps) {
  const [step, setStep] = useState<"idle" | "confirm" | "running">("idle");
  const [outcome, setOutcome] = useState<{ ok: boolean; text: string } | null>(null);
  // After a partial run the note lists only what is still kept.
  const [left, setLeft] = useState<{ paths: string[]; count: number } | null>(null);
  const shownPaths = left?.paths ?? paths;
  const shownCount = left?.count ?? count;
  const label = keptModifiedNote(shownCount) ?? "";
  const partialList = shownCount > shownPaths.length;

  const discard = () => {
    if (!target) return;
    setStep("running");
    const asked = shownPaths;
    discardContextEdits(target, asked)
      .then((result) => {
        const next = discardOutcome(asked, result);
        setOutcome({ ok: next.ok, text: next.text });
        if (next.ok) onDiscarded?.();
        else {
          const count = shownCount - (asked.length - next.remaining.length);
          setLeft({ paths: next.remaining, count });
          onRemaining?.(next.remaining);
        }
      })
      .catch((e: unknown) =>
        setOutcome({ ok: false, text: `Couldn't discard: ${getErrorMessage(e)}` }),
      )
      .finally(() => setStep("idle"));
  };

  return (
    <div
      data-surface={variant === "corner" ? "terminal" : undefined}
      className={`flex min-w-0 flex-col gap-1 text-xs ${
        variant === "corner"
          ? "rounded border border-[var(--border)] bg-[var(--popover)] px-2 py-1"
          : ""
      }`}
      data-testid="kept-modified-note"
    >
      <div className="flex min-w-0 items-center gap-2 text-[var(--muted-foreground)]">
        <PencilLine size={12} className="shrink-0" aria-hidden />
        <span
          className="min-w-0 flex-1 truncate text-[var(--foreground)]"
          title={shownPaths.join("\n")}
          data-testid="kept-modified-label"
        >
          {label}
        </span>
        {step === "idle" && !outcome?.ok && (
          <button
            type="button"
            className={SECONDARY_BUTTON}
            disabled={!target}
            title={target ? undefined : "Open a terminal in this workspace to discard its edits"}
            onClick={() => setStep("confirm")}
          >
            Discard my edits
          </button>
        )}
        {onDismiss && step !== "running" && (
          <button type="button" aria-label="Dismiss" className={ICON_BUTTON} onClick={onDismiss}>
            <X size={12} />
          </button>
        )}
      </div>
      {step === "confirm" && (
        <div
          role="group"
          aria-label="Confirm discarding edits"
          className="flex flex-col gap-1.5 pl-5 text-[var(--foreground)]"
        >
          <p>
            Replace {shownPaths.length === 1 ? "this file" : `these ${shownPaths.length} files`}{" "}
            with the team&apos;s version? Your edits to {shownPaths.length === 1 ? "it" : "them"}{" "}
            are lost.
            {partialList &&
              ` Only the ${shownPaths.length} listed are replaced; the rest stay as they are.`}
          </p>
          <ul className="max-h-20 overflow-auto font-mono text-[11px] text-[var(--muted-foreground)]">
            {shownPaths.map((p) => (
              <li key={p} className="truncate" title={p}>
                {p}
              </li>
            ))}
          </ul>
          <div className="flex gap-1.5">
            <button type="button" className={PRIMARY_BUTTON} onClick={discard}>
              Replace with team&apos;s version
            </button>
            <button type="button" className={SECONDARY_BUTTON} onClick={() => setStep("idle")}>
              Keep my edits
            </button>
          </div>
        </div>
      )}
      {step === "running" && (
        <p role="status" className="pl-5 text-[var(--muted-foreground)]">
          Replacing…
        </p>
      )}
      {outcome && step === "idle" && (
        <p
          role={outcome.ok ? "status" : "alert"}
          className={`pl-5 ${outcome.ok ? "text-[var(--muted-foreground)]" : "text-[var(--foreground)]"}`}
          data-testid="kept-modified-outcome"
        >
          {outcome.text}
        </p>
      )}
    </div>
  );
}

/** The workspace's persistent note, over its terminal (corner), from the last
 *  agent preparation. */
export function WorkspaceKeptModified({ ws, teamKey }: { ws: Workspace; teamKey: string }) {
  const kept = ws.keptModified;
  if (!kept) return null;
  const clear = () => useViewerStore.getState().setWorkspaceKeptModified(teamKey, ws.id, undefined);
  return (
    <KeptModifiedNote
      paths={kept.paths}
      count={kept.count}
      target={workspaceOperationTarget(ws, teamKey)}
      onDiscarded={() => setTimeout(clear, 2500)}
      onRemaining={(remaining) =>
        useViewerStore
          .getState()
          .setWorkspaceKeptModified(
            teamKey,
            ws.id,
            remaining.length ? { ...kept, paths: remaining, count: remaining.length } : undefined,
          )
      }
      onDismiss={clear}
    />
  );
}
