// SPDX-License-Identifier: MIT
// ---------------------------------------------------------------------------
// RF-12: the pipeline-status-nav ITEM-LIST data prep, relocated verbatim from
// `pipeline-status-nav.tsx` (no behavior change). Owns the non-scanner viewer-
// root listing, the generic-children fetch, the counts/graph/flatten pipeline,
// and the whole SLW-NC-7 pin subsystem (repo-relative key bridge + team/personal
// pin state + the ordered/stale-aware Pinned section). Returns the final
// `flatItems` the virtualizer renders plus the pin gating the context menu reads.
// The orchestrator keeps the reveal-scroll effect + virtual list ref.
// ---------------------------------------------------------------------------
import { useCallback, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { listDirectory, getTeamNavPins, setTeamNavPin, type FileEntry } from "@/tauri/commands";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useCompositeRefreshStore } from "@/shared/stores/composite-refresh-store";
import {
  isViewerFile,
  isDerivedRenderArtifact,
  getViewerFileType,
} from "@/shared/lib/viewer-file-type";
import { compareNavNames, computeCountsByAbsPath } from "@/features/dashboard/lib/nav-ordering";
import { buildPipelineGraph, type PipelineGraph } from "@/shared/lib/pipeline-graph/pipeline-graph";
import {
  collectReadyPaths,
  flattenByStage,
  flattenNavTree,
  buildNavSearchFilter,
  collectPinTargets,
  collectTreePinTargets,
  prependPinnedSection,
} from "@/features/dashboard/lib/flatten-nav-tree";
import { partitionPinnedNav, normalizePinPath } from "@/features/dashboard/lib/nav-pins";
import { useGenericChildrenByPath } from "@/features/dashboard/lib/use-generic-children-by-path";
import type { DerivedPipelineTree, StoryRef } from "@/shared/lib/pipeline-graph/types";
import { normalizeStatus } from "@/shared/lib/pipeline-graph/pipeline-utils";
import { collectAllStories } from "@/shared/lib/pipeline-graph/collect-all-stories";
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";
import { useStoryMarks } from "@/features/viewer/lib/cli-workspace/store/terminal-tab-selectors";
import {
  countLenses,
  inLens,
  stageGroupOf,
  type LensStory,
  type NavLensState,
} from "@/shared/lib/nav-lens";
import type { NavAgentContext } from "@/features/agent-context/lib/nav-agent-context";
import type { PinnedNavItem } from "@/features/dashboard/lib/nav-pins";
import type { NavSurface } from "@/features/dashboard/lib/use-nav-surface";
import { flattenChanges } from "@/features/dashboard/lib/flatten-changes";
import type { NavChange } from "@/features/git-sync/lib/nav-changes";

// SLW-NC-7: stable empty-array identity so the store default + team-pins default
// don't spawn a new [] each render (would thrash memos).
const EMPTY_PINS: string[] = [];
// Soft cap on TEAM pins — allowed past it, but the write nudges with a toast.
const TEAM_PIN_SOFT_CAP = 20;

interface UseNavItemsArgs {
  tree: DerivedPipelineTree;
  graphProp: PipelineGraph | undefined;
  expandedPaths: Set<string>;
  genericExpandedPaths: Set<string>;
  refreshKey: number | undefined;
  viewerRootPath: string | undefined;
  isDrilledIn: boolean | undefined;
  teamKey: string;
  surface: NavSurface;
  rootPath: string | undefined;
  onFileSelect: (filePath: string) => void;
  onDrillPathChange?: (absolutePath: string) => void;
  /** SW-LNS-1: live left-nav search query. Empty / whitespace ⇒ unfiltered. */
  searchQuery?: string;
  /** The team's lens: which stories show, and how they are arranged. */
  view: NavLensState;
  /** The picked agent's context: its lenses count and show only what that agent gets. */
  agentContext?: NavAgentContext;
  /** The Changes view's files (already in scope); listed instead of the tree when the view is
   *  Changes. */
  changes: readonly NavChange[];
  changesCloneRoot?: string | null;
  /** Folders collapsed in the Changes view. */
  changesCollapsed: ReadonlySet<string>;
}

