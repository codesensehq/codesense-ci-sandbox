// SPDX-License-Identifier: MIT
import { useViewerStore } from "@/shared/stores/viewer-store";
import { customRailKey, customGroupId, type WorkspaceGroup } from "./workspace-groups";
import type { SoftClosedWorkspace, Workspace } from "@/shared/stores/viewer-store";
import { getKindCapabilities } from "@/shared/lib/workspace-kind-capabilities";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";
import { prettifyDirLeaf } from "@/shared/lib/worktree/resolve";

/** DFS the tree, mapping each story `filePath` → its enclosing epic `dirName`. */
function buildStoryEpicIndex(tree: DerivedPipelineTree, out: Map<string, string>): void {
  for (const story of tree.stories) {
    out.set(story.filePath, tree.dirName);
  }
  for (const sub of tree.subEpics) {
    buildStoryEpicIndex(sub, out);
  }
}

/**
 * SW-EW-9: map each epic `dirName` → its main-doc absolute `filePath`.
 *
 * The integration `Workspace` record carries `epicDirPath` (the dir leaf) but
 * NOT the epic doc path, so the panel resolves the doc live from the tree —
 * the SAME source the nav's "Open epic terminal" uses (`mainDoc.filePath`).
 * Resolving live (vs. persisting a path on the record) keeps the doc location
 * authoritative even after the epic moves/renames. Only dirs with a `mainDoc`
 * are indexed; the last occurrence of a dirName wins (dirNames are unique in a
 * well-formed tree).
 */
export function buildEpicDocPathIndex(tree: DerivedPipelineTree | null): Map<string, string> {
  const out = new Map<string, string>();
  const walk = (node: DerivedPipelineTree): void => {
    if (node.dirName && node.mainDoc) out.set(node.dirName, node.mainDoc.filePath);
    for (const sub of node.subEpics) walk(sub);
  };
  if (tree) walk(tree);
  return out;
}

/** SW-EW-33 — one tray-chip entry: the workspace record, whether it's currently
 *  minimized (soft-closed) vs on-screen, and the epic `dirName` it belongs to
 *  (the identity-color key for the chip's epic spine; "" for roots / ungrouped). */
export interface TrayWorkspaceEntry {
  workspace: Workspace;
  minimized: boolean;
  epicDirName: string;
  customGroupId?: string;
}

/** Stable per-workspace ordering key. SW-EW-35: the tray sorts by workspace id
 *  ALONE — never by open-vs-minimized state — so a chip keeps its slot (and its
 *  `Cmd+1..9` index) when it flips open↔minimized on activate/replace. Ids are
 *  immutable, so this is fully churn-free. */
function byWorkspaceId(a: Workspace, b: Workspace): number {
  return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
}

/** DFS the tree assigning each epic `dirName` a monotonic ordinal in pipeline
 *  order — the order the left nav shows epics. SW-EW-35: lets the tray order its
 *  epic GROUPS by pipeline identity (stable across open↔minimized AND across
 *  add/remove of sibling workspaces) instead of by whichever child id is
 *  smallest. dirNames absent from the tree fall to the end (Infinity). */
function buildEpicOrderIndex(tree: DerivedPipelineTree | null): Map<string, number> {
  const order = new Map<string, number>();
  if (!tree) return order;
  let next = 0;
  const walk = (n: DerivedPipelineTree): void => {
    if (n.dirName && !order.has(n.dirName)) order.set(n.dirName, next++);
    for (const sub of n.subEpics) walk(sub);
  };
  walk(tree);
  return order;
}

