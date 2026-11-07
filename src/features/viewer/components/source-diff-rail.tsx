// SPDX-License-Identifier: MIT
import { useEffect, useRef, type ReactNode, type RefObject } from "react";
import {
  ArrowRight,
  Check,
  ChevronDown,
  ChevronRight,
  FolderTree,
  List,
  ServerCog,
  RotateCw,
  Search,
  Server,
  X,
} from "lucide-react";

import { cn } from "@/lib/utils";
import { InfoTip } from "@/components/ui/tooltip";
import type { PublishFile } from "@/features/git-sync/lib/types";
import {
  BADGE_LABEL,
  RenameChip,
  StatusChip,
} from "@/features/git-sync/components/file-status-chip";
import type { SourceDiffBaseMode } from "@/shared/stores/viewer-store";
import type { DiffRow, DiffTotals } from "../lib/source-diff-model";
import { splitPath } from "../lib/source-diff-model";
import type { RouteState } from "../lib/route-to-context";
import { needsServerUpdate } from "../lib/source-diff-errors";
import { openMachinesPanel } from "@/features/machines/lib/machine-actions";
import { ChromeIconButton } from "./overlay-shell";

/** Per-file "→ context" routing, offered only where the backend can route
 *  (a local worktree with a source repo). */
export interface RailRouting {
  routable: (f: PublishFile) => boolean;
  state: (path: string) => RouteState | undefined;
  onRoute: (f: PublishFile) => void;
  onUnroute: (f: PublishFile) => void;
  onDismiss: (f: PublishFile) => void;
}

export interface SourceDiffRailProps {
  wsId: string;
  files: PublishFile[] | null;
  loading: boolean;
  error: string | null;
  onRetry: () => void;
  /** The machine a remote workspace lives on; `null` for This Mac. */
  machineLabel: string | null;
  rows: DiffRow[];
  totals: DiffTotals;
  query: string;
  onQuery: (q: string) => void;
  filterRef: RefObject<HTMLInputElement | null>;
  treeMode: boolean;
  onToggleTree: () => void;
  onToggleDir: (path: string) => void;
  selectedFile: string | null;
  onSelect: (path: string) => void;
  baseMode: SourceDiffBaseMode;
  baseLabel: string;
  onBaseMode: (mode: SourceDiffBaseMode) => void;
  routing: RailRouting | null;
  /** Toggle the full keyboard map. */
  onShowKeys: () => void;
  keysOpen: boolean;
}

/**
 * Leave the filter without dropping focus on `<body>` (where the tool's keys
 * would stop answering): into the list — the selected row, else the first — or
 * back to the tool itself.
 */
function leaveFilter(input: HTMLInputElement, intoList: boolean) {
  const tool = input.closest<HTMLElement>('[role="dialog"]');
  const row = intoList
    ? (tool?.querySelector<HTMLElement>('[data-source-diff-row][aria-selected="true"]') ??
      tool?.querySelector<HTMLElement>("[data-source-diff-row]"))
    : null;
  const target = row ?? tool;
  if (target) target.focus({ preventScroll: true });
  else input.blur();
}

/**
 * The source diff's left rail: what is being compared (whole branch vs
 * uncommitted), a filter, and the changed files as a tree or flat list with each
 * file's status and line weight. Selection and keyboard navigation are owned by
 * the overlay, which computes `rows` (so j/k walk exactly what is on screen).
 */