export function useNavItems({
  tree,
  graphProp,
  expandedPaths,
  genericExpandedPaths,
  refreshKey,
  viewerRootPath,
  isDrilledIn,
  teamKey,
  surface,
  rootPath,
  onFileSelect,
  onDrillPathChange,
  searchQuery,
  view,
  agentContext,
  changes,
  changesCloneRoot,
  changesCollapsed,
}: UseNavItemsArgs) {
  // List non-scanner content at the viewer root.
  // R-14: scanner walks all of viewerRoot, so .md and .html files (scanner-
  // owned) are filtered OUT here to avoid double-render. The legacy
  // pipeline-root exclusion is dropped — that dir is now a regular sub-epic the
  // scanner walks. `CLAUDE.md` likewise becomes a DocRow via the scanner.
  // Non-scanner content (images, json, etc.) still lands here as
  // `generic-dir` / `generic-file`.
  const [viewerRootEntries, setViewerRootEntries] = useState<FileEntry[]>([]);
  useEffect(() => {
    if (!viewerRootPath || isDrilledIn) {
      // Synchronous reset on filter change so stale entries don't
      // flash before the async listDirectory below resolves. The
      // entries are populated from a side-effectful IPC call so they
      // can't be a pure derived value — set-state-in-effect is the
      // correct pattern here.
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setViewerRootEntries([]);
      return;
    }
    let stale = false;
    listDirectory(viewerRootPath)
      .then((entries) => {
        if (stale) return;
        const filtered = entries.filter((e) => {
          // R-14: dirs always considered — scanner will emit empty
          // sub-epics for md-empty dirs which get suppressed downstream,
          // letting generic-dir cover them here.
          if (e.is_dir) return true;
          // Files the scanner already emits via `tree.docs[]` must be
          // excluded here, or they render twice when the scan root == the
          // viewer root (empty pipelineRoot / flat layout): md + html (F.30)
          // and `.excalidraw` supporting docs (SW-SC-1).
          const ext = e.extension?.toLowerCase() ?? null;
          if (ext === "md" || ext === "html" || ext === "htm" || ext === "excalidraw") {
            return false;
          }
          // SW-SC-7: agent-only derived render — never a nav row, even though
          // `.png` is now viewable.
          if (isDerivedRenderArtifact(e.name)) return false;
          // SW-SC-7: images are scanner-routed into `tree.docs[]` (added to
          // SUPPORTING_DOC_EXTENSIONS), so they already render as doc rows.
          // Excluding them here prevents a double-render when the scan root ==
          // viewer root (flat layout) — the same reason md/html/excalidraw are
          // excluded above.
          if (getViewerFileType(e.name) === "image") return false;
          // Remaining viewable types surface here. SW-VE-2: `code`/config/data
          // files are now viewer files and land here — the scanner does NOT
          // own them (they aren't in SUPPORTING_DOC_EXTENSIONS), so there is no
          // double-render; this is the designed extension point that lets a
          // root-level `.yaml`/`.json` list, matching the expanded-subfolder path.
          return isViewerFile(e.extension);
        });
        setViewerRootEntries(filtered.sort((a, b) => compareNavNames(a.name, b.name)));
      })
      .catch(() => {
        if (!stale) setViewerRootEntries([]);
      });
    return () => {
      stale = true;
    };
  }, [viewerRootPath, isDrilledIn, refreshKey]);

  // Lifted side effects: per-folder listDirectory calls live here now.
  // PTS-2 follow-up: useOrphanFilesByPath retired. Md/html files in expanded
  // sub-epic dirs that aren't pipeline stories now arrive via the scanner
  // as `tree.docs[]` per-node — no separate listDirectory pass needed.
  const genericChildrenByPath = useGenericChildrenByPath({
    expandedPaths: genericExpandedPaths,
    refreshKey,
  });

  // Pre-computed per-sub-epic counts. Single bottom-up walk on tree change,
  // read in O(1) per folder-header inside flattenNavTree. Doubles as the
  // toolbar's aggregate via the root entry.
  const countsByAbsPath = useMemo(() => computeCountsByAbsPath(tree), [tree]);

  const epicTotal = countsByAbsPath.get(tree.absoluteDirPath)?.total ?? 0;
  const epicDone = countsByAbsPath.get(tree.absoluteDirPath)?.done ?? 0;

  // SW-FX-1: production passes the full-tree graph; fall back to a graph from
  // `tree` only when rendered standalone (tests/storybook).
  const graph = useMemo(() => graphProp ?? buildPipelineGraph(tree), [graphProp, tree]);

  // The lens reads each story's stage role (the live taxonomy) and its agent mark (the same
  // scope its workspace pill reads). In an agent's context, only what that agent gets counts.
  const stories = useMemo(() => collectAllStories(tree), [tree]);
  const markScopes = useMemo(
    () => stories.map((st) => ({ filePath: st.filePath, storyId: st.storyId ?? undefined })),
    [stories],
  );
  const marks = useStoryMarks(teamKey, markScopes);
  const statesById = useStatusTaxonomyStore((st) => st.byId);
  // Ready to start reads the same full-graph readiness the rows draw.
  const readyPaths = useMemo(() => collectReadyPaths(tree, graph), [tree, graph]);
  const lensStoryOf = useCallback(
    (st: StoryRef): LensStory => ({
      role: statesById.get(normalizeStatus(st.status))?.role,
      mark: marks[st.filePath],
      ready: readyPaths.has(st.filePath),
    }),
    [statesById, marks, readyPaths],
  );
  const inScope = useCallback(
    (st: StoryRef): boolean => {
      if (!agentContext) return true;
      const rel = agentContext.relOf(st.filePath);
      return rel !== null && agentContext.membership(rel).state !== "out";
    },
    [agentContext],
  );
  const lensCounts = useMemo(
    () => countLenses(stories.filter(inScope).map(lensStoryOf)),
    [stories, inScope, lensStoryOf],
  );
  const changesView = view.view === "changes";
  // The Changes view lists files: the story lens and the stage arrangement shape Files only.
  const byStage = !changesView && view.arrange === "stage";
  const lensOn = !changesView && view.lens !== "everything";
  // Everything in the folder tree keeps every row (an agent's context mutes, never hides);
  // a lens, or the stage list, keeps only the stories it selects.
  const storyFilter = useMemo(
    () =>
      lensOn || byStage
        ? (st: StoryRef) => inScope(st) && inLens(view.lens, lensStoryOf(st))
        : undefined,
    [lensOn, byStage, inScope, view.lens, lensStoryOf],
  );

  // SW-LNS-1: build the search overlay from the current query + tree surfaces.
  // Memoized separately from the walk so a non-search render doesn't allocate it
  // and a stable overlay doesn't re-trigger `baseFlatItems`.
  const search = useMemo(
    () =>
      buildNavSearchFilter({
        query: searchQuery ?? "",
        tree,
        viewerRootEntries,
        genericChildrenByPath,
        isDrilledIn: !!isDrilledIn,
        storyFilter: byStage ? undefined : storyFilter,
        storiesOnly: view.storiesOnly,
      }),
    [
      searchQuery,
      tree,
      viewerRootEntries,
      genericChildrenByPath,
      isDrilledIn,
      byStage,
      storyFilter,
      view.storiesOnly,
    ],
  );

  // The Changes view's files, narrowed by the typed text (by file name).
  const query = (searchQuery ?? "").trim().toLowerCase();
  const shownChanges = useMemo(
    () =>
      query
        ? changes.filter((c) => (c.absPath.split("/").pop() ?? "").toLowerCase().includes(query))
        : changes,
    [changes, query],
  );

  // Flat list of the real tree (before the SLW-NC-7 Pinned section is prepended).
  const baseFlatItems = useMemo(
    () =>
      changesView
        ? flattenChanges({
            tree,
            graph,
            changes: shownChanges,
            cloneRoot: changesCloneRoot,
            countsByAbsPath,
            collapsed: changesCollapsed,
          })
        : byStage
          ? flattenByStage({
              tree,
              graph,
              keep: (st) =>
                (storyFilter?.(st) ?? true) &&
                (!search || search.matchedFilePaths.has(st.filePath)),
              groupOf: (st) => stageGroupOf(lensStoryOf(st)),
              doneOpen: view.doneOpen,
            })
          : flattenNavTree({
              tree,
              expandedPaths,
              graph,
              viewerRootEntries,
              genericExpandedPaths,
              genericChildrenByPath,
              countsByAbsPath,
              isDrilledIn: !!isDrilledIn,
              search: search ?? undefined,
            }),
    [
      changesView,
      shownChanges,
      changesCloneRoot,
      changesCollapsed,
      byStage,
      storyFilter,
      lensStoryOf,
      view.doneOpen,
      tree,
      expandedPaths,
      graph,
      viewerRootEntries,
      genericExpandedPaths,
      genericChildrenByPath,
      countsByAbsPath,
      isDrilledIn,
      search,
    ],
  );

  // SLW-NC-7 — the "Pinned" section. Pins are stored repo-relative (to `rootAnchor`)
  // so a git-shared TEAM pin resolves for every teammate regardless of their clone
  // path; `toRel`/`toAbs` bridge that key space and the nav's absolute paths.
  const rootAnchor = useMemo(
    () => (rootPath ?? tree.absoluteDirPath).replace(/\/+$/, ""),
    [rootPath, tree.absoluteDirPath],
  );
  const toRel = useCallback(
    (abs: string): string | null => {
      if (abs === rootAnchor) return "";
      if (abs.startsWith(rootAnchor + "/")) return abs.slice(rootAnchor.length + 1);
      return null;
    },
    [rootAnchor],
  );
  const toAbs = useCallback((rel: string) => `${rootAnchor}/${rel}`, [rootAnchor]);

  // Personal pins: local per-team app-state (never committed). Team pins: git-shared
  // in `designations.yml`, re-fetched on the SLW-NC-5 composite-refresh nonce so a
  // teammate's pulled pin appears without a restart (AC-8).
  const pinsEnabled = view.source.kind === "clone";
  const personalPins = useViewerStore((s) =>
    pinsEnabled ? (s.statusNavPinnedPaths?.[teamKey] ?? EMPTY_PINS) : EMPTY_PINS,
  );
  const toggleStoredPersonalPin = useViewerStore((s) => s.toggleStatusNavPinPersonal);
  const togglePersonalPin = useCallback(
    (key: string, path: string) => {
      if (pinsEnabled) toggleStoredPersonalPin(key, path);
    },
    [pinsEnabled, toggleStoredPersonalPin],
  );
  const compositeNonce = useCompositeRefreshStore((s) => s.nonces[teamKey] ?? 0);
  const { orgRoot: pinOrgRoot, team: pinTeam } = surface;
  const [teamPins, setTeamPins] = useState<string[]>(EMPTY_PINS);
  useEffect(() => {
    let cancelled = false;
    // No active team ⇒ resolve to empty (clears any stale team pins on team switch);
    // otherwise fetch. setState only ever runs in the async callback, never in the
    // effect body (no cascading render). Fail-open: a nav shortcut hint must never
    // break the render (AC-10).
    const load =
      !pinOrgRoot || !pinTeam ? Promise.resolve(EMPTY_PINS) : getTeamNavPins(pinOrgRoot, pinTeam);
    load
      .then((pins) => {
        if (!cancelled) setTeamPins(pins ?? EMPTY_PINS);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [pinOrgRoot, pinTeam, compositeNonce]);

  const [pinnedCollapsed, setPinnedCollapsed] = useState(false);

  // Which nav paths (repo-relative) can be pinned + how they render — shared by the
  // section merge below AND the context menu's pin/unpin gating. The FULL tree
  // (expand-independent) is the base so a pin to a collapsed-away nested epic/doc
  // stays live, not falsely stale (AC-7); unioned with the visible rows so
  // currently-expanded generic children (not in the tree) are covered too. A pin
  // resolving to NEITHER is genuinely stale.
  const pinResolvable = useMemo(
    () =>
      new Map([...collectTreePinTargets(tree, toRel), ...collectPinTargets(baseFlatItems, toRel)]),
    [tree, baseFlatItems, toRel],
  );
  // Merge team + personal into the ordered/deduped/stale-aware section, then prepend.
  const pinnedItems = useMemo(
    () => (pinsEnabled ? partitionPinnedNav(teamPins, personalPins, pinResolvable) : []),
    [pinsEnabled, teamPins, personalPins, pinResolvable],
  );
  // SW-LNS-1: while searching, hide the Pinned shortcut section — pins are a
  // one-click jump, not search results, and leaving them stacked above the match
  // list reads as noise. The pin maps + toggles are still built so the context
  // menu's pin/unpin item stays accurate on the matched rows.
  const flatItems = useMemo(
    () =>
      search || byStage || changesView
        ? baseFlatItems
        : prependPinnedSection(baseFlatItems, pinnedItems, pinnedCollapsed),
    [baseFlatItems, pinnedItems, pinnedCollapsed, search, byStage, changesView],
  );

  // Is the row currently team- / personally-pinned? (drives the menu's pin vs unpin).
  const teamPinSet = useMemo(() => new Set(teamPins.map(normalizePinPath)), [teamPins]);
  const personalPinSet = useMemo(() => new Set(personalPins.map(normalizePinPath)), [personalPins]);

  // Optimistic team-pin write: flip local state now, reconcile against the committed
  // value, revert + toast on failure (AC-10). Soft-cap the team set to keep the
  // shared section from becoming clutter — allow past the cap, but nudge (AC: count cap).
  const toggleTeamPin = useCallback(
    (absPath: string, pinned: boolean) => {
      if (!pinOrgRoot || !pinTeam) {
        toast.error("Can't pin — no active team for this workspace");
        return;
      }
      const rel = toRel(absPath);
      if (rel == null) return;
      const key = normalizePinPath(rel);
      if (!key) return;
      if (pinned && !teamPinSet.has(key) && teamPins.length >= TEAM_PIN_SOFT_CAP) {
        toast.warning(
          `That's a lot of team pins (${teamPins.length + 1}). Consider unpinning a few to keep the list tidy.`,
        );
      }
      setTeamPins((cur) => {
        const without = cur.filter((p) => normalizePinPath(p) !== key);
        return pinned ? [...without, key] : without;
      });
      setTeamNavPin(pinOrgRoot, pinTeam, key, pinned).catch((e) => {
        // Delta revert (undo THIS key's optimistic change only), not a full-snapshot
        // restore — so a concurrent successful toggle of a DIFFERENT pin isn't
        // clobbered by this failure's rollback (review r1).
        setTeamPins((cur) => {
          const without = cur.filter((p) => normalizePinPath(p) !== key);
          return pinned ? without : [...without, key];
        });
        toast.error(`Couldn't ${pinned ? "pin" : "unpin"} for the team: ${String(e)}`);
      });
    },
    [pinOrgRoot, pinTeam, toRel, teamPins, teamPinSet],
  );

  // Click a pinned row → go to the real node (AC-4). A DIR pin reveals the folder by
  // drilling the nav into it (a dir isn't openable as a file — routing it through
  // onFileSelect would break); a FILE pin opens it (selection scrolls it into view).
  // No second expandable subtree. A stale pin is a no-op click — "Remove pin" clears it.
  const revealPinned = useCallback(
    (pin: PinnedNavItem) => {
      if (pin.stale) return;
      const abs = toAbs(pin.path);
      if (pin.isDir) onDrillPathChange?.(abs);
      else onFileSelect(abs);
    },
    [toAbs, onFileSelect, onDrillPathChange],
  );

  // Unpin from inside the Pinned section (hover icon + right-click), at the pin's
  // own scope (AC-6): a team row unpins the team, a personal row the local set.
  const unpinPinnedRow = useCallback(
    (pin: PinnedNavItem) => {
      if (pin.scope === "team") toggleTeamPin(toAbs(pin.path), false);
      else togglePersonalPin(teamKey, pin.path);
    },
    [toggleTeamPin, togglePersonalPin, toAbs, teamKey],
  );

  const menuRows = () => {
    if (byStage || changesView) return baseFlatItems;
    const allExpanded = new Set<string>();
    const collect = (node: DerivedPipelineTree) => {
      allExpanded.add(node.absoluteDirPath);
      node.subEpics.forEach(collect);
    };
    collect(tree);
    return flattenNavTree({
      tree,
      expandedPaths: allExpanded,
      graph,
      viewerRootEntries,
      genericExpandedPaths: new Set(genericChildrenByPath.keys()),
      genericChildrenByPath,
      countsByAbsPath,
      isDrilledIn: !!isDrilledIn,
      search: search ?? undefined,
    });
  };
  return {
    menuRows,
    flatItems,
    epicTotal,
    epicDone,
    rootAnchor,
    toRel,
    toAbs,
    pinResolvable,
    teamPinSet,
    personalPinSet,
    toggleTeamPin,
    togglePersonalPin,
    revealPinned,
    unpinPinnedRow,
    pinnedCollapsed,
    setPinnedCollapsed,
    /** SW-LNS-1: true while a search query or a lens filters `flatItems`. */
    isSearching: search !== null || byStage || changesView,
    /** How many stories each lens holds, in the current scope. */
    lensCounts,
  };
}
