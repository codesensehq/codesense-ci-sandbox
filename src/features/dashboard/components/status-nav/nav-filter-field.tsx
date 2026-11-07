// SPDX-License-Identifier: MIT
import { useActiveTeamKey } from "@/shared/lib/team-key";
import { useNavSourceCounts } from "@/features/git-sync/lib/use-nav-source-counts";
import { Lock, GitBranch } from "lucide-react";
import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
import { ListFilter, Search, X } from "lucide-react";

import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";
import { StageRailShape } from "@/shared/components/stage-rail";
import { StatusMark } from "@/shared/components/status-mark";
import { FileKindIcon } from "@/shared/lib/icons/file-kind";
import { LENS_ROLE, NAV_LENSES, type NavLens, type NavLensState } from "@/shared/lib/nav-lens";

/** The icon a lens is drawn with: the same mark or rail its stories carry. */
export function LensIcon({ lens }: { lens: NavLens }) {
  if (lens === "everything")
    return <FileKindIcon kind="folder" className="text-muted-foreground" />;
  // A mark drawn as a label holds still: motion is for a live agent, not a menu option.
  if (lens === "needs" || lens === "agents")
    return (
      <span className="cs-mark-still inline-flex">
        <StatusMark mark={lens === "needs" ? "question" : "tool"} size={8} />
      </span>
    );
  if (lens === "ready") return <StageRailShape role="not_started" ready />;
  return <StageRailShape role={LENS_ROLE[lens]} />;
}

/** The "arranged by stage" glyph: a grouped list. */
export function GroupedIcon() {
  return (
    <svg
      width={13}
      height={13}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.5}
      strokeLinecap="round"
      aria-hidden
    >
      <path d="M2.5 3.5h11M4.5 6.5h9M4.5 9.5h9M2.5 12.5h11" />
    </svg>
  );
}

/** The Changes view's glyph: a list whose last line is a change. */
export function ChangesIcon() {
  return (
    <svg
      width={13}
      height={13}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.5}
      strokeLinecap="round"
      aria-hidden
    >
      <path d="M2.5 3.5h11M2.5 7h11M2.5 10.5h5.5" />
      <circle cx="11.75" cy="11.25" r="1.9" />
    </svg>
  );
}

/** The History view's glyph: a clock turning back. */
export function HistoryIcon() {
  return (
    <svg
      width={13}
      height={13}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.5}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <path d="M2.6 8a5.4 5.4 0 1 0 1.6-3.8" />
      <path d="M2.4 2.6v2.6H5" />
      <path d="M8 5.2V8l2 1.4" />
    </svg>
  );
}

/** The native filter menu surface, exposed so a pane host can open the same choices. */
export function NavLensMenuContent({
  children,
  inputRef,
}: {
  children: React.ReactNode;
  inputRef: React.RefObject<HTMLInputElement | null>;
}) {
  return (
    <DropdownMenuContent
      align="end"
      className="w-[206px] rounded-lg border border-border p-1 shadow-[0_8px_24px_rgba(0,0,0,.18),0_2px_6px_rgba(0,0,0,.08)]"
      data-testid="left-nav-lens-menu"
      onCloseAutoFocus={(e) => {
        e.preventDefault();
        inputRef.current?.focus();
      }}
    >
      {children}
    </DropdownMenuContent>
  );
}

/**
 * The filter menu's choices (Source, View, Show stories, Arrange, Stories only). One body for
 * the field's own button and for the nav owner's host, which opens it for a pane's button.
 */
export function NavFilterMenu({ inputRef, ...items }: NavFilterMenuProps) {
  return (
    <NavLensMenuContent inputRef={inputRef}>
      <NavFilterMenuItems {...items} />
    </NavLensMenuContent>
  );
}

type NavFilterMenuProps = {
  inputRef: React.RefObject<HTMLInputElement | null>;
  view: NavLensState;
  onViewChange: (patch: Partial<NavLensState>) => void;
  counts: Record<NavLens, number>;
  changesCount: number;
  repoPath?: string;
  cloneCount?: number;
  sources?: { id: string; name: string; branch?: string | null; count?: number }[];
};

