// SPDX-License-Identifier: MIT
/**
 * A team's landing page: its home page, opened whenever the team has nothing
 * open, so the user lands on something to read instead of an empty viewer.
 *
 * - A team may declare its page in `teams.yml` (`landing_page:`, relative to the
 *   team's content root). The bundled CodeSense Docs team declares `README.html`.
 * - Otherwise, by convention: `README.md`, `README.html`, then `index.html` at the
 *   content root (in any letter case). Nothing else — never a random file.
 * - The backend resolves it (`resolve_team_landing_page`): only a regular file
 *   whose real path (symlinks resolved) is inside the content folder qualifies,
 *   since the app opens it without a click.
 * - When: entering a team (switch, launch, restart restore) with no open tabs,
 *   and closing the last tab while in the team. Saved tabs always win.
 * - Dismissing it: when the landing page itself is among the tabs just closed,
 *   the team stays empty until it is next entered, so the user can still close it.
 * - Never: before the saved viewer state is really restored, in console layout
 *   (opening a file would switch the layout), or into a team the user has left.
 * - A content folder that is missing or empty (not cloned or synced yet) opens
 *   nothing; the next entry tries again.
 */
import { pathToTeamContentDirSync } from "@/shared/lib/team-content-path";
import { logger } from "@/shared/lib/logger";
import { buildTeamKey } from "@/shared/lib/team-key";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore, type ViewerState } from "@/shared/stores/viewer-store";
import type { Org, Team } from "@/shared/types/domain";
import {
  resolveTeamLandingPage,
  type LandingCandidate,
  type LandingPageResolution,
} from "@/tauri/commands";

/** Conventional landing pages, in preference order, at the team's content root. */
export const LANDING_PAGE_FALLBACKS = ["README.md", "README.html", "index.html"] as const;

type Resolve = (
  contentRoot: string,
  candidates: LandingCandidate[],
) => Promise<LandingPageResolution>;

/** A declared landing page stays inside the team's content and is not hidden:
 *  relative, no `..`, no segment starting with `.` (`.env`, `.claude/…`).
 *  Mirrors Rust `is_content_relative`; the backend checks again, through symlinks. */
export function isContentRelative(path: string): boolean {
  if (!path || path.startsWith("/") || path.includes("\\")) return false;
  return path.split("/").every((seg) => seg !== "" && !seg.startsWith("."));
}

/** `path` relative to the team's content folder, or `null` when it is not in
 *  it. Doubled and trailing slashes are ignored, so two spellings of one file
 *  compare equal; anything else outside the folder is `null`. */
export function contentRelativePath(path: string, contentRoot: string): string | null {
  const norm = (p: string) => p.replace(/\/{2,}/g, "/").replace(/\/+$/, "");
  const file = norm(path);
  const root = norm(contentRoot);
  return file.startsWith(`${root}/`) ? file.slice(root.length + 1) || null : null;
}

/** The pages to try, in order: the declared one (when safe, exact name), then
 *  the fallbacks (any letter case). */
export function landingPageCandidates(team: Pick<Team, "landing_page">): LandingCandidate[] {
  const declared = team.landing_page?.trim();
  const out: LandingCandidate[] = [];
  if (declared && isContentRelative(declared)) out.push({ path: declared, anyCase: false });
  for (const path of LANDING_PAGE_FALLBACKS) {
    if (!out.some((c) => c.path === path)) out.push({ path, anyCase: true });
  }
  return out;
}

/** The team's content folder: `{org_root}/{team}/content`. */
export function teamContentRoot(orgRoot: string, teamName: string): string {
  return pathToTeamContentDirSync(orgRoot.replace(/\/+$/, ""), teamName);
}

/** Nothing open in the team: no tabs and no active tab. */
export function isTeamEmpty(
  s: Pick<ViewerState, "openTabs" | "activeTab">,
  teamKey: string,
): boolean {
  return !(s.openTabs[teamKey]?.length ?? 0) && !s.activeTab[teamKey];
}

// ---------------------------------------------------------------------------
// Memo: closing the last tab must not probe the disk every time. A settled
// answer ("found" / "none") is kept per content root + candidates until the
// content changes (`invalidateLandingPages`), `teams.yml` changes the declared
// page (a different key), or the team is entered again (`fresh`: a team that
// was not active may have changed unwatched). "unavailable" and errors are
// never kept.
// ---------------------------------------------------------------------------