export function SourceDiffRail(props: SourceDiffRailProps) {
  const {
    wsId,
    files,
    loading,
    error,
    onRetry,
    machineLabel,
    rows,
    totals,
    query,
    onQuery,
    filterRef,
    treeMode,
    onToggleTree,
    onToggleDir,
    selectedFile,
    onSelect,
    baseMode,
    baseLabel,
    onBaseMode,
    routing,
    onShowKeys,
    keysOpen,
  } = props;
  const count = files?.length ?? 0;
  const filtering = query.trim().length > 0;

  return (
    <aside
      data-testid={`source-diff-rail-${wsId}`}
      className="@container flex w-[clamp(220px,30%,300px)] shrink-0 flex-col border-r border-[var(--border)] bg-[color-mix(in_oklab,var(--muted)_35%,var(--popover))]"
    >
      <div className="flex flex-col gap-2 px-3 pt-2.5 pb-2">
        <div className="flex items-center gap-2">
          <h2 className="text-[13px] font-semibold text-foreground">Changes</h2>
          {count > 0 && (
            <span
              className="rounded-full bg-[color-mix(in_oklab,var(--foreground)_9%,transparent)] px-1.5 text-[11px] font-medium tabular-nums text-muted-foreground"
              aria-label={`${count} changed ${count === 1 ? "file" : "files"}`}
            >
              {count}
            </span>
          )}
          {totals.counted && count > 0 && (
            <LineStat added={totals.added} removed={totals.removed} className="ml-auto" />
          )}
        </div>
        <BaseSwitch wsId={wsId} mode={baseMode} label={baseLabel} onMode={onBaseMode} />
        <div className="flex items-center gap-1">
          <label className="group relative flex min-w-0 flex-1 items-center">
            <Search
              size={12}
              className="pointer-events-none absolute left-2 text-muted-foreground"
              aria-hidden
            />
            <input
              ref={filterRef}
              data-testid={`source-diff-filter-${wsId}`}
              type="search"
              value={query}
              onChange={(e) => onQuery(e.target.value)}
              onKeyDown={(e) => {
                // Keys that confirm or cancel an IME composition belong to it.
                if (e.nativeEvent.isComposing || e.keyCode === 229) return;
                // Escape clears first, then leaves the field (the overlay's own
                // Escape hides the tool only once focus is out of the input).
                if (e.key === "Escape") {
                  e.stopPropagation();
                  if (query) onQuery("");
                  else leaveFilter(e.currentTarget, false);
                }
                if (e.key === "Enter" || e.key === "ArrowDown") {
                  e.preventDefault();
                  leaveFilter(e.currentTarget, true);
                }
              }}
              placeholder="Filter files"
              aria-label="Filter changed files"
              spellCheck={false}
              className="h-7 w-full rounded-md border border-[var(--border)] bg-[var(--popover)] pr-7 pl-6.5 text-xs text-foreground placeholder:text-muted-foreground focus-visible:border-[var(--ring)] focus-visible:outline-none [&::-webkit-search-cancel-button]:hidden"
            />
            {query ? (
              <button
                type="button"
                aria-label="Clear filter"
                onClick={() => onQuery("")}
                className="absolute right-1.5 rounded p-0.5 text-muted-foreground hover:text-foreground"
              >
                <X size={11} aria-hidden />
              </button>
            ) : (
              <kbd className="pointer-events-none absolute right-2 font-sans text-[10px] text-muted-foreground">
                /
              </kbd>
            )}
          </label>
          <ChromeIconButton
            testId={`source-diff-tree-toggle-${wsId}`}
            size="sm"
            ariaLabel={treeMode ? "Show as a flat list" : "Group by folder"}
            tooltip={treeMode ? "Flat list · t" : "Group by folder · t"}
            onClick={onToggleTree}
          >
            {treeMode ? <List size={13} /> : <FolderTree size={13} />}
          </ChromeIconButton>
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-1.5 pb-2" role="presentation">
        {loading && files == null ? (
          // SW-VE-12 (AC-4): one "Loading diff…" state with a spinner — same
          // copy local and remote, so a tab's lazy-load reads the same way
          // whether its working tree lives on This Mac or on a Machine.
          <p
            className="flex items-center gap-1.5 px-2 py-1.5 text-xs text-muted-foreground"
            data-testid="source-diff-loading"
            // SW-VE-12 R3-M6: `role="status"` already implies `aria-live=polite`
            // — the explicit attribute was redundant. One live region keeps
            // the loading→error transition from announcing twice.
            role="status"
          >
            <RotateCw size={12} aria-hidden className="shrink-0 animate-spin" />
            Loading diff…
          </p>
        ) : error && needsServerUpdate(error) ? (
          <RailMessage>
            <span className="flex items-center gap-1.5 font-medium text-foreground">
              <ServerCog size={13} aria-hidden className="shrink-0" />
              Update the terminal server
            </span>
            <span>
              {machineLabel ? `${machineLabel} runs` : "This machine runs"} an older terminal server
              that can’t diff its workspaces. Update it from Machines, then retry.
            </span>
            <span className="mt-1 flex gap-1.5">
              <button
                type="button"
                onClick={() => openMachinesPanel(null)}
                className="inline-flex items-center rounded-md bg-foreground px-2 py-0.5 text-[11px] font-medium text-background hover:bg-foreground/85 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:[outline-color:var(--ring)]"
              >
                Open Machines
              </button>
              <RetryButton onRetry={onRetry} />
            </span>
          </RailMessage>
        ) : error ? (
          // SW-VE-12 (AC-5): remote-specific framing when the fetch runs
          // through a machine (the spec's AC-5 wording); local errors keep
          // the generic "Couldn't load the source diff." so a local disk /
          // worktree failure never reads as a network issue.
          <RailMessage tone="error">
            <span className="font-medium">
              {machineLabel ? "Couldn’t load diff from remote." : "Couldn’t load the source diff."}
            </span>
            <span className="line-clamp-4 break-words text-muted-foreground">{error}</span>
            <span className="mt-1">
              <RetryButton onRetry={onRetry} />
            </span>
          </RailMessage>
        ) : files && files.length === 0 ? (
          // SW-VE-12 (AC-3): spec copy. The old two-line "No source changes
          // / Nothing uncommitted…" has been collapsed to the one line the
          // spec names — a Branch-mode fallthrough still reads naturally.
          <RailMessage>
            <span className="font-medium text-foreground">No uncommitted changes.</span>
            {baseMode !== "head" && (
              <span>This branch matches its base. Edits appear here as they land.</span>
            )}
          </RailMessage>
        ) : filtering && rows.length === 0 ? (
          <RailMessage>
            <span>No files match “{query.trim()}”.</span>
          </RailMessage>
        ) : (
          <ul className="flex flex-col" role="listbox" aria-label="Changed files">
            {rows.map((row) =>
              row.kind === "dir" ? (
                <DirRow key={`d:${row.path}`} row={row} onToggle={() => onToggleDir(row.path)} />
              ) : (
                <FileRow
                  key={`f:${row.file.path}`}
                  row={row}
                  flat={!treeMode}
                  selected={row.file.path === selectedFile}
                  onSelect={() => onSelect(row.file.path)}
                  routing={routing}
                />
              ),
            )}
          </ul>
        )}
      </div>

      <div className="flex items-center gap-2 border-t border-[var(--border)] px-3 py-1.5 text-[10.5px] text-muted-foreground">
        {machineLabel ? (
          <span
            className="flex min-w-0 items-center gap-1"
            data-testid={`source-diff-machine-${wsId}`}
          >
            <Server size={11} aria-hidden className="shrink-0" />
            <span className="truncate">{machineLabel}</span>
          </span>
        ) : (
          <span>This Mac</span>
        )}
        <span className="ml-auto flex shrink-0 items-center gap-1.5">
          <span className="flex items-center gap-1.5 @max-[18rem]:hidden" aria-hidden>
            <Key>j</Key>
            <Key>k</Key>
            <span>files</span>
            <Key>n</Key>
            <Key>p</Key>
            <span>changes</span>
          </span>
          <InfoTip label="Keyboard shortcuts · ?" side="top">
            <button
              type="button"
              aria-label="Keyboard shortcuts"
              aria-expanded={keysOpen}
              onClick={onShowKeys}
              className={cn(
                "inline-flex h-4 min-w-4 items-center justify-center rounded border px-1 font-sans text-[10px] leading-none focus-visible:outline-2 focus-visible:[outline-color:var(--ring)]",
                keysOpen
                  ? "border-foreground/40 bg-foreground/10 text-foreground"
                  : "border-[var(--border)] text-muted-foreground hover:text-foreground",
              )}
            >
              ?
            </button>
          </InfoTip>
        </span>
      </div>
    </aside>
  );
}

