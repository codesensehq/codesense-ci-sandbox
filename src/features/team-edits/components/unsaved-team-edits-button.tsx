// SPDX-License-Identifier: MIT
import { useRef, useState } from "react";
import { toast } from "sonner";

import { Popover, PopoverAnchor, PopoverContent } from "@/components/ui/popover";
import { cn } from "@/lib/utils";
import { Chip } from "@/features/git-sync/components/file-status-chip";
import { ConflictGlyph } from "@/features/git-sync/components/git-status-button";
import { getErrorMessage } from "@/shared/lib/error-message";
import {
  discardTeamFileEdits,
  forgetMissingWorkspace,
  publishContextEdits,
  type AppWorkspaceTarget,
  type ContextOperationResult,
  type SeenTeamFile,
  type TeamFileEdit,
  type TeamFileEditsWorkspace,
} from "@/tauri/commands";

import {
  baseName,
  counterSentence,
  counterVisible,
  dirName,
  discardSentence,
  editCount,
  fileActions,
  fileNote,
  KIND_CHIP,
  plainError,
  publishAllPaths,
  removalRefusal,
  unsureSignature,
  workspaceKey,
  workspaceLine,
} from "../lib/team-edits";
import { remoteKey, selectTeamEdits, useTeamEditsStore } from "../stores/team-edits-store";
import { useTeamEditsRefresh } from "../stores/use-team-edits-refresh";
import { TeamEditDiffDialog } from "./team-edit-diff-dialog";

const CARD_BTN =
  "inline-flex h-[22px] items-center rounded-[5px] border border-border px-1.5 text-[11px] text-foreground hover:bg-[color-mix(in_srgb,var(--foreground)_6%,transparent)] focus-visible:outline-none focus-visible:ring-[1.5px] focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-45";
/** A row's quiet actions: words, no frame, until hovered or focused. */
const ROW_BTN =
  "inline-flex h-[22px] items-center rounded-[5px] px-1.5 text-[11px] text-muted-foreground hover:bg-[color-mix(in_srgb,var(--foreground)_7%,transparent)] hover:text-foreground focus-visible:text-foreground focus-visible:outline-none focus-visible:ring-[1.5px] focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:bg-transparent";
const CARD_BTN_PRIMARY =
  "inline-flex h-[22px] items-center rounded-[5px] border border-foreground bg-foreground px-2 text-[11px] font-[550] text-popover hover:opacity-90 focus-visible:outline-none focus-visible:ring-[1.5px] focus-visible:ring-ring focus-visible:ring-offset-1 disabled:cursor-not-allowed disabled:opacity-45";

/** A pen over a line: the "unsaved" mark, in the scope row's stroke family. */
function UnsavedGlyph() {
  return (
    <svg width="11" height="11" viewBox="0 0 16 16" fill="none" aria-hidden>
      <g stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
        <path d="M10.5 2.5l3 3L6 13H3v-3z" />
        <path d="M9 14h4.5" />
      </g>
    </svg>
  );
}

/** The workspace an action names, as the app's own request (no agent session):
 *  `null` with the reason when nothing can run there now. */
function actionTarget(ws: TeamFileEditsWorkspace): {
  target: AppWorkspaceTarget | null;
  why: string | null;
} {
  if (ws.target.kind === "remote" && ws.machineOnline === false) {
    return {
      target: null,
      why: "The machine is offline. Publish and Discard work once it reconnects.",
    };
  }
  return { target: ws.target, why: null };
}

/** What the person saw of these files: an action leaves alone one rewritten since. */
function seenOf(ws: TeamFileEditsWorkspace, paths: string[]): Record<string, SeenTeamFile> {
  const seen: Record<string, SeenTeamFile> = {};
  for (const f of ws.files ?? []) {
    if (!paths.includes(f.path)) continue;
    if (f.workspaceHash || f.teamHash || f.workspaceStat)
      seen[f.path] = {
        workspaceHash: f.workspaceHash,
        teamHash: f.teamHash,
        workspaceStat: f.workspaceStat,
      };
  }
  return seen;
}

type RowResult = { ok: boolean; text: string };

type PublishResult = ContextOperationResult & {
  conflicts?: unknown[];
  published?: number;
  entries?: unknown[];
  skipped?: [string, string][];
  withheldRemovals?: string[];
};

/** What one action did, said plainly — "Published." / "Discarded." only when the
 *  file really went: a skipped, withheld or conflicting file, or a copied file the
 *  team no longer has, says why instead. */