// Its own component so the source counts are read only while the menu is open.
function NavFilterMenuItems({
  view,
  onViewChange,
  counts,
  changesCount,
  repoPath = "",
  cloneCount,
  sources = [],
}: Omit<NavFilterMenuProps, "inputRef">) {
  const teamKey = useActiveTeamKey();
  const sourceCounts = useNavSourceCounts(true, repoPath, teamKey);
  const hasProfile = useLocalProfileStore((s) => !!s.status?.active);
  const source = view.source ?? { kind: "clone" };
  const sourceChip = source.kind !== "clone";
  const filesView = view.view === "files";
  return (
    <>
      {(hasProfile || sourceChip || sources.length > 0) && (
        <>
          <DropdownMenuLabel className="px-2 pt-1.5 pb-[3px] text-[11px] font-semibold">
            Source
          </DropdownMenuLabel>
          {(
            [
              { kind: "clone", label: "main · your clone", icon: <GitBranch size={12} /> },
              { kind: "private", label: "Private", icon: <Lock size={12} /> },
            ] as const
          )
            .filter((row) => row.kind !== "private" || hasProfile || source.kind === "private")
            .map((row) => (
              <DropdownMenuItem
                key={row.kind}
                role="menuitemradio"
                aria-checked={source.kind === row.kind}
                data-on={source.kind === row.kind}
                className={OPTION_CLASS}
                onSelect={() => onViewChange({ source: { kind: row.kind } })}
              >
                <span className="grid w-[18px] place-items-center text-muted-foreground">
                  {row.icon}
                </span>
                <span className="flex-1">{row.label}</span>
                <span className="text-[11.5px] tabular-nums text-muted-foreground">
                  {row.kind === "private"
                    ? sourceCounts?.private
                    : (sourceCounts?.clone ??
                      cloneCount ??
                      (source.kind === "clone" ? changesCount : ""))}
                </span>
              </DropdownMenuItem>
            ))}
          {sources.length > 0 && (
            <div className="max-h-[156px] overflow-y-auto">
              {sources.map((row) => {
                const chosen = source.kind === "worktree" && source.id === row.id;
                return (
                  <DropdownMenuItem
                    key={row.id}
                    role="menuitemradio"
                    aria-checked={chosen}
                    data-on={chosen}
                    textValue={row.name}
                    className={OPTION_CLASS}
                    onSelect={() =>
                      onViewChange({
                        source: { kind: "worktree", id: row.id },
                        view: "changes",
                      })
                    }
                  >
                    <span className="grid w-[18px] place-items-center text-muted-foreground">
                      <GitBranch size={12} />
                    </span>
                    <span className="min-w-0 flex-1 truncate" title={row.branch ?? undefined}>
                      {row.name}
                      {row.branch && (
                        <span className="ml-1 text-[11px] text-muted-foreground">{row.branch}</span>
                      )}
                    </span>
                    {row.count !== undefined && (
                      <span className="text-[11.5px] tabular-nums text-muted-foreground">
                        {row.count}
                      </span>
                    )}
                  </DropdownMenuItem>
                );
              })}
            </div>
          )}
          <DropdownMenuSeparator className="mx-0.5 my-1" />
        </>
      )}
      <DropdownMenuLabel className="px-2 pt-1.5 pb-[3px] text-[11px] font-semibold">
        View
      </DropdownMenuLabel>
      {(
        [
          { v: "files", label: "Files", icon: <FileKindIcon kind="folder" /> },
          { v: "changes", label: "Changes", icon: <ChangesIcon /> },
          { v: "history", label: "History", icon: <HistoryIcon /> },
        ] as const
      ).map(({ v, label, icon }) => (
        <DropdownMenuItem
          key={v}
          role="menuitemradio"
          aria-checked={view.view === v}
          className={OPTION_CLASS}
          data-on={view.view === v}
          data-testid={`left-nav-view-${v}`}
          onSelect={() => onViewChange({ view: v })}
        >
          <span className="grid w-[18px] place-items-center text-muted-foreground">{icon}</span>
          <span className="flex-1">{label}</span>
          {v === "changes" && (
            <span className="text-[11.5px] tabular-nums text-muted-foreground">{changesCount}</span>
          )}
        </DropdownMenuItem>
      ))}
      {filesView && <DropdownMenuSeparator className="mx-0.5 my-1" />}
      {filesView && (
        <DropdownMenuLabel className="px-2 pt-1.5 pb-[3px] text-[11px] font-semibold">
          Show stories
        </DropdownMenuLabel>
      )}
      {filesView &&
        NAV_LENSES.map(({ lens: l, label }) => (
          <DropdownMenuItem
            key={l}
            role="menuitemradio"
            aria-checked={view.lens === l}
            className={OPTION_CLASS}
            data-on={view.lens === l}
            data-testid={`left-nav-lens-${l}`}
            onSelect={() => onViewChange({ lens: l })}
          >
            <span className="grid w-[18px] place-items-center">
              <LensIcon lens={l} />
            </span>
            <span className="flex-1">{label}</span>
            {l !== "everything" && (
              <span className="text-[11.5px] tabular-nums text-muted-foreground">{counts[l]}</span>
            )}
          </DropdownMenuItem>
        ))}
      {filesView && (
        <>
          <DropdownMenuSeparator className="mx-0.5 my-1" />
          <DropdownMenuLabel className="px-2 pt-1.5 pb-[3px] text-[11px] font-semibold">
            Arrange
          </DropdownMenuLabel>
          <div
            role="group"
            aria-label="Arrange"
            className="mx-1.5 mt-0.5 mb-1 flex gap-0.5 rounded-md bg-[color-mix(in_srgb,var(--foreground)_5%,transparent)] p-0.5"
          >
            {(["folders", "stage"] as const).map((a) => (
              <DropdownMenuItem
                key={a}
                role="menuitemradio"
                aria-checked={view.arrange === a}
                data-testid={`left-nav-arrange-${a}`}
                className="h-5 flex-1 justify-center rounded px-0 text-[11.5px] text-muted-foreground focus:bg-[color-mix(in_srgb,var(--foreground)_9%,transparent)] aria-checked:bg-background aria-checked:text-foreground aria-checked:shadow-[0_1px_2px_rgba(0,0,0,.12)]"
                onSelect={(e) => {
                  e.preventDefault();
                  onViewChange({ arrange: a });
                }}
              >
                {a === "folders" ? "Folders" : "Stage"}
              </DropdownMenuItem>
            ))}
          </div>
          <DropdownMenuCheckboxItem
            checked={view.storiesOnly}
            data-testid="left-nav-stories-only"
            className="h-6 rounded-[5px] text-[12.5px] text-muted-foreground data-[state=checked]:text-foreground"
            onSelect={(e) => {
              e.preventDefault();
              onViewChange({ storiesOnly: !view.storiesOnly });
            }}
          >
            Stories only
          </DropdownMenuCheckboxItem>
        </>
      )}
    </>
  );
}