/**
 * SW-EW-33 — the SINGLE source of truth for the bottom tray's chip order, used
 * by BOTH the rendered chip array and the `Cmd+1..9` positional index so they
 * can never diverge. Spans the union of open + minimized workspaces:
 *  - `kind:"epic"` is excluded (it's never a foreground tile — matches
 *    `foregroundTilesFor` — so it can't be focused/replaced).
 *  - roots come FIRST (D9), then epic groups; within each group, integrations
 *    then stories.
 *
 * SW-EW-35: ordering is by a STABLE key on every axis, NOT by open-vs-minimized
 * state — a chip never moves when it's activated, restored, or replaced (the old
 * open-before-minimized partition over a mutating array reshuffled the tray on
 * every activate). WITHIN a group: workspace id (immutable). The epic GROUPS
 * themselves order by PIPELINE-TREE position (`buildEpicOrderIndex`), so the
 * group order matches the left nav and is stable across add/remove too — not
 * keyed on whichever child id is smallest. Pure — no store reads.
 */
export function trayWorkspaceOrder(
  openWorkspaces: readonly Workspace[],
  softClosed: Readonly<Record<string, SoftClosedWorkspace>>,
  tree: DerivedPipelineTree | null,
  customGroups: readonly WorkspaceGroup[] = [],
): TrayWorkspaceEntry[] {
  const minimizedWs = Object.values(softClosed).map((s) => s.workspace);
  const minimizedIds = new Set(minimizedWs.map((w) => w.id));
  // id-sort the union up front → each group's stories/integrations arrive
  // id-sorted, so the within-group order is stable across open↔minimized.
  const union = [...openWorkspaces, ...minimizedWs]
    .filter((w) => getKindCapabilities(w.kind).foregroundTile)
    .sort(byWorkspaceId);

  const entry = (w: Workspace, epicDirName: string): TrayWorkspaceEntry => ({
    workspace: w,
    minimized: minimizedIds.has(w.id),
    epicDirName,
  });

  // SW-GS-2a: tray placement is driven by the registry `trayGroup` capability.
  // `root` first (its own ROOTS rail, SW-EW-38), then `epic-member` (story, epic)
  // grouped under its epic dir, then `ungrouped` (generic) trailing — matching
  // SW-EW-38's roots-first → epics → Ungrouped-last rail order.
  const out: TrayWorkspaceEntry[] = union
    .filter((w) => getKindCapabilities(w.kind).trayGroup === "root")
    .map((w) => entry(w, ""));

  const storyEpics = new Map<string, string>();
  if (tree) buildStoryEpicIndex(tree, storyEpics);
  const epicOf = (w: Workspace) =>
    w.kind === "epic" ? (w.epicDirPath ?? "") : (storyEpics.get(w.storyFilePath ?? "") ?? "");
  const customMembers = new Set<string>();
  for (const group of customGroups) {
    for (const w of union) {
      if (w.kind === "root" || !group.workspaceIds.includes(w.id) || customMembers.has(w.id))
        continue;
      customMembers.add(w.id);
      out.push({ ...entry(w, epicOf(w)), customGroupId: group.id });
    }
  }
  const epicOrder = buildEpicOrderIndex(tree);
  const remaining = union.filter((w) => w.kind !== "root" && !customMembers.has(w.id));
  remaining.sort((a, b) => {
    const ak = epicOf(a),
      bk = epicOf(b);
    if (ak !== bk) {
      if (!ak) return 1;
      if (!bk) return -1;
      const rank = (epicOrder.get(ak) ?? Infinity) - (epicOrder.get(bk) ?? Infinity);
      if (Number.isFinite(rank) && rank !== 0) return rank;
      if (rank === Infinity || rank === -Infinity) return rank;
      return ak.localeCompare(bk);
    }
    if (a.kind === "epic" && b.kind !== "epic") return -1;
    if (b.kind === "epic" && a.kind !== "epic") return 1;
    return byWorkspaceId(a, b);
  });
  for (const w of remaining) out.push(entry(w, epicOf(w)));

  return out;
}

/** Derive both cached grouping views in one pass over the tray's canonical entries. */
export function buildWorkspaceGrouping(
  open: readonly Workspace[],
  minimized: Readonly<Record<string, SoftClosedWorkspace>>,
  tree: DerivedPipelineTree | null,
  groups: readonly WorkspaceGroup[] = [],
): { railKeyIndex: Record<string, string>; baseOrder: string[] } {
  const entries = trayWorkspaceOrder(open, minimized, tree, groups);
  return {
    railKeyIndex: Object.fromEntries(
      entries.map((entry) => [entry.workspace.id, railKeyOf(entry)]),
    ),
    baseOrder: entries.map((entry) => entry.workspace.id),
  };
}