function RetryButton({ onRetry }: { onRetry: () => void }) {
  return (
    <button
      type="button"
      onClick={onRetry}
      className="inline-flex items-center gap-1 rounded-md border border-[var(--border)] px-2 py-0.5 text-[11px] font-medium text-foreground hover:bg-muted focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:[outline-color:var(--ring)]"
    >
      <RotateCw size={11} aria-hidden />
      Retry
    </button>
  );
}

function Key({ children }: { children: ReactNode }) {
  return (
    <kbd className="inline-flex h-4 min-w-4 items-center justify-center rounded border border-[var(--border)] px-1 font-sans text-[10px] leading-none text-muted-foreground">
      {children}
    </kbd>
  );
}

function RailMessage({ tone, children }: { tone?: "error"; children: ReactNode }) {
  // SW-VE-12 R3-M6: `role="status"` on the sibling loading element already
  // owns the one polite live region; the error branch no longer upgrades
  // to `role="alert"` (assertive) because loading→error then announces
  // twice. Visual tone (red text) still carries the severity.
  return (
    <div
      className={cn(
        "flex flex-col gap-1 px-2 py-2 text-xs leading-relaxed text-muted-foreground",
        tone === "error" && "text-destructive",
      )}
    >
      {children}
    </div>
  );
}

