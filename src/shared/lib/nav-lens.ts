// SPDX-License-Identifier: MIT
import type { StatusRole } from "@/tauri/commands";
import type { StatusMarkKind } from "./status-mark";

/** Which stories the left nav shows: all of them, or one slice. */
export type NavLens =
  | "everything"
  | "needs"
  | "agents"
  | "ready"
  | "active"
  | "review"
  | "blocked"
  | "not_started"
  | "done";

/** How the left nav lays out what it shows: the folder tree, or a flat list by stage. */
export type NavArrange = "folders" | "stage";

/** What the left nav lists: every file, only the files GitHub doesn't have yet, or the
 *  context repo's commits. */
export type NavView = "files" | "changes" | "history";

export type NavSource = { kind: "clone" } | { kind: "private" } | { kind: "worktree"; id: string };

/** Which branches History draws: every one, the main line, or one branch by name. View-only:
 *  it never checks anything out. */
export type HistoryBranchFilter =
  | { kind: "all" }
  | { kind: "main" }
  | { kind: "branch"; name: string };

/** One team's left-nav view: files or changes, its lens, its arrangement, and whether files
 *  are hidden. */
export interface NavLensState {
  source: NavSource;
  view: NavView;
  lens: NavLens;
  arrange: NavArrange;
  /** Hide every row that is not a story (epics stay as the frame). */
  storiesOnly: boolean;
  /** Arranged by stage, the Done group is collapsed until opened. */
  doneOpen: boolean;
  /** History's branch filter. */
  historyBranches: HistoryBranchFilter;
}

export const DEFAULT_NAV_LENS: NavLensState = {
  source: { kind: "clone" },
  view: "files",
  lens: "everything",
  arrange: "folders",
  storiesOnly: false,
  doneOpen: false,
  historyBranches: { kind: "all" },
};

export function normalizeNavLens(saved?: Partial<NavLensState>): NavLensState {
  return { ...DEFAULT_NAV_LENS, ...saved, source: saved?.source ?? DEFAULT_NAV_LENS.source };
}

/** The lenses in menu order, each with its words. */
export const NAV_LENSES: ReadonlyArray<{ lens: NavLens; label: string }> = [
  { lens: "everything", label: "Everything" },
  { lens: "needs", label: "Needs you" },
  { lens: "agents", label: "Agents on it" },
  { lens: "ready", label: "Ready to start" },
  { lens: "active", label: "In progress" },
  { lens: "review", label: "In review" },
  { lens: "blocked", label: "Blocked" },
  { lens: "not_started", label: "Not started" },
  { lens: "done", label: "Done" },
];

/** The stage lenses, by the taxonomy role they select. */
export const LENS_ROLE: Readonly<Partial<Record<NavLens, StatusRole>>> = {
  active: "active",
  review: "review",
  blocked: "blocked",
  not_started: "not_started",
  done: "done",
};

/** What a lens reads of one story: its stage's role, its agent mark (the pill's), and
 *  whether it is ready to start (not started, every dependency done). */
export interface LensStory {
  role: StatusRole | undefined;
  mark: StatusMarkKind | undefined;
  ready?: boolean;
}

/**
 * Whether a story belongs in a lens. "Needs you" is a story whose mark is the amber
 * Needs you mark (a question or an approval waits), exactly what its pill and row show;
 * "Agents on it" is any story whose workspace still runs an agent (a session that ended or
 * crashed, a plain shell's exit included, is no agent on it); "Ready to start" is a not-started
 * story whose dependencies are all done; the stage lenses select by role.
 */
export function inLens(lens: NavLens, story: LensStory): boolean {
  switch (lens) {
    case "everything":
      return true;
    case "needs":
      return isNeedsMark(story.mark);
    case "agents":
      return story.mark !== undefined && story.mark !== "ended" && story.mark !== "crashed";
    case "ready":
      return story.ready === true && story.role === "not_started";
    default:
      return story.role === LENS_ROLE[lens];
  }
}

/** How many stories each lens holds. */
export function countLenses(stories: Iterable<LensStory>): Record<NavLens, number> {
  const counts = Object.fromEntries(NAV_LENSES.map(({ lens }) => [lens, 0])) as Record<
    NavLens,
    number
  >;
  for (const s of stories) for (const { lens } of NAV_LENSES) if (inLens(lens, s)) counts[lens]++;
  return counts;
}

/** The stage groups, in the order arranged-by-stage lists them: Needs you first, Done last. */
export const STAGE_GROUPS: ReadonlyArray<{
  key: "needs" | StatusRole | "off";
  label: string;
}> = [
  { key: "needs", label: "Needs you" },
  { key: "active", label: "In progress" },
  { key: "review", label: "In review" },
  { key: "blocked", label: "Blocked" },
  { key: "not_started", label: "Not started" },
  { key: "off", label: "No stage" },
  { key: "done", label: "Done" },
];

/** Any of the three needs-you marks (question / permission / blocked). The lens
 *  rule reads them as one bucket — the row's own mark still shows which. */
function isNeedsMark(mark: StatusMarkKind | undefined): boolean {
  return mark === "question" || mark === "permission" || mark === "blocked";
}

/** The stage group a story is listed under: Needs you wins over its stage. */
export function stageGroupOf(story: LensStory): (typeof STAGE_GROUPS)[number]["key"] {
  if (isNeedsMark(story.mark)) return "needs";
  return story.role ?? "off";
}

/** MCP writes only the source and view, through the same setter as the View menu. */
export function mcpNavLens(value: unknown): Partial<NavLensState> | null {
  if (!value || typeof value !== "object") return null;
  const raw = value as Record<string, unknown>;
  const patch: Partial<NavLensState> = {};
  if (typeof raw.source === "string") {
    patch.source =
      raw.source === "clone" || raw.source === "private"
        ? { kind: raw.source }
        : { kind: "worktree", id: raw.source };
  }
  if (raw.view === "files" || raw.view === "changes" || raw.view === "history")
    patch.view = raw.view;
  else if (patch.source?.kind === "worktree") patch.view = "changes";
  return Object.keys(patch).length ? patch : null;
}