function resultText(
  verb: "publish" | "discard",
  paths: string[],
  result: PublishResult,
): RowResult {
  const failed = result.failedCount ?? result.failed?.length ?? 0;
  const conflicts = Array.isArray(result.conflicts) ? result.conflicts.length : 0;
  if (result.status === "cancelled") return { ok: false, text: "Stopped. Nothing changed." };
  const notDone = (what: string): RowResult => ({
    ok: false,
    text: verb === "publish" ? `Not published: ${what}.` : `Not discarded: ${what}.`,
  });
  if (conflicts > 0) return notDone("the team changed it too");
  if (failed > 0 || result.status === "failed") {
    const why = result.failed?.[0]?.[1];
    return notDone(why ? plainError(why) : "it didn't go through");
  }
  if (verb === "publish") {
    const skipped = (result.skipped ?? []).find(([p]) => paths.includes(p));
    if (skipped) return notDone(plainError(skipped[1]));
    if ((result.withheldRemovals ?? []).some((p) => paths.includes(p)))
      return notDone("it is still in the team");
    if (result.status === "nothingToPublish") return notDone("the team already has it");
    return { ok: true, text: "Published." };
  }
  if ((result.notInTeam ?? []).some((p) => paths.includes(p)))
    return notDone("the team no longer has this file");
  return { ok: true, text: "Discarded." };
}

function FileRow({
  file,
  disabled,
  disabledWhy,
  onView,
  onPublish,
  onDiscard,
}: {
  file: TeamFileEdit;
  disabled: boolean;
  disabledWhy: string | null;
  onView: () => void;
  onPublish: () => Promise<RowResult>;
  onDiscard: () => Promise<RowResult>;
}) {
  const [confirming, setConfirming] = useState(false);
  const discardRef = useRef<HTMLButtonElement>(null);
  // Leaving the confirm puts focus back on the row's Discard, never on <body>.
  const endConfirm = () => {
    setConfirming(false);
    requestAnimationFrame(() => discardRef.current?.focus());
  };
  const [busy, setBusy] = useState<null | "publish" | "discard">(null);
  const [result, setResult] = useState<RowResult | null>(null);
  const chip = KIND_CHIP[file.kind];
  const actions = fileActions(file);
  const note = fileNote(file);
  const run = async (verb: "publish" | "discard", act: () => Promise<RowResult>) => {
    setBusy(verb);
    setResult(null);
    try {
      setResult(await act());
    } catch (e) {
      setResult({ ok: false, text: plainError(getErrorMessage(e)) });
    } finally {
      setBusy(null);
      if (verb === "discard") endConfirm();
      else setConfirming(false);
    }
  };
  const off = disabled || busy !== null;
  return (
    <li className="px-3 py-1" data-testid="team-edit-row" data-kind={file.kind}>
      <div className="flex min-w-0 items-start gap-2">
        <span className="mt-[2px] flex-none">
          <Chip letter={chip.letter} cssVar={chip.cssVar} label={chip.word} />
        </span>
        <span className="min-w-0 flex-1" title={file.path}>
          <span className="block truncate text-foreground">{baseName(file.path)}</span>
          {dirName(file.path) && (
            <span className="block truncate text-[11px] text-muted-foreground">
              {dirName(file.path)}
            </span>
          )}
          {note && !confirming && (
            <span
              className={cn(
                "block text-[11px]",
                file.kind === "conflict"
                  ? "text-[var(--cs-git-conflict)]"
                  : "text-muted-foreground",
              )}
            >
              {note}
            </span>
          )}
        </span>
        {!confirming && (
          <span className="flex flex-none items-center">
            {actions.view && (
              <button type="button" className={ROW_BTN} onClick={onView} disabled={busy !== null}>
                View
              </button>
            )}
            {actions.publish && (
              <button
                type="button"
                className={ROW_BTN}
                disabled={off}
                title={
                  disabledWhy ??
                  (file.kind === "removed"
                    ? "Remove it from the team too"
                    : "Send this change to the team")
                }
                onClick={() => void run("publish", onPublish)}
              >
                {busy === "publish" ? "Publishing…" : "Publish"}
              </button>
            )}
            {actions.discard && (
              <button
                ref={discardRef}
                type="button"
                className={ROW_BTN}
                disabled={off}
                title={disabledWhy ?? undefined}
                onClick={() => setConfirming(true)}
              >
                {busy === "discard" ? "Discarding…" : "Discard"}
              </button>
            )}
          </span>
        )}
      </div>
      {confirming && (
        <div
          className="mt-1 flex items-center gap-2 pl-6"
          role="group"
          aria-label="Confirm discard"
          onKeyDown={(e) => {
            // Escape cancels the confirm, not the whole list.
            if (e.key === "Escape") {
              e.stopPropagation();
              e.preventDefault();
              endConfirm();
            }
          }}
        >
          <span className="min-w-0 flex-1 text-[11px] text-foreground">
            {discardSentence(file)}
          </span>
          <button
            type="button"
            className={CARD_BTN_PRIMARY}
            onClick={() => void run("discard", onDiscard)}
          >
            Discard
          </button>
          {/* No undo: the safe choice holds focus, so Enter twice never throws work away. */}
          <button type="button" className={CARD_BTN} autoFocus onClick={endConfirm}>
            Cancel
          </button>
        </div>
      )}
      {result && (
        <p
          className={cn(
            "mt-0.5 pl-6 text-[11px]",
            result.ok ? "text-muted-foreground" : "text-[var(--cs-git-conflict)]",
          )}
          role={result.ok ? "status" : "alert"}
        >
          {result.text}
        </p>
      )}
    </li>
  );
}

