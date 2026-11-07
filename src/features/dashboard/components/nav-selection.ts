// SPDX-License-Identifier: MIT
/**
 * SW-DNS-1: what the left nav has selected, as a dashboard reads it through
 * `codesense.getNavSelection()` / `codesense.onNavSelectionChange(cb)`.
 *
 * The selection belongs to ONE team: the doc's own (see `resolveDocTeam`). It is
 * read from that team's slices of the viewer store (`activeTab`, `drillPaths`),
 * and a path is reported only when it resolves into that team's own content root,
 * so a doc never sees another team's (or a source repo's) paths. Identity — the
 * story or epic a path is — comes from the context index.
 *
 * Resolutions are shared and debounced; the host puts the result in slot context.
 * Hidden documents catch up when shown.
 */
import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";

import type {
  ContextKind,
  ContextQuery,
  ContextQueryResult,
  ContextRow,
} from "@/shared/lib/context-query/types";
import { buildTeamKey } from "@/shared/lib/team-key";
import { createListenerGroup } from "@/shared/lib/tauri-listeners";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { bridgeDocContext, queryContext, type ContentLocation } from "@/tauri/commands";
import { CONTEXT_INDEX_CHANGED_EVENT } from "@/tauri/events";

/** How long nav changes gather before one resolution (the old Pipeline panel used 50 ms). */
export const NAV_SELECTION_DEBOUNCE_MS = 80;
/** At most one re-resolution per this long for context-index changes (a git pull
 *  is a burst of them, and none says which paths changed). */
export const INDEX_THROTTLE_MS = 1000;

export type NavSelectionFile = {
  /** Absolute path of the active tab. */
  path: string;
  /** Relative to the team's content root. */
  relPath: string;
  /** What the index says it is; `null` when it isn't indexed. */
  kind: ContextKind | null;
  id: string | null;
  title: string | null;
  /** A story's parent epic (`parent_epic`, else its folder epic); an epic doc's own id. */
  epic: string | null;
};

export type NavSelectionEpic = { id: string; title: string | null; path: string; relPath: string };

export type NavSelectionDrill = {
  path: string;
  relPath: string;
  /** The epic main doc in the drilled folder or its nearest ancestor. */
  epic: NavSelectionEpic | null;
};

export type NavSelection = {
  org: string;
  team: string;
  file: NavSelectionFile | null;
  /** `null` when the nav is at its root. */
  drill: NavSelectionDrill | null;
};

export type NavTeam = { org: string; team: string };
export type NavInputs = { activePath: string | null; drillPath: string | null };

export interface NavSelectionDeps {
  docContext: (path: string) => Promise<ContentLocation | null>;
  query: (q: ContextQuery, page: string) => Promise<ContextQueryResult>;
}

// Lazy: a module that imports this one must not touch the commands at load time.
const realDeps: NavSelectionDeps = {
  docContext: (path) => bridgeDocContext(path),
  query: (q, page) => queryContext(q, page),
};

/** What kind of doc is asking, from the trusted host (never the doc). */
export type NavDocKind = {
  docPath: string;
  pane?: boolean;
  message?: { org: string; team: string };
};

/**
 * The team whose nav a doc sees: a pane's is the team on screen (it is shown in
 * that team's right pane); an inbox message's is the message's; a centre doc's is
 * the team its file lives in (team views stay mounted through a switch, so this
 * must not follow the active team), else — a private or all-orgs doc — the team
 * on screen.
 */
export async function resolveDocTeam(
  doc: NavDocKind,
  active: NavTeam | null,
  deps: NavSelectionDeps = realDeps,
): Promise<NavTeam | null> {
  if (doc.message) return { org: doc.message.org, team: doc.message.team };
  if (doc.pane) return active;
  const loc = await deps.docContext(doc.docPath).catch(() => null);
  if (loc?.layer === "team" && loc.org && loc.team) return { org: loc.org, team: loc.team };
  return active;
}

/** The team's own slices of the nav state. */
export function navInputsFor(
  state: { activeTab: Record<string, string>; drillPaths: Record<string, string> },
  team: NavTeam,
): NavInputs {
  const key = buildTeamKey(team.org, team.team);
  return { activePath: state.activeTab[key] || null, drillPath: state.drillPaths[key] || null };
}

function dirname(p: string): string {
  const i = p.lastIndexOf("/");
  return i <= 0 ? "/" : p.slice(0, i);
}

function relTo(root: string, p: string): string {
  return p === root ? "" : p.slice(root.length + 1);
}

/** Where `path` sits, when that is `team`'s own content root. */
async function teamLocation(
  path: string | null,
  team: NavTeam,
  deps: NavSelectionDeps,
): Promise<ContentLocation | null> {
  if (!path || !path.startsWith("/")) return null;
  const loc = await deps.docContext(path).catch(() => null);
  if (!loc || loc.layer !== "team" || loc.org !== team.org || loc.team !== team.team) return null;
  if (path !== loc.contentRoot && !path.startsWith(`${loc.contentRoot}/`)) return null;
  return loc;
}