/**
 * SW-EW-38 — reserved rail key for the roots pill. Roots carry `epicDirName: ""`
 * just like the Ungrouped bucket, so a discriminator is needed to tell the two
 * apart in BOTH the rail render AND the selection key. A leading space can never
 * be a real pipeline-tree `dirName`, so this sentinel can't collide. Exported so
 * the store/tray share ONE definition.
 */
export const ROOTS_RAIL_KEY = " roots";

/**
 * SW-EW-38 — the one discriminator that maps a tray entry to its rail pill.
 * Roots → `ROOTS_RAIL_KEY`; everything else → its `epicDirName` (real epic
 * `dirName`, or `""` for the Ungrouped bucket). This is the key stored in
 * `selectedEpicDirName` and compared for the selected ring + follow-focus, so
 * roots-`""` and Ungrouped-`""` never merge or shadow each other (R2).
 */
export function railKeyOf(entry: TrayWorkspaceEntry): string {
  return entry.workspace.kind === "root"
    ? ROOTS_RAIL_KEY
    : entry.customGroupId
      ? customRailKey(entry.customGroupId)
      : entry.epicDirName;
}

/** SW-EW-38 — one epic-rail pill: its key, display title, member chips (in the
 *  source `trayWorkspaceOrder` order), and whether any member is on-screen
 *  (vs. all-minimized → dimmed pill). */
export interface EpicRailEntry {
  railKey: string;
  title: string;
  members: TrayWorkspaceEntry[];
  hasOpen: boolean;
}

export interface RailInfo {
  title: string;
  kind: "epic" | "roots" | "ungrouped" | "custom";
}

/** Shared group identity for tray and pane. Team is reserved for team-owned groups. */
export function railInfo(
  teamKey: string,
  railKey: string,
  groups?: readonly WorkspaceGroup[],
): RailInfo {
  const id = customGroupId(railKey);
  if (id !== null)
    return {
      title:
        (groups ?? useViewerStore.getState().customGroupsByTeam[teamKey] ?? []).find(
          (g) => g.id === id,
        )?.name ?? "Custom group",
      kind: "custom",
    };
  if (railKey === ROOTS_RAIL_KEY) return { title: "Repositories", kind: "roots" };
  if (railKey === "") return { title: "Ungrouped", kind: "ungrouped" };
  return { title: prettifyDirLeaf(railKey) || railKey, kind: "epic" };
}

/** Display title for a rail pill from its key + members. Roots → the single
 *  root's repo slug (or `"~"`); Ungrouped → `"Ungrouped"`; epic → prettified
 *  `dirName`. Pure — safe in a render path. */
function railTitle(
  railKey: string,
  members: TrayWorkspaceEntry[],
  teamKey: string,
  groups?: readonly WorkspaceGroup[],
): string {
  if (railKey === ROOTS_RAIL_KEY) {
    return members.length === 1 ? (members[0].workspace.sourceRepoSlug ?? "~") : "~";
  }
  return railInfo(teamKey, railKey, groups).title;
}

/** Rail keys in first-encounter order, the roots pill always first: a person's order can put a
 *  story before a root (a flat-mode drag), and that moves the root's pill within the strip, never
 *  the ROOTS rail pill off the rail's head. */
function rootsFirst(order: string[]): string[] {
  const at = order.indexOf(ROOTS_RAIL_KEY);
  return at <= 0 ? order : [ROOTS_RAIL_KEY, ...order.slice(0, at), ...order.slice(at + 1)];
}

