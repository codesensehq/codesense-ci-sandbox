// SPDX-License-Identifier: MIT
import type { ReactNode } from "react";
import {
  Check,
  CircleAlert,
  CloudOff,
  GitBranch,
  LoaderCircle,
  Monitor,
  Server,
  WrapText,
} from "lucide-react";

import { cn } from "@/lib/utils";
import { InfoTip } from "@/components/ui/tooltip";
import { indentationLabel, languageLabel, lineEndingLabel } from "@/shared/components/code-editor";

import type { PaneStatus } from "./worktree-file-pane";

/**
 * The bottom status bar: where the file lives and its save state on the left,
 * the cursor and the file's format on the right — the readouts people glance
 * at in any editor, in the order they expect. Low-priority items drop first as
 * the tile narrows (container queries, not viewport breakpoints: an editor in
 * a quarter-screen split tile is narrow on a wide monitor).
 */
export function EditorStatusBar({
  location,
  machineLabel,
  machineOffline = null,
  branch,
  status,
  wrap,
  onToggleWrap,
  onGoToLine,
}: {
  location: "local" | "remote";
  machineLabel: string | null;
  /** "offline" / "offline since 14:05" while the machine can't be reached. */
  machineOffline?: string | null;
  branch: string | null;
  status: PaneStatus | null;
  wrap: boolean;
  onToggleWrap: () => void;
  onGoToLine: () => void;
}) {
  const editable = status?.kind === "editable";
  return (
    <div
      role="status"
      aria-live="off"
      data-testid="worktree-status-bar"
      className="@container/status flex h-6 min-w-0 shrink-0 items-center gap-0.5 overflow-hidden border-t border-[var(--border)] bg-[var(--card)] px-1.5 text-[0.7rem] text-muted-foreground"
    >
      <Item
        label={
          location !== "remote"
            ? "On this Mac"
            : machineOffline
              ? `Can’t reach ${machineLabel ?? "the machine"} (${machineOffline}) — files can’t be read or saved until it reconnects`
              : `On ${machineLabel ?? "a remote machine"}`
        }
        testId="worktree-status-location"
        className={cn("shrink", machineOffline && "text-[var(--cs-status-modified)]")}
      >
        {location === "remote" ? (
          machineOffline ? (
            <CloudOff aria-hidden className="size-3 shrink-0" />
          ) : (
            <Server aria-hidden className="size-3 shrink-0 text-[var(--accent)]" />
          )
        ) : (
          <Monitor aria-hidden className="size-3 shrink-0" />
        )}
        <span className="min-w-0 max-w-[12rem] truncate">
          {location === "remote" ? (machineLabel ?? "Remote") : "This Mac"}
        </span>
        {location === "remote" && machineOffline && (
          <span className="shrink-0">· {machineOffline}</span>
        )}
      </Item>
      {branch && (
        <Item label={`Branch ${branch}`} className="hidden shrink @[30rem]/status:flex">
          <GitBranch aria-hidden className="size-3 shrink-0" />
          <span className="min-w-0 max-w-[14rem] truncate">{branch}</span>
        </Item>
      )}
      {status && editable && <SaveState status={status} remote={location === "remote"} />}
      {status?.changes && <ChangeSummary changes={status.changes} />}

      <span className="min-w-2 flex-1" />

      {status && editable && status.cursor && (
        <ItemButton label="Go to line (⌃G)" onClick={onGoToLine} testId="worktree-status-cursor">
          <span className="tabular-nums">
            Ln {status.cursor.line}, Col {status.cursor.column}
          </span>
          {status.cursor.selections > 1 ? (
            <span className="tabular-nums">({status.cursor.selections} cursors)</span>
          ) : status.cursor.selectedChars > 0 ? (
            <span className="tabular-nums">({status.cursor.selectedChars} selected)</span>
          ) : null}
        </ItemButton>
      )}
      {status && editable && (
        <>
          <Item
            label="Indentation (detected from the file)"
            className="hidden @[48rem]/status:flex"
          >
            {indentationLabel(status.indentation)}
          </Item>
          <Item label="Encoding" className="hidden @[54rem]/status:flex">
            UTF-8
          </Item>
          <Item
            label="Line endings (kept as the file has them)"
            className="hidden @[54rem]/status:flex"
          >
            {lineEndingLabel(status.lineEnding)}
          </Item>
        </>
      )}
      {status && status.kind !== "idle" && (
        <Item label="Language" className="hidden @[36rem]/status:flex">
          {languageLabel(status.language)}
        </Item>
      )}
      <ItemButton
        label={wrap ? "Word wrap on (⌥Z)" : "Word wrap off (⌥Z)"}
        onClick={onToggleWrap}
        pressed={wrap}
        testId="worktree-status-wrap"
      >
        <WrapText aria-hidden className="size-3" />
        <span className="hidden @[40rem]/status:inline">Wrap</span>
      </ItemButton>
    </div>
  );
}