interface MemoEntry {
  contentRoot: string;
  result: Promise<LandingPageResolution>;
  /** The answer once it has settled, for a synchronous read. */
  settled?: LandingPageResolution;
}

const memo = new Map<string, MemoEntry>();
/** Content roots whose page is shown somewhere (the nav marker, the menus): a
 *  change under one notifies even when nothing is memoised for it yet (its
 *  content was not there at the last lookup). */
const shownRoots = new Set<string>();

/** Bumped whenever resolved pages are forgotten, so a view showing the starter
 *  page (the nav marker) can look it up again. */
let memoVersion = 0;
const memoListeners = new Set<() => void>();

export function subscribeLandingPages(listener: () => void): () => void {
  memoListeners.add(listener);
  return () => memoListeners.delete(listener);
}

export function landingPagesVersion(): number {
  return memoVersion;
}

function resolveMemoised(
  contentRoot: string,
  candidates: LandingCandidate[],
  resolve: Resolve,
  fresh: boolean,
): Promise<LandingPageResolution> {
  const key = `${contentRoot}\u0000${JSON.stringify(candidates)}`;
  if (fresh) memo.delete(key);
  const hit = memo.get(key);
  if (hit) return hit.result;
  const result = resolve(contentRoot, candidates).then(asLandingPageResolution);
  const entry: MemoEntry = { contentRoot, result };
  memo.set(key, entry);
  result.then(
    (r) => {
      if (r.kind === "unavailable" && memo.get(key)?.result === result) memo.delete(key);
      else entry.settled = r;
    },
    () => {
      if (memo.get(key)?.result === result) memo.delete(key);
    },
  );
  return result;
}

/** The resolver's answer as a `LandingPageResolution`: anything missing or
 *  malformed (a backend or IPC mock without the command answers `undefined`)
 *  counts as "unavailable" (a malformed one is logged) — nothing opens and nothing is memoised — instead of
 *  throwing on `.kind`. */
export function asLandingPageResolution(raw: unknown): LandingPageResolution {
  if (raw && typeof raw === "object") {
    const r = raw as { kind?: unknown; path?: unknown };
    if (r.kind === "found" && typeof r.path === "string" && r.path) {
      return { kind: "found", path: r.path };
    }
    if (r.kind === "none") return { kind: "none" };
    if (r.kind === "unavailable") return { kind: "unavailable" };
  }
  // No answer at all (a backend or mock without the command) stays quiet; an
  // answer of the wrong shape is a contract drift worth seeing.
  if (raw !== undefined && raw !== null) {
    logger.warn("viewer", `team landing page: unexpected resolver answer ${JSON.stringify(raw)}`);
  }
  return { kind: "unavailable" };
}

/** Forget resolved landing pages: those under `changedPath`'s team content, or
 *  all of them when no path is given. */
export function invalidateLandingPages(changedPath?: string): void {
  const under = (root: string) =>
    !changedPath || changedPath === root || changedPath.startsWith(`${root}/`);
  let dropped = [...shownRoots].some(under);
  for (const [key, { contentRoot }] of memo) {
    if (under(contentRoot)) {
      memo.delete(key);
      dropped = true;
    }
  }
  if (dropped) {
    memoVersion += 1;
    for (const listener of memoListeners) listener();
  }
}

/** The page `team` opens when nothing is open (memoised like the auto-open), for
 *  showing it — never opening it. */
export function lookupTeamLandingPage(
  org: Pick<Org, "org_root">,
  team: Pick<Team, "name" | "landing_page">,
  resolve: Resolve = resolveTeamLandingPage,
): Promise<LandingPageResolution> {
  const contentRoot = teamContentRoot(org.org_root, team.name);
  shownRoots.add(contentRoot);
  return resolveMemoised(contentRoot, landingPageCandidates(team), resolve, false);
}

/** The memoised, settled page for `team`, read synchronously; `undefined` when
 *  it has not been looked up or is still being looked up. */
export function peekTeamLandingPage(
  org: Pick<Org, "org_root">,
  team: Pick<Team, "name" | "landing_page">,
): LandingPageResolution | undefined {
  const contentRoot = teamContentRoot(org.org_root, team.name);
  return memo.get(`${contentRoot}\u0000${JSON.stringify(landingPageCandidates(team))}`)?.settled;
}

// ---------------------------------------------------------------------------