/**
 * SW-EW-38 (R1) — derive the epic rail from the SINGLE ordered tray array (the
 * computed `trayWorkspaceOrder` with the person's order applied). A
 * first-encounter reduction over it yields the rail with no second grouping
 * pass, ROOTS kept first — so the rail order, the row-2 slice, and the
 * published `Cmd` order all come from ONE source and cannot diverge. Each
 * epic's pills keep the order they come in. In grouped mode the strip shows only
 * the selected epic's pills, so a drag or Alt+arrows there reorders among that
 * epic's members only; moving a pill to another epic's place is done in flat
 * mode or the Agents pane. Pure — no store reads.
 */
export function epicRailEntries(
  entries: readonly TrayWorkspaceEntry[],
  teamKey = "",
  groups?: readonly WorkspaceGroup[],
): EpicRailEntry[] {
  const byKey = new Map<string, TrayWorkspaceEntry[]>();
  const order: string[] = [];
  for (const e of entries) {
    const key = railKeyOf(e);
    let members = byKey.get(key);
    if (!members) {
      members = [];
      byKey.set(key, members);
      order.push(key);
    }
    members.push(e);
  }
  return rootsFirst(order).map((railKey) => {
    const members = byKey.get(railKey)!;
    return {
      railKey,
      title: railTitle(railKey, members, teamKey, groups),
      members,
      hasOpen: members.some((m) => !m.minimized),
    };
  });
}

/**
 * SW-EW-38 (R1) — the selected epic's row-2 chips: a pure filter of the single
 * `trayWorkspaceOrder` array by rail key. Preserves source order (integrations-
 * then-stories, id-sorted), so the rendered row-2 array IS the array published
 * to `setTrayOrder` for `Cmd+1..9` (single source per epic). An unknown key
 * yields `[]` — the caller falls back to the first rail pill (AC-8).
 */
export function epicChildEntries(
  entries: readonly TrayWorkspaceEntry[],
  selectedRailKey: string,
): TrayWorkspaceEntry[] {
  return entries.filter((e) => railKeyOf(e) === selectedRailKey);
}

/**
 * SW-EW-38 (AC-6) — the workspace to activate when an epic pill is clicked:
 * the group's most-recently-used member (the first id in the per-team MRU stack
 * that belongs to the group), else its integration/planning workspace, else its
 * first child by the existing within-group order. Pure — the component hands the
 * resolved id to the locked `activateTrayWorkspace` state-rule. `undefined` for
 * an empty group (caller no-ops). Extracted so the MRU/fallback policy is unit-
 * testable without the component.
 */
export function pickMruResumeTarget(
  members: readonly Pick<Workspace, "id" | "kind">[],
  mruStack: readonly string[],
): string | undefined {
  const memberIds = new Set(members.map((m) => m.id));
  return (
    mruStack.find((id) => memberIds.has(id)) ??
    members.find((m) => m.kind === "epic")?.id ??
    members[0]?.id
  );
}

/** OC-6 — one epic rail reduced to what keyboard prev/next-epic needs: its key
 *  and its member workspaces (open ∪ minimized) in tray order. */
export interface EpicRailMembers {
  railKey: string;
  members: readonly Pick<Workspace, "id" | "kind">[];
}

/**
 * OC-6 — reconstruct the ordered epic rails from a flat ordered id list plus a
 * workspaceId→railKey map, WITHOUT the pipeline tree. The keyboard prev/next-epic
 * command runs imperatively (outside React), where the tree — which lives only in
 * `usePipelineTree` — is unreachable. The store already caches the union order
 * (`trayFullOrderByTeam`) and the same grouping the tray renders
 * (`railKeyByWorkspaceId`, built by `syncWorkspaceFrontmatter` from THIS module's
 * `trayWorkspaceOrder` + `railKeyOf`). Grouping the ordered ids by rail key in
 * first-encounter order reproduces `epicRailEntries` order exactly, so keyboard
 * rail order can never diverge from the rendered rail. Ids with no resolvable
 * workspace are skipped. Pure — no store reads.
 */
