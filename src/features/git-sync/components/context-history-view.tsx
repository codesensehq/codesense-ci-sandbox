// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown } from "lucide-react";

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";
import { AgentFaceView } from "@/features/viewer/components/agent-identity";
import { resolveAgentFace } from "@/features/viewer/lib/agent-list-view";
import { useAvatarPacks } from "@/shared/hooks/use-avatar-packs";
import { FileKindIcon, fileKindOf } from "@/shared/lib/icons/file-kind";
import type { HistoryBranchFilter } from "@/shared/lib/nav-lens";
import { stripViewerExtension } from "@/shared/lib/viewer-file-type";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { ContextRepoStatus, HistoryCommit } from "@/tauri/commands";

import { joinOrgPath } from "../lib/group-changes-by-node";
import {
  aheadBehindLine,
  commitWhen,
  commitWho,
  graphWidth,
  LANE_STROKE,
  layoutHistory,
  refChips,
  type HistoryRow,
  type RefChip,
} from "../lib/history-graph";
import {
  CHANGE_KIND_WORD,
  toNavChanges,
  type ChangeScope,
  type NavChange,
} from "../lib/nav-changes";
import { getPublishSet } from "../lib/publish-set-source";
import { scopePublishSet } from "../lib/scope-publish-set";
import { useContextHistory } from "../lib/use-context-history";
import { useContextGitStore } from "../store/use-context-git-store";

/** Rows tighten to one line with columns once the nav is dragged this wide. */
const WIDE_PX = 640;

const clamp = (v: number, lo: number, hi: number) => Math.round(Math.min(hi, Math.max(lo, v)));

/** The Branches and By columns grow with the nav, so the subject keeps the most room. */
function columnWidths(width: number): { branches: number; by: number } {
  return { branches: clamp(width * 0.22, 130, 260), by: clamp(width * 0.16, 100, 150) };
}
/** A commit's files list this many before "All N files". */
const FILES_SHOWN = 8;
const FILE_ROW_H = 24;
/** Lane 0's centre: room for half a 16px face and its 2px ring inside the nav. */
const LANE_BASE = 11;

const CHANGE_NAME_CLASS: Record<NavChange["kind"], string> = {
  added: "text-[var(--cs-git-added)]",
  modified: "text-[var(--cs-git-modified)]",
  renamed: "text-[var(--cs-git-modified)]",
  deleted: "text-[var(--cs-git-deleted)] line-through decoration-1",
  conflict: "text-[var(--cs-git-conflict)]",
};