function WorkspaceSection({
  teamKey,
  ws,
  now,
  onView,
  onRemoved,
}: {
  teamKey: string;
  ws: TeamFileEditsWorkspace;
  now: number;
  onView: (file: TeamFileEdit) => void;
  onRemoved: (nextAction: HTMLButtonElement | null) => void;
}) {
  const sectionRef = useRef<HTMLElement>(null);
  const [removing, setRemoving] = useState(false);
  const counting = useTeamEditsStore((s) =>
    ws.target.kind === "remote"
      ? selectTeamEdits(s, teamKey).counting.includes(
          remoteKey(ws.target.machineId, ws.target.workspaceHandle),
        )
      : false,
  );
  const refresh = useTeamEditsStore((s) => s.refresh);
  const [allBusy, setAllBusy] = useState(false);
  const [allResult, setAllResult] = useState<RowResult | null>(null);
  const { target, why } = actionTarget(ws);
  const line = workspaceLine(ws, now);
  const files = ws.state === "missing" ? [] : (ws.files ?? []);
  const all = publishAllPaths(ws);
  const settle = () => void refresh(teamKey, ws.target.kind === "remote" ? ws.target : undefined);
  const remove = async () => {
    if (ws.target.kind !== "remote") return;
    const machine = ws.machineName ?? ws.target.machineId;
    setRemoving(true);
    setAllResult(null);
    try {
      const result = await forgetMissingWorkspace(ws.target);
      if (result.status === "removed") {
        const next =
          sectionRef.current?.nextElementSibling?.querySelector<HTMLButtonElement>(
            "button:not(:disabled)",
          ) ?? null;
        await refresh(teamKey);
        toast.success(`Removed ${ws.name} from the list. Nothing on ${machine} was deleted.`);
        onRemoved(next);
      } else {
        useTeamEditsStore.setState((s) => {
          const previous = selectTeamEdits(s, teamKey);
          if (!previous.status) return s;
          const workspaces = previous.status.workspaces.map((row) =>
            workspaceKey(row) === workspaceKey(ws)
              ? {
                  ...row,
                  ...result.edits,
                  state: result.state,
                  ...(result.state === "offline" ? { machineOnline: false } : {}),
                }
              : row,
          );
          return {
            byTeam: {
              ...s.byTeam,
              [teamKey]: {
                ...previous,
                status: {
                  ...previous.status,
                  workspaces,
                  total: workspaces.reduce((n, row) => n + editCount(row), 0),
                },
              },
            },
          };
        });
        setAllResult({ ok: false, text: removalRefusal(result, machine) });
      }
    } catch {
      setAllResult({ ok: false, text: "Not removed: couldn't confirm the folder is gone." });
    } finally {
      setRemoving(false);
    }
  };
  const publish = async (paths: string[], includeDeletes: boolean) => {
    if (!target) return { ok: false, text: why ?? "Not available here." };
    const result = await publishContextEdits(target, paths, includeDeletes, seenOf(ws, paths));
    settle();
    return resultText("publish", paths, result);
  };
  const discard = async (paths: string[]) => {
    if (!target) return { ok: false, text: why ?? "Not available here." };
    const result = await discardTeamFileEdits(target, paths, seenOf(ws, paths));
    settle();
    return resultText("discard", paths, result);
  };
  return (
    <section
      ref={sectionRef}
      className="border-t border-border py-1.5 first:border-t-0"
      data-testid="team-edits-workspace"
    >
      <div className="flex items-baseline gap-2 px-3 pb-0.5">
        <span
          className="min-w-0 flex-1 truncate font-semibold"
          title={ws.target.kind === "local" ? ws.target.codeRepoPath : ws.cwd}
        >
          {ws.name}
        </span>
        {editCount(ws) > 0 && (
          <span className="flex-none tabular-nums text-muted-foreground">{editCount(ws)}</span>
        )}
      </div>
      {(line || ws.target.kind === "remote") && (
        <div className="flex items-center gap-2 px-3 pb-1 text-[11px] text-muted-foreground">
          <span
            className="min-w-0 flex-1"
            role={ws.state === "unknown" && !allResult ? "alert" : undefined}
          >
            {removing ? "Checking the folder is still gone…" : counting ? "Checking…" : line}
          </span>
          {ws.target.kind === "remote" && ws.machineOnline && ws.state === "missing" && (
            <button
              type="button"
              className={CARD_BTN}
              disabled={removing || counting}
              onClick={() => void remove()}
            >
              {removing ? "Removing…" : "Remove from list"}
            </button>
          )}
          {ws.target.kind === "remote" && ws.machineOnline && ws.state !== "missing" && (
            <button
              type="button"
              className={CARD_BTN}
              disabled={counting}
              onClick={() => ws.target.kind === "remote" && void refresh(teamKey, ws.target)}
            >
              Check now
            </button>
          )}
        </div>
      )}
      {files.length > 0 && (
        <ul className="flex flex-col">
          {files.map((file) => (
            <FileRow
              key={`${file.kind}:${file.path}`}
              file={file}
              disabled={!target}
              disabledWhy={why}
              onView={() => onView(file)}
              onPublish={() => publish([file.path], file.kind === "removed")}
              onDiscard={() => discard([file.path])}
            />
          ))}
        </ul>
      )}
      {files.length > 0 && (why || all.length > 1) && (
        <div className="flex items-center gap-2 px-3 pt-1">
          <span className="min-w-0 flex-1 text-[11px] text-muted-foreground">{why}</span>
          {all.length > 1 && (
            <button
              type="button"
              className={CARD_BTN_PRIMARY}
              disabled={!target || allBusy}
              onClick={async () => {
                setAllBusy(true);
                try {
                  setAllResult(await publish(all, false));
                } catch (e) {
                  setAllResult({ ok: false, text: plainError(getErrorMessage(e)) });
                } finally {
                  setAllBusy(false);
                }
              }}
            >
              {allBusy ? "Publishing…" : `Publish ${all.length}`}
            </button>
          )}
        </div>
      )}
      {allResult && (
        <p
          className={cn(
            "px-3 pt-0.5 text-[11px]",
            allResult.ok ? "text-muted-foreground" : "text-[var(--cs-git-conflict)]",
          )}
          role={allResult.ok ? "status" : "alert"}
        >
          {allResult.text}
        </p>
      )}
    </section>
  );
}