export function railsFromOrder(
  orderedIds: readonly string[],
  railKeyById: Readonly<Record<string, string>>,
  lookupWorkspace: (id: string) => Pick<Workspace, "id" | "kind"> | undefined,
): EpicRailMembers[] {
  const byKey = new Map<string, Pick<Workspace, "id" | "kind">[]>();
  const order: string[] = [];
  for (const id of orderedIds) {
    const ws = lookupWorkspace(id);
    if (!ws) continue;
    const key = railKeyById[id] ?? "";
    let members = byKey.get(key);
    if (!members) {
      members = [];
      byKey.set(key, members);
      order.push(key);
    }
    members.push(ws);
  }
  return rootsFirst(order).map((railKey) => ({ railKey, members: byKey.get(railKey)! }));
}

/**
 * OC-6 — the adjacent epic rail for keyboard prev/next-epic. Walks the rail order
 * from the selected rail in `direction` with wrap (last↔first), skipping rails
 * with no MRU resume target (empty / all-ineligible), and returns the first
 * eligible rail. `undefined` when there is no OTHER eligible rail (a single
 * eligible rail, or none) — the command is then a safe non-destructive no-op.
 * Pure; reuses `pickMruResumeTarget` for the eligibility test so keyboard nav and
 * pointer resume share ONE resume policy.
 */
export function resolveAdjacentEligibleRail(
  rails: readonly EpicRailMembers[],
  selectedRailKey: string,
  direction: "next" | "prev",
  mruStack: readonly string[],
): EpicRailMembers | undefined {
  const n = rails.length;
  if (n === 0) return undefined;
  const startIdx = rails.findIndex((r) => r.railKey === selectedRailKey);
  const step = direction === "next" ? 1 : -1;
  // When the selected rail isn't in the list, begin just outside the range so the
  // first visited index is the near end (0 for next, n-1 for prev).
  const base = startIdx === -1 ? (direction === "next" ? -1 : n) : startIdx;
  for (let i = 1; i <= n; i++) {
    const idx = (((base + step * i) % n) + n) % n;
    if (startIdx !== -1 && idx === startIdx) break; // wrapped fully back → no other eligible
    const rail = rails[idx];
    if (pickMruResumeTarget(rail.members, mruStack)) return rail;
  }
  return undefined;
}

/**
 * OC-7 — the AC-8 selected-rail fallback, shared by every rail-scoped nav axis
 * (epic-nav and grouped workspace-cycle): keep the stored rail key when it still
 * maps to a visible rail, else fall back to the first rail in visible order — so
 * navigation always starts from the rail the user actually sees selected, never a
 * stale/absent one. `visibleRailKeys` is the rail key of each entry in visible
 * order (duplicates are fine). Pure — no store reads.
 */
export function resolveSelectedRailKey(
  visibleRailKeys: readonly string[],
  storedKey: string,
): string {
  return visibleRailKeys.includes(storedKey) ? storedKey : (visibleRailKeys[0] ?? "");
}

/**
 * OC-7 — the adjacent workspace id for keyboard prev/next-WORKSPACE, within one
 * ordered scope: flat mode passes the whole visible tray order; grouped mode
 * passes only the selected rail's member ids. Steps ±1 from `currentId` with wrap
 * (last↔first). `undefined` when the scope has ≤1 member (nowhere to go) — a safe
 * non-destructive no-op. When `currentId` isn't in scope (e.g. the selection just
 * moved rails), lands on the near end so the command still moves. Pure — no store
 * reads. The workspace-axis sibling of {@link resolveAdjacentEligibleRail}.
 */
export function resolveAdjacentWorkspaceInScope(
  scope: readonly string[],
  currentId: string,
  direction: "next" | "prev",
): string | undefined {
  const n = scope.length;
  if (n <= 1) return undefined;
  const idx = scope.indexOf(currentId);
  if (idx === -1) return direction === "next" ? scope[0] : scope[n - 1];
  const step = direction === "next" ? 1 : -1;
  return scope[(((idx + step) % n) + n) % n];
}