/** Whole branch (fork point) vs uncommitted (HEAD): the one question a reviewer
 *  asks first, so it sits above the list it scopes. */
function BaseSwitch({
  wsId,
  mode,
  label,
  onMode,
}: {
  wsId: string;
  mode: SourceDiffBaseMode;
  label: string;
  onMode: (mode: SourceDiffBaseMode) => void;
}) {
  const option = (value: SourceDiffBaseMode, text: string, hint: string) => (
    <button
      type="button"
      role="radio"
      aria-checked={mode === value}
      data-testid={`source-diff-base-${value === "head" ? "head" : "branch"}-${wsId}`}
      onClick={() => onMode(value)}
      title={hint}
      className={cn(
        "flex-1 rounded-[5px] px-2 py-1 text-[11.5px] font-medium transition-colors duration-150",
        mode === value
          ? "bg-[var(--popover)] text-foreground shadow-[0_1px_2px_rgb(0_0_0/0.18)]"
          : "text-muted-foreground hover:text-foreground",
      )}
    >
      {text}
    </button>
  );
  return (
    <div className="flex flex-col gap-1">
      <div
        role="radiogroup"
        aria-label="Compare against"
        className="flex rounded-md bg-[color-mix(in_oklab,var(--foreground)_7%,transparent)] p-0.5"
      >
        {option("branchBase", "Branch", "Everything this branch changed since it forked (b)")}
        {option("head", "Uncommitted", "Only edits not yet committed (b)")}
      </div>
      <InfoTip label={`Comparing ${label}`} side="bottom">
        <span
          data-testid={`source-diff-base-toggle-${wsId}`}
          className="truncate px-0.5 text-[11px] text-muted-foreground"
        >
          {label}
        </span>
      </InfoTip>
    </div>
  );
}