/** A filter shown as a chip inside the field: it combines with the typed text. */
function Chip({
  icon,
  label,
  count,
  onClear,
  testId,
}: {
  icon: React.ReactNode;
  label: string;
  count?: number;
  onClear: () => void;
  testId: string;
}) {
  return (
    <span
      className="inline-flex h-[18px] flex-none items-center gap-[5px] whitespace-nowrap rounded bg-[color-mix(in_srgb,var(--foreground)_10%,transparent)] pl-1.5 pr-[3px] text-[11.5px] font-medium text-foreground"
      data-testid={testId}
    >
      <span className="grid min-w-3.5 place-items-center">{icon}</span>
      {label}
      {count !== undefined && (
        <span className="font-normal tabular-nums text-muted-foreground">{count}</span>
      )}
      <button
        type="button"
        aria-label={`Clear ${label}`}
        className="grid h-3.5 w-3.5 place-items-center rounded-sm text-muted-foreground hover:text-foreground focus-visible:outline focus-visible:outline-1 focus-visible:outline-ring"
        onClick={(e) => {
          e.preventDefault();
          onClear();
        }}
      >
        <X size={10} strokeWidth={2} aria-hidden />
      </button>
    </span>
  );
}

// The menu's own rule sizes every bare svg to 16px; the marks, rails and drawings here keep
// their drawn size.
const OPTION_CLASS =
  "[&_svg:not([class*='size-'])]:size-auto h-[26px] gap-2 rounded-[5px] px-2 text-[12.5px] focus:bg-[color-mix(in_srgb,var(--foreground)_8%,transparent)] focus:text-foreground data-[on=true]:bg-[color-mix(in_srgb,var(--foreground)_8%,transparent)]";