export type LandingOutcome =
  /** The landing page was opened. */
  | "opened"
  /** The team has something open (saved tabs, or the user opened one meanwhile). */
  | "occupied"
  /** The team has no landing page: nothing opened. */
  | "none"
  /** The landing page was among the tabs just closed: left empty on purpose. */
  | "dismissed"
  /** Not now: saved state not restored yet, a lookup already running, console
   *  layout, the team is no longer the active one, or its content isn't there. */
  | "skipped";

const inFlight = new Set<string>();
/** Tabs closed (emptying the team) while its lookup was already running: the
 *  running lookup honours them as dismissals too. */
const closedDuringLookup = new Map<string, Set<string>>();

function isActiveTeam(orgSlug: string, teamName: string): boolean {
  const { activeOrgSlug, activeTeamName, orgs } = useOrgStore.getState();
  return (
    activeOrgSlug === orgSlug &&
    activeTeamName === teamName &&
    !!orgs.find((o) => o.slug === orgSlug)?.teams.some((t) => t.name === teamName)
  );
}

/** Opening a file reveals the main pane, which would switch a console layout
 *  to shared: an unprompted open must never change the layout. */
function inConsoleLayout(s: Pick<ViewerState, "layoutMode">): boolean {
  return s.layoutMode === "console";
}

/**
 * Open `team`'s landing page when the team has nothing open. `justClosed` lists
 * the tabs whose closing emptied the team: when the landing page is one of them
 * the team stays empty (the user dismissed it).
 */
export async function openTeamLandingPage(input: {
  org: Pick<Org, "slug" | "org_root">;
  team: Pick<Team, "name" | "landing_page">;
  justClosed?: readonly string[];
  /** Entering the team: resolve afresh instead of trusting the memo. */
  fresh?: boolean;
  resolve?: Resolve;
}): Promise<LandingOutcome> {
  const { org, team, justClosed = [], fresh = false, resolve = resolveTeamLandingPage } = input;
  const teamKey = buildTeamKey(org.slug, team.name);
  const before = useViewerStore.getState();
  // Saved tabs come back with `loadPersistedState`; deciding before it finishes
  // would open the landing page over them (the boot watchdog can force
  // `stateLoaded` early, never `persistedStateRestored`).
  if (!before.persistedStateRestored) return "skipped";
  if (inFlight.has(teamKey)) {
    const pending = closedDuringLookup.get(teamKey) ?? new Set<string>();
    for (const path of justClosed) pending.add(path);
    closedDuringLookup.set(teamKey, pending);
    return "skipped";
  }
  if (!isTeamEmpty(before, teamKey)) return "occupied";
  if (inConsoleLayout(before)) return "skipped";
  inFlight.add(teamKey);
  try {
    let result: LandingPageResolution;
    try {
      result = await resolveMemoised(
        teamContentRoot(org.org_root, team.name),
        landingPageCandidates(team),
        resolve,
        fresh,
      );
    } catch (e) {
      logger.warn("viewer", `team landing page for ${teamKey} failed: ${e}`);
      return "skipped";
    }
    if (result.kind === "unavailable") return "skipped";
    // The user switched team (or the team went away) while we looked: never
    // open a page into a team they have left.
    if (!isActiveTeam(org.slug, team.name)) return "skipped";
    const now = useViewerStore.getState();
    // The user opened something while we looked: theirs wins.
    if (!isTeamEmpty(now, teamKey)) return "occupied";
    if (inConsoleLayout(now)) return "skipped";
    if (result.kind === "none") return "none";
    // Compare by content-relative path: a tab may spell the content root
    // differently from the resolver (doubled or trailing slashes).
    const contentRoot = teamContentRoot(org.org_root, team.name);
    const rel = (p: string) => contentRelativePath(p, contentRoot) ?? p;
    const page = rel(result.path);
    const closed = [...justClosed, ...(closedDuringLookup.get(teamKey) ?? [])];
    if (closed.some((p) => rel(p) === page)) return "dismissed";
    // repoPath "" on purpose: no nav drill or pipeline-scope change — the page
    // opens as a tab and leaves the user's navigation where it was.
    now.openFile(teamKey, "", result.path, { keepLayout: true });
    logger.debug("viewer", `opened team landing page for ${teamKey}: ${result.path}`);
    return "opened";
  } finally {
    inFlight.delete(teamKey);
    closedDuringLookup.delete(teamKey);
  }
}