function DirRow({
  row,
  onToggle,
}: {
  row: Extract<DiffRow, { kind: "dir" }>;
  onToggle: () => void;
}) {
  return (
    <li>
      <button
        type="button"
        tabIndex={-1}
        aria-expanded={!row.collapsed}
        onClick={onToggle}
        style={{ paddingLeft: 6 + row.depth * 12 }}
        className="flex w-full items-center gap-1 rounded-md py-[3px] pr-2 text-left text-muted-foreground hover:bg-[color-mix(in_oklab,var(--foreground)_5%,transparent)] hover:text-foreground"
      >
        {row.collapsed ? (
          <ChevronRight size={12} aria-hidden className="shrink-0" />
        ) : (
          <ChevronDown size={12} aria-hidden className="shrink-0" />
        )}
        <span className="min-w-0 flex-1 truncate font-mono text-[11.5px]" title={row.path}>
          {row.name}
        </span>
        {row.collapsed && (
          <span className="shrink-0 text-[10.5px] tabular-nums">{row.fileCount}</span>
        )}
      </button>
    </li>
  );
}

function FileRow({
  row,
  flat,
  selected,
  onSelect,
  routing,
}: {
  row: Extract<DiffRow, { kind: "file" }>;
  flat: boolean;
  selected: boolean;
  onSelect: () => void;
  routing: RailRouting | null;
}) {
  const { file } = row;
  const renamed = file.renamedFrom != null;
  const { dir, name } = splitPath(file.path);
  const extraBadges = (file.badges ?? []).filter((b) => b !== "rename");
  const routeState = routing?.state(file.path);
  // Keyboard stepping (j/k) can move the selection past the list's edge — keep
  // the selected row on screen.
  const rowRef = useRef<HTMLButtonElement | null>(null);
  useEffect(() => {
    if (selected) rowRef.current?.scrollIntoView?.({ block: "nearest" });
  }, [selected]);
  return (
    <li className="group relative flex items-center" data-path={file.path}>
      <button
        ref={rowRef}
        type="button"
        role="option"
        data-source-diff-row
        aria-selected={selected}
        onClick={onSelect}
        title={renamed ? `${file.renamedFrom} → ${file.path}` : file.path}
        style={{ paddingLeft: flat ? 6 : 6 + row.depth * 12 + 12 }}
        className={cn(
          "flex min-w-0 flex-1 items-center gap-2 rounded-md py-[3px] pr-2 text-left",
          "focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:[outline-color:var(--ring)]",
          selected
            ? "bg-[color-mix(in_oklab,var(--foreground)_11%,transparent)] text-foreground"
            : "text-[color-mix(in_oklab,var(--foreground)_82%,transparent)] hover:bg-[color-mix(in_oklab,var(--foreground)_5%,transparent)]",
        )}
      >
        {renamed ? <RenameChip /> : <StatusChip status={file.status} />}
        <span
          className={cn(
            "min-w-0 flex-1 truncate font-mono text-[12px]",
            file.status === "deleted" &&
              "line-through decoration-[color-mix(in_oklab,currentColor_45%,transparent)]",
            selected && "font-medium",
          )}
        >
          {flat && dir ? <span className="text-muted-foreground">{dir}</span> : null}
          {flat ? name : row.name}
        </span>
        {extraBadges.map((b) => (
          <span
            key={b}
            className="shrink-0 rounded bg-muted px-1 text-[10px] font-medium text-muted-foreground"
          >
            {BADGE_LABEL[b]}
          </span>
        ))}
        <LineStat added={file.added} removed={file.removed} />
      </button>
      {routing && (
        <RouteAffordance
          routable={routing.routable(file)}
          state={routeState}
          onRoute={() => routing.onRoute(file)}
          onUnroute={() => routing.onUnroute(file)}
          onDismiss={() => routing.onDismiss(file)}
        />
      )}
    </li>
  );
}

/** +added / −removed, green / red, tabular; nothing when the file has no count
 *  (binary, or a synthesized add/delete). Never color alone: the signs carry it. */