function str(v: unknown): string | null {
  return typeof v === "string" && v.trim() ? v.trim() : null;
}

/** An epic main doc's id: its `epic_id` / `sub_epic`, else its folder name. */
function epicIdOf(row: ContextRow): string {
  return (
    str(row.fields.sub_epic) ??
    str(row.fields.epic_id) ??
    dirname(row.path).split("/").pop() ??
    row.id
  );
}

// The epic main docs of a content root (epics, and `type: sub-epic` READMEs, which
// the index files as docs), cached per root until the index changes.
const epicCache = new Map<string, Promise<ContextRow[]>>();

function epicDocs(root: string, page: string, deps: NavSelectionDeps): Promise<ContextRow[]> {
  let hit = epicCache.get(root);
  if (!hit) {
    const where = { sameRootAs: `${root}/`, layers: ["team" as const] };
    hit = Promise.all([
      deps.query({ where: { ...where, kinds: ["epic"] }, select: ["epic_id"], limit: 5000 }, page),
      deps.query(
        {
          where: { ...where, kinds: ["doc"] },
          filters: [{ key: "type", op: "in", value: ["sub-epic", "sub_epic", "subepic"] }],
          select: ["epic_id", "sub_epic", "type"],
          limit: 5000,
        },
        page,
      ),
    ]).then(
      ([a, b]) => [...(a.rows ?? []), ...(b.rows ?? [])],
      () => [],
    );
    epicCache.set(root, hit);
    void hit.then((rows) => {
      // A failed read is not remembered: the next change retries.
      if (rows.length === 0 && epicCache.get(root) === hit) epicCache.delete(root);
    });
  }
  return hit;
}

// The last few resolutions (two docs on two teams, a pane and a centre doc), so
// every watching doc shares one resolution per selection.
const MEMO_SLOTS = 8;
const resolutionMemo = new Map<string, Promise<NavSelection>>();
// When caches were last dropped for an index change: one burst, one drop, however
// many docs are watching.
let lastIndexClear = 0;

/** Drop cached epics and resolutions for an index change, once per burst. */
function clearForIndexChange(): void {
  const now = Date.now();
  if (now - lastIndexClear < INDEX_THROTTLE_MS / 2) return;
  lastIndexClear = now;
  clearNavSelectionCaches();
}

/** Forget cached epics and the last resolution (the index changed). */
export function clearNavSelectionCaches(): void {
  epicCache.clear();
  resolutionMemo.clear();
}

async function resolveUncached(
  team: NavTeam,
  inputs: NavInputs,
  page: string,
  deps: NavSelectionDeps,
): Promise<NavSelection> {
  const [fileLoc, drillLoc] = await Promise.all([
    teamLocation(inputs.activePath, team, deps),
    teamLocation(inputs.drillPath, team, deps),
  ]);

  let file: NavSelectionFile | null = null;
  if (fileLoc && inputs.activePath && inputs.activePath !== fileLoc.contentRoot) {
    const path = inputs.activePath;
    const result = await deps
      .query(
        {
          where: { sameRootAs: path, pathPrefix: path, layers: ["team"] },
          select: ["epic_id", "sub_epic", "relationships"],
          limit: 5,
        },
        page,
      )
      .catch(() => null);
    const row = result?.rows?.find((r) => r.path === path) ?? null;
    const rel = row?.relationships;
    file = {
      path,
      relPath: relTo(fileLoc.contentRoot, path),
      kind: row?.kind ?? null,
      id: row ? (row.kind === "epic" ? epicIdOf(row) : row.id) : null,
      title: row?.title ?? null,
      epic: row
        ? row.kind === "epic"
          ? epicIdOf(row)
          : (str(rel?.parentEpic) ?? str(rel?.folderEpic))
        : null,
    };
  }

  let drill: NavSelectionDrill | null = null;
  const drillPath = inputs.drillPath?.replace(/\/+$/, "") ?? null;
  if (drillLoc && drillPath && drillPath !== drillLoc.contentRoot) {
    const root = drillLoc.contentRoot;
    const docs = await epicDocs(root, page, deps);
    let best: ContextRow | null = null;
    let bestDepth = -1;
    for (const r of docs) {
      const dir = dirname(r.path);
      if (dir === root || !(drillPath === dir || drillPath.startsWith(`${dir}/`))) continue;
      if (dir.length > bestDepth) {
        best = r;
        bestDepth = dir.length;
      }
    }
    drill = {
      path: drillPath,
      relPath: relTo(root, drillPath),
      epic: best
        ? {
            id: epicIdOf(best),
            title: best.title ?? null,
            path: best.path,
            relPath: relTo(root, best.path),
          }
        : null,
    };
  }

  return { org: team.org, team: team.team, file, drill };
}