/**
 * SW-XS-27: the scope row's "N unsaved" counter — team files changed in a
 * workspace and not in the team yet, like unsaved changes. Hidden when there are
 * none. A click opens the list: each workspace's files with View, Publish and
 * Discard (the existing explicit operations), conflicts marked, plain words.
 */
export function UnsavedTeamEditsButton({ teamKey }: { teamKey: string }) {
  useTeamEditsRefresh(teamKey);
  const { status, error } = useTeamEditsStore((s) => selectTeamEdits(s, teamKey));
  const refreshAll = useTeamEditsStore((s) => s.refreshAll);
  const [open, setOpen] = useState(false);
  // "Can't check" can be hidden until what couldn't be checked changes.
  const [hiddenUnsure, setHiddenUnsure] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [viewing, setViewing] = useState<{ ws: TeamFileEditsWorkspace; file: TeamFileEdit } | null>(
    null,
  );
  const anchorRef = useRef<HTMLButtonElement>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const unsureKey = status ? unsureSignature(status) : "";
  if (!status || (!counterVisible(status) && !open)) return null;
  if (status.total === 0 && hiddenUnsure === unsureKey) return null;
  const sentence = counterSentence(status);
  const listed = status.workspaces;
  const conflicts = listed.reduce(
    (n, w) =>
      n +
      (w.state === "missing"
        ? 0
        : (w.files?.filter((file) => file.kind === "conflict").length ?? 0)),
    0,
  );
  return (
    <>
      <Popover
        open={open}
        onOpenChange={(next) => {
          setOpen(next);
          if (next) setNow(Date.now());
        }}
      >
        <PopoverAnchor asChild>
          <button
            ref={anchorRef}
            type="button"
            data-testid="unsaved-team-edits"
            aria-label={sentence}
            aria-haspopup="dialog"
            aria-expanded={open}
            title={sentence}
            onClick={() => {
              setNow(Date.now());
              setOpen((o) => {
                // Opening the list reads again, machine workspaces included
                // (no timer keeps it current).
                if (!o) void refreshAll(teamKey);
                return !o;
              });
            }}
            className={cn(
              "inline-flex h-[22px] flex-none items-center gap-1 whitespace-nowrap rounded-md border pr-[7px] pl-1.5 text-[11.5px] tabular-nums text-foreground",
              "border-[color-mix(in_srgb,var(--foreground)_14%,transparent)] transition-colors hover:bg-[color-mix(in_srgb,var(--foreground)_7%,transparent)]",
              "focus-visible:outline-none focus-visible:ring-[1.5px] focus-visible:ring-ring",
              open && "bg-[color-mix(in_srgb,var(--foreground)_10%,transparent)]",
            )}
          >
            <UnsavedGlyph />
            <span className="font-semibold">
              {status.total > 0 ? (
                <>
                  {status.total}
                  <span className="@max-[12rem]:hidden"> unsaved</span>
                </>
              ) : status.workspaces.some((ws) => ws.state === "missing") ? (
                "Folder gone"
              ) : counterVisible(status) ? (
                "Can't check"
              ) : (
                "No unsaved"
              )}
            </span>
            {conflicts > 0 && (
              <span
                className="inline-flex items-center gap-0.5 font-semibold text-[var(--cs-git-conflict)]"
                data-testid="unsaved-conflicts"
              >
                <ConflictGlyph />
                {conflicts}
              </span>
            )}
          </button>
        </PopoverAnchor>
        <PopoverContent
          side="right"
          align="start"
          sideOffset={10}
          className="max-h-[min(70vh,560px)] w-[380px] gap-0 overflow-y-auto rounded-md border border-border p-0 text-[11.5px] leading-[1.45] shadow-[0_6px_18px_rgba(0,0,0,.2)] ring-0"
          data-testid="unsaved-team-edits-card"
          aria-label="Unsaved team edits"
          onEscapeKeyDown={(e) => {
            // Escape inside a Discard confirm cancels that confirm only.
            const at = document.activeElement;
            if (at instanceof HTMLElement && at.closest('[aria-label="Confirm discard"]'))
              e.preventDefault();
          }}
          onInteractOutside={(e) => {
            if (anchorRef.current?.contains(e.target as Node)) e.preventDefault();
          }}
        >
          <header className="px-3 pt-2.5 pb-2">
            <h2 ref={headingRef} tabIndex={-1} className="text-[12px] font-semibold">
              Unsaved team edits
            </h2>
            <p className="mt-0.5 text-muted-foreground">
              Team files changed in a workspace and not in the team yet. Publish sends a change to
              the team; Discard throws it away.
            </p>
            {status.total === 0 && (
              <div className="mt-1.5 flex items-center gap-2">
                <span className="min-w-0 flex-1 text-muted-foreground">
                  {status.workspaces.some((ws) => ws.state === "missing")
                    ? "A workspace folder is gone (below)."
                    : status.workspaces.every((ws) => ws.state === "checked")
                      ? "No unsaved team edits."
                      : "Nothing unsaved was found, but a workspace couldn’t be checked (below)."}
                </span>
                <button
                  type="button"
                  className={CARD_BTN}
                  onClick={() => {
                    setHiddenUnsure(unsureKey);
                    setOpen(false);
                  }}
                >
                  Hide
                </button>
              </div>
            )}
            {(error || status.remoteError) && (
              <p className="mt-1 text-[var(--cs-git-conflict)]" role="alert">
                {error
                  ? `Couldn't check just now: ${plainError(error)}`
                  : "Workspaces on machines couldn't be listed."}
              </p>
            )}
          </header>
          {listed.map((ws) => (
            <WorkspaceSection
              key={workspaceKey(ws)}
              teamKey={teamKey}
              ws={ws}
              now={now}
              onRemoved={(next) =>
                requestAnimationFrame(() => {
                  (next?.isConnected ? next : (headingRef.current ?? anchorRef.current))?.focus();
                })
              }
              onView={(file) => {
                setOpen(false);
                setViewing({ ws, file });
              }}
            />
          ))}
        </PopoverContent>
      </Popover>
      {viewing && (
        <TeamEditDiffDialog
          key={`${workspaceKey(viewing.ws)}|${viewing.file.path}`}
          workspace={viewing.ws}
          file={viewing.file}
          onClose={() => setViewing(null)}
          // The list closed behind the dialog: focus returns to the counter.
          returnFocusTo={anchorRef}
        />
      )}
    </>
  );
}