export function LineStat({
  added,
  removed,
  className,
}: {
  added?: number;
  removed?: number;
  className?: string;
}) {
  if (added == null && removed == null) return null;
  return (
    <span
      className={cn(
        "flex shrink-0 items-center gap-1 font-mono text-[10.5px] tabular-nums",
        className,
      )}
      aria-label={`${added ?? 0} added, ${removed ?? 0} removed`}
    >
      <span className="text-[var(--cs-status-added)]">+{added ?? 0}</span>
      <span className="text-[var(--cs-status-deleted)]">−{removed ?? 0}</span>
    </span>
  );
}

/**
 * The per-row "→ context" action and its optimistic / undo / outcome states.
 * Quiet until the row is hovered, focused or selected — a review list reads
 * cleaner without an action label on every line — but always keyboard
 * reachable, and any in-flight or outcome state stays visible.
 */
function RouteAffordance({
  routable,
  state,
  onRoute,
  onUnroute,
  onDismiss,
}: {
  routable: boolean;
  state: RouteState | undefined;
  onRoute: () => void;
  onUnroute: () => void;
  onDismiss: () => void;
}) {
  const wrap = (children: ReactNode, quiet = false) => (
    <span
      data-route-action
      className={cn(
        "flex shrink-0 items-center gap-1 pr-1 text-[10.5px]",
        quiet &&
          "absolute right-1 rounded-md bg-[var(--popover)] opacity-0 shadow-[0_1px_3px_rgb(0_0_0/0.2)] transition-opacity duration-150 group-hover:opacity-100 focus-within:opacity-100",
      )}
    >
      {children}
    </span>
  );

  const dismissableNotice = (label: string, className: string, reason?: string) =>
    wrap(
      <InfoTip label={reason}>
        <span className="flex items-center gap-1">
          <span className={cn("px-1", className)}>{label}</span>
          <button
            type="button"
            data-route-dismiss
            onClick={onDismiss}
            aria-label="Dismiss"
            className="rounded p-0.5 text-muted-foreground hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <X size={10} aria-hidden />
          </button>
        </span>
      </InfoTip>,
    );

  if (state?.kind === "routing") {
    return wrap(<span className="px-1 text-muted-foreground">routing…</span>);
  }
  if (state?.kind === "routed") {
    return wrap(
      <>
        <Check size={11} className="text-muted-foreground" aria-hidden />
        <span className="text-muted-foreground">routed</span>
        <button
          type="button"
          data-route-undo
          onClick={onUnroute}
          className="rounded px-1 font-medium text-muted-foreground underline underline-offset-2 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          Undo
        </button>
      </>,
    );
  }
  if (state?.kind === "collision") {
    // Route collisions are not filed into the Conflicts resolver, so the copy
    // must not send the user there. Nothing was clobbered.
    return dismissableNotice(
      "already in context (differs)",
      "text-amber-600 dark:text-amber-500",
      "A different version is already in the team context repo. Nothing was clobbered — commit or reconcile the file, then dismiss and retry.",
    );
  }
  if (state?.kind === "alreadyTracked") {
    return dismissableNotice("already tracked", "text-muted-foreground");
  }
  if (state?.kind === "blocked") {
    return dismissableNotice("can’t route", "text-muted-foreground", state.reason);
  }
  if (state?.kind === "error") {
    return wrap(
      <InfoTip label={state.message}>
        <button
          type="button"
          onClick={onRoute}
          className="rounded px-1 font-medium text-destructive hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          route failed — retry
        </button>
      </InfoTip>,
    );
  }
  if (!routable) return null;
  return wrap(
    <InfoTip label="Send this file to the team context repo">
      <button
        type="button"
        data-route-to-context
        onClick={onRoute}
        className="flex items-center gap-0.5 rounded px-1 py-0.5 font-medium text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        <ArrowRight size={11} aria-hidden />
        context
      </button>
    </InfoTip>,
    true,
  );
}