function SaveState({ status, remote }: { status: PaneStatus; remote: boolean }) {
  if (status.stale) {
    const where = remote ? "on the machine" : "on disk";
    return (
      <Item label={`The file changed ${where} under your edits`} testId="worktree-status-save">
        <CircleAlert aria-hidden className="size-3 text-[var(--cs-status-modified)]" />
        <span className="text-[var(--cs-status-modified)]">Changed {where}</span>
      </Item>
    );
  }
  if (status.saving) {
    return (
      <Item label="Saving" testId="worktree-status-save">
        <LoaderCircle aria-hidden className="size-3 animate-spin" />
        <span>Saving…</span>
      </Item>
    );
  }
  if (status.saveError) {
    return (
      <Item label={status.saveError} testId="worktree-status-save">
        <CircleAlert aria-hidden className="size-3 text-destructive" />
        <span className="text-destructive">Not saved</span>
      </Item>
    );
  }
  if (status.dirty) {
    return (
      <Item label="Unsaved changes — ⌘S to save" testId="worktree-status-save">
        <span aria-hidden className="size-1.5 rounded-full bg-foreground/70" />
        <span>Unsaved</span>
      </Item>
    );
  }
  return (
    <Item label="All changes saved" testId="worktree-status-save">
      <Check aria-hidden className="size-3" />
      <span>Saved</span>
    </Item>
  );
}

function ChangeSummary({ changes }: { changes: NonNullable<PaneStatus["changes"]> }) {
  const total = changes.added + changes.modified + changes.deleted;
  if (total === 0) {
    return (
      <Item label="No changes since the last commit" className="hidden @[44rem]/status:flex">
        <span>Unchanged since commit</span>
      </Item>
    );
  }
  return (
    <Item
      label={`Since the last commit: ${changes.added} added, ${changes.modified} modified, ${changes.deleted} deletions`}
      className="hidden @[44rem]/status:flex"
      testId="worktree-status-changes"
    >
      <span className="tabular-nums text-[var(--cs-status-added)]">+{changes.added}</span>
      <span className="tabular-nums text-[var(--cs-status-modified)]">~{changes.modified}</span>
      <span className="tabular-nums text-[var(--cs-status-deleted)]">−{changes.deleted}</span>
    </Item>
  );
}

const ITEM = "flex h-full min-w-0 shrink-0 items-center gap-1 rounded-sm px-1.5 whitespace-nowrap";

function Item({
  label,
  className,
  testId,
  children,
}: {
  label: string;
  className?: string;
  testId?: string;
  children: ReactNode;
}) {
  return (
    <InfoTip label={label} side="top">
      <span className={cn(ITEM, className)} data-testid={testId} aria-label={label}>
        {children}
      </span>
    </InfoTip>
  );
}

function ItemButton({
  label,
  onClick,
  pressed,
  testId,
  className,
  children,
}: {
  label: string;
  onClick: () => void;
  pressed?: boolean;
  testId?: string;
  className?: string;
  children: ReactNode;
}) {
  return (
    <InfoTip label={label} side="top">
      <button
        type="button"
        onClick={onClick}
        aria-pressed={pressed}
        aria-label={label}
        data-testid={testId}
        className={cn(
          ITEM,
          "hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:[outline-color:var(--ring)]",
          pressed && "text-foreground",
          className,
        )}
      >
        {children}
      </button>
    </InfoTip>
  );
}