/** The nav's width, read as it changes (the History view lays out by it). */
function useWidth(ref: React.RefObject<HTMLElement | null>): number {
  const [w, setW] = useState(272);
  useEffect(() => {
    const el = ref.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver((entries) => {
      for (const e of entries) setW(Math.round(e.contentRect.width));
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [ref]);
  return w;
}

// ---------------------------------------------------------------------------
// Faces
// ---------------------------------------------------------------------------

/** An agent's face, dealt the way the agents list deals it. */
function AgentCommitFace({ name, avatar, size }: { name: string; avatar?: string; size: number }) {
  const { faces } = useAvatarPacks();
  const face = resolveAgentFace(
    { avatar: avatar ?? null, agentRevision: undefined, handle: name, displayName: name },
    { name, faces },
  );
  return <AgentFaceView face={face} size={size} />;
}

/** A person: their initial in a quiet disc. */
function PersonFace({ name, size }: { name: string; size: number }) {
  return (
    <span
      aria-hidden
      className="inline-grid flex-none place-items-center rounded-full bg-[color-mix(in_srgb,var(--foreground)_14%,transparent)] font-semibold leading-none text-foreground"
      style={{ width: size, height: size, fontSize: Math.round(size * 0.55) }}
    >
      {(name[0] ?? "?").toUpperCase()}
    </span>
  );
}

// ---------------------------------------------------------------------------
// The graph cell
// ---------------------------------------------------------------------------

interface GraphGeometry {
  laneW: number;
  maxLanes: number;
  width: number;
}

function laneX(g: GraphGeometry, lane: number): number {
  return LANE_BASE + Math.min(lane, g.maxLanes - 1) * g.laneW;
}

function Segment({
  x1,
  y1,
  x2,
  y2,
  kind,
}: {
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  kind: keyof typeof LANE_STROKE;
}) {
  const st = LANE_STROKE[kind];
  const mid = (y1 + y2) / 2;
  const d = x1 === x2 ? `M${x1} ${y1}V${y2}` : `M${x1} ${y1}C${x1} ${mid} ${x2} ${mid} ${x2} ${y2}`;
  return (
    <path
      d={d}
      fill="none"
      stroke={st.color}
      strokeWidth={st.width}
      strokeDasharray={st.dash || undefined}
      strokeLinecap="round"
    />
  );
}

/** A commit's mark: a square, outlined when GitHub lacks it, a diamond for a merged PR.
 *  An agent's commit wears the agent's face instead (drawn over the cell). */
function CommitMark({ c, x, y }: { c: HistoryCommit; x: number; y: number }) {
  if (c.agent) return null;
  if (c.pr && c.lane === "main")
    return (
      <rect
        x={x - 4}
        y={y - 4}
        width={8}
        height={8}
        rx={1}
        transform={`rotate(45 ${x} ${y})`}
        fill="var(--cs-sidebar-bg, var(--background))"
        stroke="var(--cs-lane-main)"
        strokeWidth={1.75}
      />
    );
  if (c.localOnly)
    return (
      <rect
        x={x - 3.5}
        y={y - 3.5}
        width={7}
        height={7}
        rx={1.5}
        fill="var(--cs-sidebar-bg, var(--background))"
        stroke="var(--cs-lane-main)"
        strokeWidth={1.5}
      />
    );
  return (
    <rect x={x - 3.5} y={y - 3.5} width={7} height={7} rx={1.5} fill={LANE_STROKE[c.lane].color} />
  );
}

function GraphCell({ row, g, h }: { row: HistoryRow; g: GraphGeometry; h: number }) {
  const mid = h / 2;
  const x = laneX(g, row.lane);
  const folded = Math.max(row.before.length, row.after.length) - g.maxLanes + 1;
  return (
    <span className="relative flex-none" style={{ width: g.width }}>
      <svg width={g.width} height={h} aria-hidden className="block">
        {row.before.map((l, j) =>
          l && !row.into.includes(j) ? (
            <Segment key={`p${j}`} x1={laneX(g, j)} y1={0} x2={laneX(g, j)} y2={h} kind={l.kind} />
          ) : null,
        )}
        {row.into.map((j) => (
          <Segment
            key={`i${j}`}
            x1={laneX(g, j)}
            y1={0}
            x2={x}
            y2={mid}
            kind={row.before[j]?.kind ?? "main"}
          />
        ))}
        {row.outs.map((o, k) => (
          <Segment key={`o${k}`} x1={x} y1={mid} x2={laneX(g, o.lane)} y2={h} kind={o.kind} />
        ))}
        {folded > 1 && (
          <text
            x={laneX(g, g.maxLanes - 1) + 5}
            y={mid + 3.5}
            fontSize={9}
            fill="var(--muted-foreground)"
          >
            +{folded}
          </text>
        )}
        <CommitMark c={row.commit} x={x} y={mid} />
      </svg>
      {row.commit.agent && (
        <span
          className="cs-history-face absolute leading-[0]"
          style={{ left: x - 8, top: mid - 8 }}
        >
          <AgentCommitFace
            name={row.commit.agent.name}
            avatar={row.commit.agent.avatar}
            size={16}
          />
        </span>
      )}
    </span>
  );
}

/** The lanes that keep running past an open commit, beside its files. */
function PassCell({ row, g }: { row: HistoryRow; g: GraphGeometry }) {
  return (
    <span className="flex-none" style={{ width: g.width }}>
      <svg width={g.width} height={FILE_ROW_H} aria-hidden className="block">
        {row.after.map((l, j) =>
          l ? (
            <Segment
              key={j}
              x1={laneX(g, j)}
              y1={0}
              x2={laneX(g, j)}
              y2={FILE_ROW_H}
              kind={l.kind}
            />
          ) : null,
        )}
      </svg>
    </span>
  );
}

// ---------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------

const CHIP_CLASS: Record<RefChip["look"], string> = {
  head: "border-foreground bg-foreground font-semibold text-[var(--cs-sidebar-bg,var(--background))]",
  ahead: "border-dashed border-border text-foreground",
  behind: "border-dashed border-border text-foreground",
  pr: "border-dashed border-border text-foreground",
  remote: "border-border text-muted-foreground",
  branch: "border-border text-muted-foreground",
};

function Chip({ chip }: { chip: RefChip }) {
  return (
    <span
      className={cn(
        "inline-block h-[15px] min-w-0 max-w-full flex-shrink truncate rounded-[3px] border px-[5px] text-[10.5px] leading-[13px]",
        CHIP_CLASS[chip.look],
      )}
      data-testid={`history-ref-${chip.look}`}
    >
      {chip.text}
    </span>
  );
}

// ---------------------------------------------------------------------------
// Files under an open commit
// ---------------------------------------------------------------------------

type FilesState =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "ready"; changes: NavChange[] };

function useCommitFiles(
  orgRoot: string | null,
  sha: string | null,
  teamPrefix: string | null,
  scope: ChangeScope | undefined,
): FilesState | null {
  const [state, setState] = useState<{ sha: string; files: FilesState } | null>(null);
  useEffect(() => {
    if (!orgRoot || !sha) return;
    let live = true;
    getPublishSet(orgRoot, undefined, sha)
      .then((set) => {
        if (!live) return;
        const changes = toNavChanges(scopePublishSet(set, teamPrefix), orgRoot);
        setState({ sha, files: { kind: "ready", changes } });
      })
      .catch((e) => {
        if (live) setState({ sha, files: { kind: "error", message: String(e) } });
      });
    return () => {
      live = false;
    };
  }, [orgRoot, sha, teamPrefix]);
  // The agent's context narrows what was read, without reading it again.
  return useMemo(() => {
    if (!sha) return null;
    if (state?.sha !== sha) return { kind: "loading" };
    if (!scope || state.files.kind !== "ready") return state.files;
    const changes = state.files.changes.filter((c) => {
      const rel = scope.relOf(c.absPath);
      return rel !== null && scope.membership(rel).state !== "out";
    });
    return { kind: "ready", changes };
  }, [sha, state, scope]);
}

function fileLabel(change: NavChange): string {
  const name = change.relPath.split("/").pop() ?? change.relPath;
  return stripViewerExtension(name);
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

export interface ContextHistoryViewProps {
  localOnly?: boolean;
  teamKey: string;
  navRepoPath: string;
  orgRoot: string | null;
  /** The team's content, repo-relative: History lists only commits that touched it. */
  teamPrefix: string | null;
  status: ContextRepoStatus | null;
  /** The filter field's text: narrows to commits whose words match. */
  query: string;
  branches: HistoryBranchFilter;
  onBranchesChange: (b: HistoryBranchFilter) => void;
  /** The agent whose context the nav shows: only commits that touched what it gets. */
  agentScope?: ChangeScope & { agentName: string };
}

/** A focusable entry: a commit, or one file under the open commit. */
type Focus = { sha: string; file: string | null };

/**
 * The context repo's history in the left nav: commits newest first on lanes (main in ink,
 * merged branches grey, an open PR dashed, a backup dotted), a filled label on the branch
 * this Mac is on, merged PRs as diamonds with their number, and an agent's commit wearing
 * the agent's face. Opening a commit unfolds its files, tinted by change; a file opens that
 * commit's change in an ordinary tab. Maximized (or dragged wide), each commit is one line
 * with columns. Read-only: the branch filter changes what is drawn, never what is checked out.
 */
export function ContextHistoryView({
  teamKey,
  navRepoPath,
  orgRoot,
  teamPrefix,
  status,
  query,
  branches,
  onBranchesChange,
  agentScope,
  localOnly = false,
}: ContextHistoryViewProps) {
  const rootRef = useRef<HTMLDivElement | null>(null);
  const width = useWidth(rootRef);
  const wide = width >= WIDE_PX;
  const cols = columnWidths(width);
  const paths = useMemo(() => (teamPrefix ? [teamPrefix] : null), [teamPrefix]);
  const historyMoves = useContextGitStore((s) => s.byTeam[teamKey]?.historyMoves ?? 0);
  const { page, loading, loadingMore, error, loadMore } = useContextHistory(
    orgRoot,
    branches,
    paths,
    historyMoves,
  );

  const commits = useMemo(() => {
    const q = query.trim().toLowerCase();
    return page.commits
      .map((c) => (localOnly ? { ...c, localOnly: false } : c))
      .filter((c) => {
        if (agentScope && !c.forkPoint) {
          const touches = (c.files ?? []).some((f) => {
            const rel = orgRoot ? agentScope.relOf(joinOrgPath(orgRoot, f)) : null;
            return rel !== null && agentScope.membership(rel).state !== "out";
          });
          if (!touches) return false;
        }
        if (!q) return true;
        const who = commitWho(c);
        return [c.subject, who.who, who.forWhom ?? "", c.shortSha, c.pr ? `#${c.pr}` : ""].some(
          (s) => s.toLowerCase().includes(q),
        );
      });
  }, [page.commits, query, agentScope, orgRoot, localOnly]);
  const rows = useMemo(() => layoutHistory(commits), [commits]);

  const g: GraphGeometry = useMemo(() => {
    const laneW = wide ? 12 : 10;
    const maxLanes = wide ? 9 : width < 220 ? 2 : width < 300 ? 4 : 6;
    const lanes = Math.min(graphWidth(rows), maxLanes);
    return { laneW, maxLanes, width: LANE_BASE + lanes * laneW };
  }, [rows, wide, width]);

  const [open, setOpen] = useState<string | null>(null);
  const [allFiles, setAllFiles] = useState(false);
  const [focus, setFocus] = useState<Focus | null>(null);
  const files = useCommitFiles(orgRoot, open, teamPrefix, agentScope);
  // The file whose tab is showing this commit's change reads as selected.
  const activePath = useViewerStore((s) => s.activeTab[teamKey]);
  const activeDiff = useViewerStore((s) => (activePath ? s.tabDiff[teamKey]?.[activePath] : null));
  const activeCommit = activeDiff && activeDiff !== true ? activeDiff.sha : null;
  const readyFiles = files?.kind === "ready" ? files.changes : [];
  const shownFiles = allFiles ? readyFiles : readyFiles.slice(0, FILES_SHOWN);
  const [now] = useState(() => Date.now());

  /** In an agent's context, the tier a file reaches the agent from ("team", "repo"). */
  const tierOf = (f: NavChange): string | null => {
    if (!agentScope) return null;
    const rel = agentScope.relOf(f.absPath);
    return rel === null ? null : (agentScope.membership(rel).tier ?? null);
  };

  const toggle = useCallback((sha: string) => {
    setAllFiles(false);
    setOpen((o) => (o === sha ? null : sha));
  }, []);

  const openFile = useCallback(
    (c: HistoryCommit, change: NavChange, pin: boolean) => {
      const who = commitWho(c).who;
      useViewerStore.getState().openCommitChangeTab(
        teamKey,
        navRepoPath,
        change.absPath,
        {
          sha: c.sha,
          shortSha: c.shortSha,
          subject: c.subject,
          pr: c.pr,
          who,
          committedAt: c.committedAt,
          oldPath: change.renamedFrom,
          added: change.added,
          removed: change.removed,
        },
        pin,
      );
    },
    [teamKey, navRepoPath],
  );

  // The keyboard walks what is on screen: commits, and the files of the open one.
  const order = useMemo(() => {
    const out: Focus[] = [];
    for (const r of rows) {
      out.push({ sha: r.commit.sha, file: null });
      if (r.commit.sha === open)
        for (const f of shownFiles) out.push({ sha: open, file: f.absPath });
    }
    return out;
  }, [rows, open, shownFiles]);
  const itemRefs = useRef(new Map<string, HTMLElement>());
  const keyOf = (f: Focus) => `${f.sha}|${f.file ?? ""}`;
  const moveFocus = (f: Focus) => {
    setFocus(f);
    itemRefs.current.get(keyOf(f))?.focus();
  };
  const onKeyDown = (e: React.KeyboardEvent) => {
    const cur = focus ?? order[0];
    if (!cur) return;
    const i = order.findIndex((o) => keyOf(o) === keyOf(cur));
    const commit = commits.find((c) => c.sha === cur.sha);
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        if (i < order.length - 1) moveFocus(order[i + 1]);
        break;
      case "ArrowUp":
        e.preventDefault();
        if (i > 0) moveFocus(order[i - 1]);
        break;
      case "ArrowRight":
        e.preventDefault();
        if (!cur.file && open !== cur.sha) toggle(cur.sha);
        break;
      case "ArrowLeft":
        e.preventDefault();
        if (cur.file) moveFocus({ sha: cur.sha, file: null });
        else if (open === cur.sha) toggle(cur.sha);
        break;
      case "Enter":
        e.preventDefault();
        if (cur.file && commit) {
          const change = readyFiles.find((f) => f.absPath === cur.file);
          if (change) openFile(commit, change, true);
        } else toggle(cur.sha);
        break;
    }
  };

  const filterLabel =
    branches.kind === "all"
      ? "All branches"
      : branches.kind === "main"
        ? "Main only"
        : branches.name;
  const otherBranches = page.branches.filter((b) => b !== "main");
  const synced = otherBranches.filter((b) => b.startsWith("gs/"));
  const named = otherBranches.filter((b) => !b.startsWith("gs/"));
  const openPrOf = (b: string) => page.openPrs.find((p) => p.branch === b);

  const bar = (
    <div
      className="flex flex-shrink-0 items-center gap-1.5 border-b px-2 py-1 text-[11px] text-muted-foreground"
      style={{ borderColor: "var(--cs-sidebar-border, var(--border))" }}
    >
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <button
            type="button"
            className="inline-flex h-5 max-w-[60%] flex-none items-center gap-1 rounded bg-[color-mix(in_srgb,var(--foreground)_6%,transparent)] px-1.5 text-[11.5px] text-foreground hover:bg-[color-mix(in_srgb,var(--foreground)_10%,transparent)] focus-visible:outline focus-visible:outline-1 focus-visible:outline-ring"
            data-testid="history-branch-filter"
            aria-label={`Branches: ${filterLabel}`}
          >
            <span className="truncate">{filterLabel}</span>
            <ChevronDown size={11} aria-hidden className="flex-none" />
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent
          align="start"
          className="w-[210px] rounded-lg border border-border p-1"
          data-testid="history-branch-menu"
        >
          <DropdownMenuLabel className="px-2 pt-1.5 pb-[3px] text-[11px] font-semibold">
            Branches
          </DropdownMenuLabel>
          <BranchOption
            label="Main only"
            on={branches.kind === "main"}
            onSelect={() => onBranchesChange({ kind: "main" })}
          />
          <BranchOption
            label="All branches"
            count={page.branches.length}
            on={branches.kind === "all"}
            onSelect={() => onBranchesChange({ kind: "all" })}
          />
          {otherBranches.length > 0 && (
            <>
              <DropdownMenuSeparator className="mx-0.5 my-1" />
              <DropdownMenuLabel className="px-2 pt-1.5 pb-[3px] text-[11px] font-semibold">
                One branch
              </DropdownMenuLabel>
              {[...synced.filter((b) => openPrOf(b)), ...named].map((b) => {
                const pr = openPrOf(b);
                return (
                  <BranchOption
                    key={b}
                    label={b}
                    suffix={pr ? `#${pr.number}` : undefined}
                    on={branches.kind === "branch" && branches.name === b}
                    onSelect={() => onBranchesChange({ kind: "branch", name: b })}
                  />
                );
              })}
              {synced.filter((b) => !openPrOf(b)).length > 0 && (
                <p className="px-2 py-1 text-[12px] text-muted-foreground">
                  {synced.filter((b) => !openPrOf(b)).length} merged sync branches
                </p>
              )}
            </>
          )}
        </DropdownMenuContent>
      </DropdownMenu>
      <span className="flex-1" />
      <span className="min-w-0 truncate whitespace-nowrap" data-testid="history-ahead-behind">
        {localOnly ? "Private · local only" : aheadBehindLine(status)}
      </span>
    </div>
  );

  const header = wide ? (
    <div
      className="flex flex-shrink-0 items-center gap-2.5 border-b px-2 py-1 text-[11px] font-semibold text-muted-foreground"
      style={{ borderColor: "var(--cs-sidebar-border, var(--border))" }}
      aria-hidden
    >
      <span style={{ width: Math.max(g.width, 40) }}>Graph</span>
      <span className="flex-1">Commit</span>
      <span className="text-right" style={{ width: cols.branches }}>
        Branches
      </span>
      <span style={{ width: cols.by }}>By</span>
      <span className="w-[46px] text-right">When</span>
      <span className="w-[56px]">Hash</span>
    </div>
  ) : null;

  const empty =
    loading && commits.length === 0
      ? "Reading history…"
      : error
        ? `History can't be read: ${error}`
        : query.trim()
          ? `No commit matches "${query.trim()}".`
          : agentScope
            ? `No commit has touched what ${agentScope.agentName} gets.`
            : "No commits here yet.";

  return (
    <div ref={rootRef} className="flex min-h-0 flex-1 flex-col" data-testid="history-view">
      {bar}
      {header}
      {commits.length === 0 ? (
        <p
          className="px-3 pt-16 text-center text-xs text-muted-foreground"
          data-testid="history-empty"
        >
          {empty}
        </p>
      ) : (
        <div
          role="tree"
          aria-label="History"
          className="min-h-0 flex-1 overflow-y-auto pt-0.5 pb-1.5"
          onKeyDown={onKeyDown}
          data-testid="history-list"
        >
          {rows.map((row, idx) => {
            const c = row.commit;
            const isOpen = open === c.sha;
            const isFocus = focus?.sha === c.sha && !focus.file;
            const who = commitWho(c);
            const chips = refChips(c, status, page);
            const when = commitWhen(c.committedAt, now);
            const aria = `${c.pr ? `#${c.pr} ` : ""}${c.subject}, ${who.who}${who.forWhom ? ` for ${who.forWhom}` : ""}, ${when}${c.localOnly ? ", not on GitHub yet" : ""}${chips.length ? `, ${chips.map((x) => x.text).join(", ")}` : ""}, ${c.shortSha}`;
            const h = wide ? 30 : 38;
            const tabbable = focus ? isFocus : idx === 0;
            return (
              <div key={c.sha} role="none">
                <div
                  role="treeitem"
                  aria-expanded={isOpen}
                  aria-selected={isOpen}
                  aria-label={aria}
                  aria-owns={isOpen ? `history-files-${c.shortSha}` : undefined}
                  tabIndex={tabbable ? 0 : -1}
                  ref={(el) => {
                    if (el) itemRefs.current.set(keyOf({ sha: c.sha, file: null }), el);
                    else itemRefs.current.delete(keyOf({ sha: c.sha, file: null }));
                  }}
                  data-testid="history-commit"
                  data-sha={c.sha}
                  data-lane={c.lane}
                  className={cn(
                    "relative flex cursor-default items-stretch pr-2 outline-none select-none",
                    "focus-visible:shadow-[inset_0_0_0_1.5px_var(--ring)]",
                    isOpen ? "cs-history-open cs-sidebar-item-selected" : "cs-sidebar-hover",
                    c.forkPoint && "opacity-65",
                    wide && "items-center gap-2.5",
                  )}
                  style={{ height: h }}
                  onFocus={() => setFocus({ sha: c.sha, file: null })}
                  onClick={() => toggle(c.sha)}
                >
                  <GraphCell row={row} g={g} h={h} />
                  {wide ? (
                    <WideColumns c={c} chips={chips} when={when} who={who} cols={cols} />
                  ) : (
                    <span className="flex min-w-0 flex-1 flex-col justify-center gap-0.5 pl-1">
                      <span className="truncate text-[12.5px] text-foreground">
                        {c.pr && (
                          <span className="mr-[5px] text-[11px] font-semibold tabular-nums text-muted-foreground">
                            #{c.pr}
                          </span>
                        )}
                        {c.subject}
                      </span>
                      <span className="flex min-w-0 items-center gap-1 overflow-hidden whitespace-nowrap text-[11px] text-muted-foreground">
                        <span className="flex-none">
                          <b className="font-medium text-foreground">{who.who}</b>
                          {who.forWhom && ` for ${who.forWhom}`} · {when}
                        </span>
                        {chips.map((chip) => (
                          <Chip key={`${chip.look}:${chip.text}`} chip={chip} />
                        ))}
                      </span>
                    </span>
                  )}
                </div>
                {isOpen && (
                  <div role="group" id={`history-files-${c.shortSha}`}>
                    {files?.kind === "loading" && (
                      <FileLine row={row} g={g}>
                        <span className="text-[11px] text-muted-foreground">Reading files…</span>
                      </FileLine>
                    )}
                    {files?.kind === "error" && (
                      <FileLine row={row} g={g}>
                        <span className="truncate text-[11px] text-muted-foreground">
                          Its files can't be read: {files.message}
                        </span>
                      </FileLine>
                    )}
                    {files?.kind === "ready" && readyFiles.length === 0 && (
                      <FileLine row={row} g={g}>
                        <span className="text-[11px] text-muted-foreground">
                          {agentScope
                            ? `Nothing ${agentScope.agentName} gets changed here.`
                            : "No team file changed here."}
                        </span>
                      </FileLine>
                    )}
                    {shownFiles.map((f) => {
                      const k = keyOf({ sha: c.sha, file: f.absPath });
                      const focused = focus?.file === f.absPath && focus.sha === c.sha;
                      const selectedFile = activePath === f.absPath && activeCommit === c.sha;
                      const lines = [
                        f.added ? `+${f.added}` : null,
                        f.removed ? `−${f.removed}` : null,
                      ]
                        .filter(Boolean)
                        .join(" ");
                      return (
                        <FileLine key={f.absPath} row={row} g={g}>
                          <div
                            role="treeitem"
                            tabIndex={focused ? 0 : -1}
                            aria-label={`${fileLabel(f)}, ${CHANGE_KIND_WORD[f.kind].toLowerCase()}`}
                            ref={(el) => {
                              if (el) itemRefs.current.set(k, el);
                              else itemRefs.current.delete(k);
                            }}
                            data-testid="history-file"
                            aria-selected={selectedFile}
                            className={cn(
                              "flex h-[22px] min-w-0 flex-1 cursor-default items-center gap-1.5 rounded-[5px] pr-1.5 pl-1 outline-none",
                              "focus-visible:shadow-[inset_0_0_0_1.5px_var(--ring)]",
                              selectedFile ? "cs-sidebar-item-selected" : "cs-sidebar-hover",
                            )}
                            onFocus={() => setFocus({ sha: c.sha, file: f.absPath })}
                            onClick={() => openFile(c, f, false)}
                            onDoubleClick={() => openFile(c, f, true)}
                          >
                            <FileKindIcon
                              kind={fileKindOf(f.relPath)}
                              size={13}
                              className="cs-sidebar-icon text-muted-foreground"
                            />
                            <span
                              className={cn(
                                "min-w-0 truncate text-[12.5px]",
                                CHANGE_NAME_CLASS[f.kind],
                              )}
                            >
                              {fileLabel(f)}
                            </span>
                            <span className="flex-1" />
                            {tierOf(f) && (
                              <span className="flex-none text-[11px] text-muted-foreground">
                                {tierOf(f)}
                              </span>
                            )}
                            {lines && (
                              <span className="flex-none text-[11px] tabular-nums text-muted-foreground">
                                {lines}
                              </span>
                            )}
                          </div>
                        </FileLine>
                      );
                    })}
                    {!allFiles && readyFiles.length > FILES_SHOWN && (
                      <FileLine row={row} g={g}>
                        <button
                          type="button"
                          className="rounded px-1 text-[11px] text-muted-foreground hover:text-foreground focus-visible:outline focus-visible:outline-1 focus-visible:outline-ring"
                          data-testid="history-all-files"
                          onClick={() => setAllFiles(true)}
                        >
                          All {readyFiles.length} files
                        </button>
                      </FileLine>
                    )}
                  </div>
                )}
              </div>
            );
          })}
          {page.nextCursor && (
            <button
              type="button"
              className="mx-2 mt-1 rounded px-1 text-[11px] text-muted-foreground hover:text-foreground focus-visible:outline focus-visible:outline-1 focus-visible:outline-ring"
              disabled={loadingMore}
              onClick={loadMore}
              data-testid="history-older"
            >
              {loadingMore ? "Reading older commits…" : "Older commits"}
            </button>
          )}
        </div>
      )}
    </div>
  );
}

function FileLine({
  row,
  g,
  children,
}: {
  row: HistoryRow;
  g: GraphGeometry;
  children: React.ReactNode;
}) {
  return (
    <div className="flex items-center pr-2" style={{ height: FILE_ROW_H }}>
      <PassCell row={row} g={g} />
      <div className="flex min-w-0 flex-1 items-center pl-1">{children}</div>
    </div>
  );
}

function WideColumns({
  c,
  chips,
  when,
  who,
  cols,
}: {
  cols: { branches: number; by: number };
  c: HistoryCommit;
  chips: RefChip[];
  when: string;
  who: { who: string; forWhom: string | null };
}) {
  return (
    <>
      <span className="min-w-0 flex-1 truncate text-[12.5px] text-foreground">
        {c.pr && (
          <span className="mr-[5px] text-[11px] font-semibold tabular-nums text-muted-foreground">
            #{c.pr}
          </span>
        )}
        {c.subject}
      </span>
      <span
        className="flex flex-none items-center justify-end gap-1 overflow-hidden"
        style={{ width: cols.branches }}
      >
        {chips.map((chip) => (
          <Chip key={`${chip.look}:${chip.text}`} chip={chip} />
        ))}
      </span>
      <span
        className="flex flex-none items-center gap-1.5 overflow-hidden whitespace-nowrap text-[11.5px]"
        style={{ width: cols.by }}
      >
        {c.agent ? (
          <AgentCommitFace name={c.agent.name} avatar={c.agent.avatar} size={14} />
        ) : (
          <PersonFace name={who.who} size={14} />
        )}
        <span className="text-foreground">{who.who}</span>
        {who.forWhom && <span className="text-muted-foreground"> for {who.forWhom}</span>}
      </span>
      <span className="w-[46px] flex-none text-right text-[11px] tabular-nums text-muted-foreground">
        {when}
      </span>
      <span className="w-[56px] flex-none font-mono text-[10.5px] text-muted-foreground">
        {c.shortSha}
      </span>
    </>
  );
}

function BranchOption({
  label,
  suffix,
  count,
  on,
  onSelect,
}: {
  label: string;
  suffix?: string;
  count?: number;
  on: boolean;
  onSelect: () => void;
}) {
  return (
    <DropdownMenuItem
      role="menuitemradio"
      aria-checked={on}
      data-on={on}
      className="h-[26px] gap-2 rounded-[5px] px-2 text-[12.5px] data-[on=true]:bg-[color-mix(in_srgb,var(--foreground)_8%,transparent)]"
      onSelect={onSelect}
    >
      <span className="min-w-0 flex-1 truncate">
        {label}
        {suffix && <span className="text-muted-foreground"> · {suffix}</span>}
      </span>
      {count !== undefined && (
        <span className="text-[11.5px] tabular-nums text-muted-foreground">{count}</span>
      )}
    </DropdownMenuItem>
  );
}