/**
 * The left nav's filter field: typed text, the chosen view and lens as chips beside it, an
 * amber count of stories that need you, and the lens menu at the field's end (View: Files,
 * Changes or History; Show stories, with live counts; Arrange by folders or stage; Stories only). Esc
 * clears the text, then a chip.
 */
export function NavFilterField({
  query,
  onQueryChange,
  inputRef,
  view,
  onViewChange,
  counts,
  changesCount,
  repoPath = "",
  cloneCount,
  sources = [],
  onSourcesOpen,
}: {
  repoPath?: string;
  query: string;
  onQueryChange: (q: string) => void;
  inputRef: React.RefObject<HTMLInputElement | null>;
  view: NavLensState;
  onViewChange: (patch: Partial<NavLensState>) => void;
  counts: Record<NavLens, number>;
  /** Files GitHub doesn't have yet, in this scope. */
  changesCount: number;
  cloneCount?: number;
  sources?: { id: string; name: string; branch?: string | null; count?: number }[];
  onSourcesOpen?: () => void;
}) {
  const source = view.source ?? { kind: "clone" };
  const sourceChip = source.kind !== "clone";
  const worktree =
    source.kind === "worktree" ? sources.find((row) => row.id === source.id) : undefined;
  const lens = NAV_LENSES.find((l) => l.lens === view.lens)!;
  const changesView = view.view === "changes";
  const historyView = view.view === "history";
  // The story lens and the stage arrangement shape the Files view; Changes lists files and
  // History lists commits.
  const filesView = view.view === "files";
  const lensChip = filesView && view.lens !== "everything";
  const stageChip = filesView && view.arrange === "stage";
  const anyChip = sourceChip || changesView || historyView || lensChip || stageChip;
  const needs = counts.needs;
  const pressOnField = (e: React.MouseEvent<HTMLDivElement>): boolean => {
    const target = e.target as HTMLElement;
    return (
      e.currentTarget.contains(target) && target !== inputRef.current && !target.closest("button")
    );
  };
  const clearOne = (): boolean => {
    if (changesView || historyView) onViewChange({ view: "files" });
    else if (lensChip) onViewChange({ lens: "everything" });
    else if (stageChip) onViewChange({ arrange: "folders" });
    else return false;
    return true;
  };

  return (
    // A div, not a label: a label hands any click on it to its first labelable child, which
    // with a chip showing is that chip's Clear button. A press anywhere but a button or the
    // input focuses the input instead. The lens menu is portalled but its React events still
    // bubble here, so only presses inside the field's own DOM count.
    <div
      onMouseDown={(e) => {
        if (!pressOnField(e)) return;
        e.preventDefault();
        inputRef.current?.focus();
      }}
      onClick={(e) => {
        if (pressOnField(e)) inputRef.current?.focus();
      }}
      className="flex h-6 min-w-0 flex-1 cursor-text items-center gap-1.5 rounded-md bg-[color-mix(in_srgb,var(--foreground)_4%,transparent)] pl-1.5 pr-1 text-muted-foreground transition-colors hover:bg-[color-mix(in_srgb,var(--foreground)_6%,transparent)] focus-within:bg-[color-mix(in_srgb,var(--foreground)_7%,transparent)] focus-within:text-foreground focus-within:ring-1 focus-within:ring-ring"
      data-testid="left-nav-filter-field"
    >
      <Search size={12} className="flex-shrink-0" aria-hidden />
      {sourceChip && (
        <Chip
          icon={
            source.kind === "private" ? (
              <Lock size={11} aria-hidden />
            ) : (
              <GitBranch size={11} aria-hidden />
            )
          }
          label={
            source.kind === "private"
              ? "Private"
              : (worktree?.name ?? (source.kind === "worktree" ? source.id : ""))
          }
          onClear={() => onViewChange({ source: { kind: "clone" } })}
          testId="left-nav-source-chip"
        />
      )}
      {changesView && (
        <Chip
          icon={<ChangesIcon />}
          label="Changes"
          count={changesCount}
          onClear={() => onViewChange({ view: "files" })}
          testId="left-nav-changes-chip"
        />
      )}
      {historyView && (
        <Chip
          icon={<HistoryIcon />}
          label="History"
          onClear={() => onViewChange({ view: "files" })}
          testId="left-nav-history-chip"
        />
      )}
      {lensChip && (
        <Chip
          icon={<LensIcon lens={view.lens} />}
          label={lens.label}
          count={counts[view.lens]}
          onClear={() => onViewChange({ lens: "everything" })}
          testId="left-nav-lens-chip"
        />
      )}
      {stageChip && (
        <Chip
          icon={<GroupedIcon />}
          label="By stage"
          onClear={() => onViewChange({ arrange: "folders" })}
          testId="left-nav-stage-chip"
        />
      )}
      <span className="relative flex min-w-0 flex-1 items-center">
        <input
          ref={inputRef}
          type="text"
          value={query}
          onChange={(e) => onQueryChange(e.target.value)}
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === "Escape") {
              e.preventDefault();
              if (query) onQueryChange("");
              else if (!clearOne()) e.currentTarget.blur();
            } else if (e.key === "Backspace" && !query) {
              clearOne();
            }
          }}
          aria-label="Filter files in the nav"
          data-testid="left-nav-search-input"
          className="w-full min-w-0 bg-transparent text-xs text-foreground outline-none"
        />
        {!query && !anyChip && (
          <span
            aria-hidden
            className="pointer-events-none absolute inset-y-0 left-0 flex items-center text-xs text-muted-foreground"
          >
            <span className="@max-[13rem]:hidden">Filter files</span>
            <span className="hidden @max-[13rem]:inline">Filter</span>
          </span>
        )}
      </span>
      {query && !anyChip && (
        <button
          type="button"
          onClick={(e) => {
            e.preventDefault();
            onQueryChange("");
          }}
          aria-label="Clear nav filter"
          className="flex-shrink-0 rounded-sm text-muted-foreground hover:text-foreground focus-visible:outline focus-visible:outline-1 focus-visible:outline-ring"
        >
          <X size={12} />
        </button>
      )}
      {needs > 0 && view.lens !== "needs" && (
        <button
          type="button"
          className="inline-flex h-[18px] flex-none items-center gap-1 rounded-full bg-[color-mix(in_srgb,var(--cs-status-needs)_13%,transparent)] pl-1 pr-[5px] text-[11px] font-semibold tabular-nums text-[var(--cs-status-needs)] focus-visible:outline focus-visible:outline-1 focus-visible:outline-ring"
          title={`${needs} ${needs === 1 ? "story needs" : "stories need"} you`}
          aria-label={`${needs} ${needs === 1 ? "story needs" : "stories need"} you: show ${needs === 1 ? "it" : "them"}`}
          data-testid="left-nav-needs-badge"
          onClick={(e) => {
            e.preventDefault();
            onViewChange({ lens: "needs" });
          }}
        >
          <StatusMark mark="question" size={6} />
          {needs}
        </button>
      )}
      <DropdownMenu
        onOpenChange={(open) => {
          if (open) onSourcesOpen?.();
        }}
      >
        <DropdownMenuTrigger asChild>
          <button
            type="button"
            aria-label="View, show and arrange"
            title="View, show and arrange"
            data-testid="left-nav-lens-button"
            className={cn(
              "grid h-5 w-5 flex-none place-items-center rounded text-muted-foreground hover:text-foreground focus-visible:outline focus-visible:outline-1 focus-visible:outline-ring data-[state=open]:bg-[color-mix(in_srgb,var(--foreground)_9%,transparent)] data-[state=open]:text-foreground",
              anyChip && "text-foreground",
            )}
            onClick={(e) => e.stopPropagation()}
          >
            <ListFilter size={13} aria-hidden />
          </button>
        </DropdownMenuTrigger>
        <NavFilterMenu
          inputRef={inputRef}
          view={view}
          onViewChange={onViewChange}
          counts={counts}
          changesCount={changesCount}
          repoPath={repoPath}
          cloneCount={cloneCount}
          sources={sources}
        />
      </DropdownMenu>
    </div>
  );
}