/** Resolve `team`'s selection; concurrent asks for the same inputs share one. */
export function resolveNavSelection(
  team: NavTeam,
  inputs: NavInputs,
  page: string,
  deps: NavSelectionDeps = realDeps,
): Promise<NavSelection> {
  const key = JSON.stringify([team.org, team.team, inputs.activePath, inputs.drillPath]);
  if (deps !== realDeps) return resolveUncached(team, inputs, page, deps);
  const hit = resolutionMemo.get(key);
  if (hit) {
    resolutionMemo.delete(key);
    resolutionMemo.set(key, hit);
    return hit;
  }
  const value = resolveUncached(team, inputs, page, deps);
  resolutionMemo.set(key, value);
  while (resolutionMemo.size > MEMO_SLOTS) {
    resolutionMemo.delete(resolutionMemo.keys().next().value as string);
  }
  // A failed resolution is not remembered.
  value.catch(() => {
    if (resolutionMemo.get(key) === value) resolutionMemo.delete(key);
  });
  return value;
}

function activeTeam(): NavTeam | null {
  const { activeOrgSlug, activeTeamName } = useOrgStore.getState();
  return activeOrgSlug && activeTeamName ? { org: activeOrgSlug, team: activeTeamName } : null;
}

/** Resolve one live document's team selection for its slot-context lifecycle. */
export function useNavSelectionPush(opts: {
  live: boolean;
  docKey: string | null;
  doc: NavDocKind;
  visible?: boolean;
  onSelection: (selection: NavSelection | null) => void;
}): void {
  const { live, docKey } = opts;
  const visible = opts.visible ?? true;
  const latest = useRef(opts);
  useEffect(() => {
    latest.current = opts;
  });
  const visibleRef = useRef(visible);
  const wakeRef = useRef<(() => void) | null>(null);
  useEffect(() => {
    visibleRef.current = visible;
    if (visible) wakeRef.current?.();
  }, [visible]);
  const { docPath, pane, message } = opts.doc;
  const messageOrg = message?.org;
  const messageTeam = message?.team;

  useEffect(() => {
    if (!live || !docKey) return;
    const doc: NavDocKind = {
      docPath,
      pane,
      message: messageOrg && messageTeam ? { org: messageOrg, team: messageTeam } : undefined,
    };
    let disposed = false;
    let gen = 0;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let indexTimer: ReturnType<typeof setTimeout> | null = null;
    let lastInputs: string | null = null;
    let lastPushed: string | null = null;
    // A change seen while off screen: resolved when the doc is shown again.
    let missedWhileHidden = false;

    const push = (selection: NavSelection | null) => {
      const key = JSON.stringify(selection);
      if (key === lastPushed) return;
      lastPushed = key;
      latest.current.onSelection(selection);
    };
    const run = async (force: boolean) => {
      // The first answer is always sent; later ones wait until the doc is seen.
      if (lastPushed !== null && !visibleRef.current) {
        missedWhileHidden = true;
        return;
      }
      const my = ++gen;
      let selection: NavSelection | null = null;
      try {
        const team = await resolveDocTeam(doc, activeTeam());
        if (disposed || my !== gen) return;
        const inputs = team ? navInputsFor(useViewerStore.getState(), team) : null;
        const inputsKey = JSON.stringify([team, inputs]);
        if (!force && inputsKey === lastInputs) return;
        lastInputs = inputsKey;
        if (team && inputs) selection = await resolveNavSelection(team, inputs, docPath);
      } catch {
        // Never leave a first `getNavSelection()` waiting; keep a known selection.
        if (disposed || my !== gen || lastPushed !== null) return;
        lastInputs = null;
        push(null);
        return;
      }
      if (disposed || my !== gen) return;
      push(selection);
    };
    const schedule = (force = false) => {
      if (timer !== null) clearTimeout(timer);
      timer = setTimeout(() => {
        timer = null;
        void run(force);
      }, NAV_SELECTION_DEBOUNCE_MS);
    };
    wakeRef.current = () => {
      if (!missedWhileHidden) return;
      missedWhileHidden = false;
      schedule(true);
    };

    void run(true);
    const offViewer = useViewerStore.subscribe((s, prev) => {
      if (s.activeTab !== prev.activeTab || s.drillPaths !== prev.drillPaths) schedule();
    });
    const offOrg = useOrgStore.subscribe((s, prev) => {
      if (s.activeOrgSlug !== prev.activeOrgSlug || s.activeTeamName !== prev.activeTeamName) {
        schedule();
      }
    });
    // An index change can change what a path is (a story's epic, a new epic doc).
    // The event names no paths, so a burst is throttled to one re-resolution.
    const group = createListenerGroup();
    group.track(
      listen(CONTEXT_INDEX_CHANGED_EVENT, () => {
        if (indexTimer !== null) return;
        indexTimer = setTimeout(() => {
          indexTimer = null;
          clearForIndexChange();
          void run(true);
        }, INDEX_THROTTLE_MS);
      }),
    );
    return () => {
      disposed = true;
      wakeRef.current = null;
      if (timer !== null) clearTimeout(timer);
      if (indexTimer !== null) clearTimeout(indexTimer);
      offViewer();
      offOrg();
      group.dispose();
    };
  }, [live, docKey, docPath, pane, messageOrg, messageTeam]);
}
