// SPDX-License-Identifier: MIT
/**
 * SW-RP-1: the structured right-pane layout model + its pure operations.
 *
 * The right pane is an ordered list of **sections**, each a **group** of
 * registered panels switched by tabs, with a `collapsed` flag and a relative
 * `size`. These functions are pure (no store, no React) so the behaviour is
 * unit-testable in isolation; the store and the renderer call them.
 *
 * Mirrors the shape/defaulting discipline of `ForegroundLayout` in the viewer
 * store — every persisted value flows through a defaulter/coercer so a malformed
 * or absent blob degrades to a sane default instead of crashing.
 */

export const SCRATCH_PANEL_ID = "scratch";
// The right pane holds only the user's own surfaces. Retired panels ("pipeline", "board",
// "worktrees", "automations", "agent-result", "running-agents", "workflow-library",
// "triggers", "agent-profiles") have no id constant: a persisted section carrying one is foreign and
// pruned by `reconcileLayoutToBuiltins`. Agents live in the bottom CLI workspace.
/** One Inbox placement; its dashboard follows the active team and profile. */
export const INBOX_PANEL_ID = "slot:right-pane:inbox";
/** Stable ids for the built-in default sections (must survive reload so the
 *  migrate-empty result equals the default layout, and so collapse/expand can
 *  address them deterministically). */
export const MAIN_SECTION_ID = "rps-main";
export const SCRATCH_SECTION_ID = "rps-scratch";
/** SW-AV-10: the human label rendered on the default main section's title bar. */
export const LIVE_GROUP_LABEL = "Live";

/** Namespaced panel id of a custom right pane (`<space>/panes/<id>.html`):
 *  `pane:<org>:<team>/<id>` for a team pane, `pane:private:<org>/<id>` for a private one. */
export const PANE_PANEL_ID_PREFIX = "pane:";
/** True for a pane panel id. A pane is placed by explicit action and registers
 *  passively, so its section is retained across reloads, team
 *  switches and a missing file: a saved `pane:` placement is never evicted. */
export function isDurablePaneId(id: string): boolean {
  return id.startsWith(PANE_PANEL_ID_PREFIX);
}

/** Scratch's share of the vertical space in the default layout. Matches the
 *  legacy `rightPaneSplitRatio` fallback (0.45 — the bottom/scratch fraction)
 *  so a fresh install reproduces today's split exactly (no regression). */
const DEFAULT_SPLIT_RATIO = 0.45;
/** Relative size handed to a freshly-created section; the renderer normalises
 *  weights across expanded sections, so the exact value is not load-bearing. */
const NEW_SECTION_SIZE = 0.4;
/** Legacy `rightPaneSplitRatio` value at/above which the old UI treated the
 *  main section as "minimized" (mirrors viewer-store IS_MAXIMIZED_THRESHOLD; kept
 *  local so this pure model never imports the store that imports it). */
const LEGACY_MAXIMIZED_THRESHOLD = 0.93;

export interface PanelGroup {
  /** Ordered panel ids; rendered as tabs when length > 1. */
  panelIds: string[];
  /** The visible panel in the group. Always one of `panelIds`. */
  activePanelId: string;
}

export interface RightPaneSection {
  id: string;
  group: PanelGroup;
  collapsed: boolean;
  /** Relative vertical weight; normalised across expanded sections at render. */
  size: number;
  /** SW-AV-10: an optional human GROUP label (SETUP / LIVE) rendered on the
   *  section's title bar so the two-group IA reads itself. Absent ⇒ no label
   *  (e.g. Notes), which is the backward-compatible default for every pre-AV-10
   *  persisted section. Passive — it names the group; the tabs own navigation. */
  label?: string;
}

export interface RightPaneLayout {
  sections: RightPaneSection[];
}

/** A new-section id source. The store passes a monotonic factory; tests pass a
 *  deterministic one. Built-in/default sections use fixed ids, not this. */
export type SectionIdFactory = () => string;

let _seq = 0;
/** Default runtime id source for newly-created sections. */
export const defaultSectionIdFactory: SectionIdFactory = () => `rps-${++_seq}`;

/** Mint a section id that does NOT collide with any id already in `layout`. The
 *  default factory's counter resets on reload, so a fresh `rps-N` can clash with
 *  a persisted section of the same id — re-roll until unique (duplicate section
 *  ids would break React keys, the SortableContext, and id-keyed model ops). */
function freshSectionId(layout: RightPaneLayout, idFactory: SectionIdFactory): string {
  const used = new Set(layout.sections.map((s) => s.id));
  let id = idFactory();
  // A monotonic factory yields a free id within `used.size + 1` tries; cap the
  // loop so a pathological (constant/cyclic) factory can't hang the thread.
  for (let i = 0; used.has(id) && i <= used.size; i++) id = idFactory();
  // Last resort: guarantee uniqueness even if the factory keeps colliding.
  if (used.has(id)) {
    let n = 0;
    while (used.has(`${id}-${n}`)) n++;
    id = `${id}-${n}`;
  }
  return id;
}

/** Legacy persisted right-pane state read on load for back-compat migration. */
export interface LegacyRightPaneState {
  /** An already-migrated layout (preferred when valid). */
  rightPaneLayout?: unknown;
  /** Renamed installed Inbox copies discovered once while hydrating saved state. */
  inboxPanelIds?: readonly string[];
  /** team-keyed (or scalar) main/scratch split = scratch's bottom share. */
  rightPaneSplitRatio?: Record<string, number> | number;
}

// ---------------------------------------------------------------------------
// Construction / migration
// ---------------------------------------------------------------------------

/**
 * The fresh-install layout: the Inbox (the daily home) over Notes. Stable section ids + shape so it is a safe
 * migration target: the legacy view-mode/split migration and every id-addressed op
 * (dnd, collapse, reveal) rely on `sections[0]` = main and `sections[1]` = Notes.
 * Fresh-install only — a PERSISTED layout is restored verbatim (coerce/reconcile keep
 * the user's arrangement, pruning retired panels).
 */
export function defaultRightPaneLayout(): RightPaneLayout {
  return {
    sections: [
      {
        id: MAIN_SECTION_ID,
        label: LIVE_GROUP_LABEL,
        group: {
          panelIds: [INBOX_PANEL_ID],
          activePanelId: INBOX_PANEL_ID,
        },
        collapsed: false,
        size: 1 - DEFAULT_SPLIT_RATIO,
      },
      {
        id: SCRATCH_SECTION_ID,
        group: { panelIds: [SCRATCH_PANEL_ID], activePanelId: SCRATCH_PANEL_ID },
        collapsed: false,
        size: DEFAULT_SPLIT_RATIO,
      },
    ],
  };
}

/** Pick the first entry of a team-keyed record (deterministic by key-insertion
 *  order) — migration runs before team activation, so "the active team's value"
 *  is unavailable; we take a stable first entry and accept the documented loss. */
function firstRecordValue<T>(value: Record<string, T> | T | undefined): T | undefined {
  if (value == null) return undefined;
  if (typeof value === "object") {
    const values = Object.values(value as Record<string, T>);
    return values.length > 0 ? values[0] : undefined;
  }
  return value;
}

/** Convert legacy canonical Inbox placements once, keeping the saved real section
 *  ahead of a transient alias if both were persisted. Surface-only panes are independent. */
export function migrateInboxPlacements(
  layout: RightPaneLayout,
  installed: readonly string[] = [],
): RightPaneLayout {
  const ids = layout.sections.flatMap((section) => section.group.panelIds);
  const canonical = ids.filter(
    (id) => (id.startsWith("pane:") && id.endsWith("/inbox")) || installed.includes(id),
  );
  const legacy = ["weft-board", "pane:inbox"].filter((id) => ids.includes(id));
  // A newly discovered installed copy still owns its saved section, even if a
  // reveal added the fixed host while installation metadata was unavailable.
  const source = canonical[0] ?? (ids.includes(INBOX_PANEL_ID) ? INBOX_PANEL_ID : legacy[0]);
  if (!source) return layout;
  let next = replacePanel(layout, source, INBOX_PANEL_ID);
  for (const id of [...canonical, ...legacy]) if (id !== source) next = removePanel(next, id);
  return next;
}

/** Build a `rightPaneLayout` from legacy state: prefer an already-migrated valid
 *  layout; otherwise fold the legacy split into the default. */
export function migrateLegacyRightPaneState(legacy: LegacyRightPaneState): RightPaneLayout {
  if (legacy.rightPaneLayout !== undefined) {
    const coerced = coerceRightPaneLayout(legacy.rightPaneLayout);
    if (coerced) return migrateInboxPlacements(coerced, legacy.inboxPanelIds);
    // malformed → fall through to a fresh default
  }

  const layout = defaultRightPaneLayout();

  const split = firstRecordValue(legacy.rightPaneSplitRatio);
  if (typeof split === "number" && split > 0 && split < 1) {
    if (split >= LEGACY_MAXIMIZED_THRESHOLD) {
      // Legacy "main minimized" (split ≈ 0.95) → collapse the main section
      // instead of restoring it as an unusable sliver (dev-note intent).
      layout.sections[0].collapsed = true;
    } else {
      layout.sections[0].size = 1 - split; // main = complement
      layout.sections[1].size = split; // scratch = the split share
    }
  }

  return ensureExpandedFloor(layout);
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

export interface PanelLocation {
  sectionIndex: number;
  panelIndex: number;
}

/** Locate a panel within the layout, or null if it is not present. */
export function findPanelLocation(layout: RightPaneLayout, panelId: string): PanelLocation | null {
  for (let sectionIndex = 0; sectionIndex < layout.sections.length; sectionIndex++) {
    const panelIndex = layout.sections[sectionIndex].group.panelIds.indexOf(panelId);
    if (panelIndex !== -1) return { sectionIndex, panelIndex };
  }
  return null;
}

/** True when `panelId` is the ACTIVE panel of an EXPANDED section — i.e. actually
 *  visible in the pane (not collapsed to a title bar, not a background tab in a
 *  group, not absent). The inverse is the agent module's "minimized" state
 *  (SW-AW-5): registered/promotable but not currently surfaced. */
export function isPanelVisible(layout: RightPaneLayout, panelId: string): boolean {
  return layout.sections.some((s) => !s.collapsed && s.group.activePanelId === panelId);
}

/** Number of expanded (non-collapsed) sections — the floor invariant guards
 *  this from reaching 0 while the pane is open. */
export function expandedSectionCount(layout: RightPaneLayout): number {
  return layout.sections.filter((s) => !s.collapsed).length;
}

/** Re-assert the at-least-one-expanded floor: if the layout has sections but
 *  none is expanded, expand the first. Applied after any op that could drop the
 *  last expanded section (removePanel) and to loaded/coerced state — so the pane
 *  is never open with only collapsed title bars. Mutates `layout` in place. */
function ensureExpandedFloor(layout: RightPaneLayout): RightPaneLayout {
  if (layout.sections.length > 0 && !layout.sections.some((s) => !s.collapsed)) {
    layout.sections[0].collapsed = false;
  }
  return layout;
}

// ---------------------------------------------------------------------------
// Mutations (pure — return a new layout)
// ---------------------------------------------------------------------------

function cloneSection(section: RightPaneSection): RightPaneSection {
  return {
    ...section,
    group: { panelIds: [...section.group.panelIds], activePanelId: section.group.activePanelId },
  };
}

function cloneLayout(layout: RightPaneLayout): RightPaneLayout {
  return { sections: layout.sections.map(cloneSection) };
}

/** Return a new layout with `mutate` applied to the matching section's clone
 *  (all sections are cloned so the result is a deep-enough copy). The shared
 *  shape behind the single-field section setters. */
function updateSection(
  layout: RightPaneLayout,
  sectionId: string,
  mutate: (s: RightPaneSection) => void,
): RightPaneLayout {
  return {
    sections: layout.sections.map((s) => {
      const clone = cloneSection(s);
      if (clone.id === sectionId) mutate(clone);
      return clone;
    }),
  };
}

/** After a panel is spliced out of a group, point `activePanelId` at the
 *  clamped neighbor if the removed panel was active. Mutates `group`. */
function reassignActive(group: PanelGroup, removedId: string, removedIndex: number): void {
  if (group.activePanelId === removedId && group.panelIds.length > 0) {
    group.activePanelId = group.panelIds[Math.min(removedIndex, group.panelIds.length - 1)];
  }
}

/** Remove a panel from wherever it lives; drop a section that empties. Returns
 *  the new layout (panel may have been absent — then unchanged). */
export function removePanel(layout: RightPaneLayout, panelId: string): RightPaneLayout {
  const next = cloneLayout(layout);
  for (let i = 0; i < next.sections.length; i++) {
    const group = next.sections[i].group;
    const idx = group.panelIds.indexOf(panelId);
    if (idx === -1) continue;
    group.panelIds.splice(idx, 1);
    if (group.panelIds.length === 0) {
      next.sections.splice(i, 1);
    } else {
      reassignActive(group, panelId, idx);
    }
    break;
  }
  // Dropping a section can leave every survivor collapsed → re-assert the floor.
  return ensureExpandedFloor(next);
}

/** Insert `panelId` as a new top section and collapse the others (the seam a
 *  consumer like SW-GS-10 calls to place the agent without evicting anything). */
export function addPanelAsTopSection(
  layout: RightPaneLayout,
  panelId: string,
  idFactory: SectionIdFactory = defaultSectionIdFactory,
): RightPaneLayout {
  const without = removePanel(layout, panelId);
  const others = without.sections.map((s) => ({ ...cloneSection(s), collapsed: true }));
  const top: RightPaneSection = {
    id: freshSectionId(without, idFactory),
    group: { panelIds: [panelId], activePanelId: panelId },
    collapsed: false,
    size: NEW_SECTION_SIZE,
  };
  return { sections: [top, ...others] };
}

/** Reveal a built-in panel in the pane: if absent, add it as a new top section;
 *  if present, activate its tab and expand its (possibly-collapsed) section. The
 *  shared "make this panel visible" op behind every `reveal*` store action
 *  (Notes, panes) — pure, so the visibility rule is unit-tested
 *  once here rather than re-derived per caller. No-op-safe on an already-visible
 *  panel (setActivePanel/expandSection both idempotent). */
export function revealPanel(layout: RightPaneLayout, panelId: string): RightPaneLayout {
  const loc = findPanelLocation(layout, panelId);
  if (!loc) return addPanelAsTopSection(layout, panelId);
  const sectionId = layout.sections[loc.sectionIndex].id;
  return expandSection(setActivePanel(layout, sectionId, panelId), sectionId);
}

/** Replace a surface in its existing section, preserving its size, tab and collapse state. */
export function replacePanel(layout: RightPaneLayout, from: string, to: string): RightPaneLayout {
  if (from === to || !findPanelLocation(layout, from)) return layout;
  const withoutDuplicate = findPanelLocation(layout, to) ? removePanel(layout, to) : layout;
  return {
    sections: withoutDuplicate.sections.map((section) => ({
      ...section,
      group: {
        panelIds: section.group.panelIds.map((id) => (id === from ? to : id)),
        activePanelId: section.group.activePanelId === from ? to : section.group.activePanelId,
      },
    })),
  };
}

/** Project a PERSISTED layout to what is currently RENDERABLE, without mutating
 *  storage. This is where dead-section prevention lives now that reconcile retains
 *  durable `pane:` sections: a group member whose panel is not registered (a pane
 *  this team's chain doesn't show, or one still mid-async-registration) is hidden; a section whose ENTIRE
 *  group is unregistered is dropped; a hidden stored `activePanelId` falls back to
 *  the first renderable member for DISPLAY only (the stored value is never
 *  changed); and the expanded floor is re-asserted over the visible set. Pure and
 *  synchronous (no store, no IO) so it is safe on the render path and unit-tested
 *  in isolation. Returns the SAME layout ref when nothing is filtered, so an
 *  all-registered pane never churns. The caller renders this projection but keeps
 *  WRITING through the raw layout — so a team-absent pane is hidden, never
 *  removed (absent ≠ closed). */
export function selectRenderableLayout(
  layout: RightPaneLayout,
  isPanelRegistered: (id: string) => boolean,
): RightPaneLayout {
  let changed = false;
  const sections: RightPaneSection[] = [];
  for (const s of layout.sections) {
    const panelIds = s.group.panelIds.filter(isPanelRegistered);
    if (panelIds.length === 0) {
      changed = true; // whole group unregistered → drop the section
      continue;
    }
    const activePanelId = panelIds.includes(s.group.activePanelId)
      ? s.group.activePanelId
      : panelIds[0];
    if (panelIds.length === s.group.panelIds.length && activePanelId === s.group.activePanelId) {
      sections.push(s); // unchanged — reuse the ref
    } else {
      changed = true;
      sections.push({ ...s, group: { panelIds, activePanelId } });
    }
  }
  if (!changed) return layout;
  // Re-assert the expanded floor over the VISIBLE set — clone (never mutate the
  // possibly-shared first survivor) if dropping sections left all collapsed.
  if (sections.length > 0 && !sections.some((s) => !s.collapsed)) {
    sections[0] = { ...sections[0], collapsed: false };
  }
  return { sections };
}

/** Collapse a section to its title bar — unless it is the last expanded one
 *  (the floor invariant keeps the pane from going visually empty). */
export function collapseSection(layout: RightPaneLayout, sectionId: string): RightPaneLayout {
  const target = layout.sections.find((s) => s.id === sectionId);
  if (!target || target.collapsed) return cloneLayout(layout);
  if (expandedSectionCount(layout) <= 1) return cloneLayout(layout); // floor: keep ≥1 open
  return updateSection(layout, sectionId, (s) => {
    s.collapsed = true;
  });
}

/** Expand a collapsed section. */
export function expandSection(layout: RightPaneLayout, sectionId: string): RightPaneLayout {
  return updateSection(layout, sectionId, (s) => {
    s.collapsed = false;
  });
}

/** Pull a panel out of its (multi-panel) group into its own new section placed
 *  directly below the source. No-op if the panel is already alone. */
export function splitPanelToOwnSection(
  layout: RightPaneLayout,
  panelId: string,
  idFactory: SectionIdFactory = defaultSectionIdFactory,
): RightPaneLayout {
  const loc = findPanelLocation(layout, panelId);
  if (!loc) return cloneLayout(layout);
  const source = layout.sections[loc.sectionIndex];
  if (source.group.panelIds.length <= 1) return cloneLayout(layout); // already solo

  const next = cloneLayout(layout);
  const srcGroup = next.sections[loc.sectionIndex].group;
  srcGroup.panelIds.splice(loc.panelIndex, 1);
  reassignActive(srcGroup, panelId, loc.panelIndex);
  const newSection: RightPaneSection = {
    id: freshSectionId(next, idFactory),
    group: { panelIds: [panelId], activePanelId: panelId },
    collapsed: false,
    size: NEW_SECTION_SIZE,
  };
  next.sections.splice(loc.sectionIndex + 1, 0, newSection);
  return next;
}

/** Move a panel into the target section's group (as a tab) and make it active;
 *  drop the source section if it empties. No-op if already in the target. */
export function mergePanelIntoSection(
  layout: RightPaneLayout,
  panelId: string,
  targetSectionId: string,
): RightPaneLayout {
  const loc = findPanelLocation(layout, panelId);
  const target = layout.sections.find((s) => s.id === targetSectionId);
  if (!loc || !target) return cloneLayout(layout);
  if (layout.sections[loc.sectionIndex].id === targetSectionId) return cloneLayout(layout);

  const without = removePanel(layout, panelId);
  return {
    sections: without.sections.map((s) => {
      if (s.id !== targetSectionId) return cloneSection(s);
      const clone = cloneSection(s);
      clone.group.panelIds.push(panelId);
      clone.group.activePanelId = panelId;
      return clone;
    }),
  };
}

/** Reorder a section within the stack: pull it out and re-insert it at the
 *  clamped target index. Pure array move — touches no `collapsed`/`size`/`group`
 *  field, so each section keeps its weight + tab state and the expanded-count
 *  (hence the floor) is invariant. No-op for an unknown section id. */
export function moveSection(
  layout: RightPaneLayout,
  sectionId: string,
  toIndex: number,
): RightPaneLayout {
  const from = layout.sections.findIndex((s) => s.id === sectionId);
  if (from === -1) return cloneLayout(layout);
  const next = cloneLayout(layout);
  const [moved] = next.sections.splice(from, 1);
  const clamped = Math.max(0, Math.min(toIndex, next.sections.length));
  next.sections.splice(clamped, 0, moved);
  return next;
}

/** Reorder a panel within its own group (tab order) — pull it out of `panelIds`
 *  and re-insert at the clamped target index (arrayMove semantics, mirroring
 *  `moveSection`). `activePanelId` is untouched. No-op for an unknown panel. */
export function movePanelWithinSection(
  layout: RightPaneLayout,
  panelId: string,
  toIndex: number,
): RightPaneLayout {
  const loc = findPanelLocation(layout, panelId);
  if (!loc) return cloneLayout(layout);
  const next = cloneLayout(layout);
  const ids = next.sections[loc.sectionIndex].group.panelIds;
  const [moved] = ids.splice(loc.panelIndex, 1);
  const clamped = Math.max(0, Math.min(toIndex, ids.length));
  ids.splice(clamped, 0, moved);
  return next;
}

/** Set the relative size (vertical weight) of one section. */
export function setSectionSize(
  layout: RightPaneLayout,
  sectionId: string,
  size: number,
): RightPaneLayout {
  return updateSection(layout, sectionId, (s) => {
    s.size = Math.max(0, size);
  });
}

/** Switch the active panel (tab) of a group. */
export function setActivePanel(
  layout: RightPaneLayout,
  sectionId: string,
  panelId: string,
): RightPaneLayout {
  return updateSection(layout, sectionId, (s) => {
    if (s.group.panelIds.includes(panelId)) s.group.activePanelId = panelId;
  });
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

function isValidSection(value: unknown): value is RightPaneSection {
  if (typeof value !== "object" || value === null) return false;
  const s = value as Record<string, unknown>;
  if (typeof s.id !== "string") return false;
  if (typeof s.collapsed !== "boolean") return false;
  // SW-AV-10: the group label is optional; when present it must be a string.
  if (s.label !== undefined && typeof s.label !== "string") return false;
  // Finite, non-negative weight only — NaN/Infinity/negative would break the
  // flex `var(--rps-size-…)` and the divider's `sum` math.
  if (typeof s.size !== "number" || !Number.isFinite(s.size) || s.size < 0) return false;
  const group = s.group as Record<string, unknown> | undefined;
  if (!group || typeof group !== "object") return false;
  // A group must hold at least one panel, all string ids, and an active id that
  // is actually a member — otherwise the section renders empty + unfixable.
  if (!Array.isArray(group.panelIds) || group.panelIds.length === 0) return false;
  if (!group.panelIds.every((p) => typeof p === "string")) return false;
  if (typeof group.activePanelId !== "string") return false;
  if (!group.panelIds.includes(group.activePanelId)) return false;
  return true;
}

/** Defensively coerce a persisted/untrusted value to a `RightPaneLayout`, or
 *  null if it is not a well-formed, non-empty layout. Rejects a panel id that
 *  appears in more than one section (would mount the same panel twice and be
 *  un-removable) and a section id that appears twice (would break React keys,
 *  the SortableContext, and id-keyed model ops), and re-asserts the
 *  at-least-one-expanded floor. */
export function coerceRightPaneLayout(value: unknown): RightPaneLayout | null {
  if (typeof value !== "object" || value === null) return null;
  const sections = (value as Record<string, unknown>).sections;
  if (!Array.isArray(sections) || sections.length === 0) return null;
  if (!sections.every(isValidSection)) return null;
  const seenPanels = new Set<string>();
  const seenSections = new Set<string>();
  for (const s of sections as RightPaneSection[]) {
    if (seenSections.has(s.id)) return null; // duplicate section id
    seenSections.add(s.id);
    for (const panelId of s.group.panelIds) {
      if (seenPanels.has(panelId)) return null; // duplicate panel across sections
      seenPanels.add(panelId);
    }
  }
  return ensureExpandedFloor({ sections: (sections as RightPaneSection[]).map(cloneSection) });
}

/** The panels that PERSIST across restarts — the built-ins registered at module
 *  load (`registerBuiltinPanels`), always present. KEEP IN SYNC with
 *  `registerBuiltinPanels` (builtin-panels.tsx): a built-in missing here would be
 *  pruned from the restored layout on every load. Retired panel ids (e.g. the old
 *  PR review panes) are pruned on restore. */
export const BUILTIN_PANEL_IDS: ReadonlySet<string> = new Set([SCRATCH_PANEL_ID, INBOX_PANEL_ID]);

/** SW-AW-5: prune every non-built-in panel from a loaded layout. A DYNAMIC panel
 *  (e.g. the agent module's, registered only while its feature is engaged) re-adds
 *  its own section via `addPanelAsTopSection` when it re-engages — so a persisted
 *  layout must NOT restore its section, or the next boot renders a dead,
 *  unregistered section (blank body, raw-id title) and can strand the built-ins
 *  collapsed beneath it. Applied on load, after migrate/coerce. `removePanel` drops
 *  emptied sections and re-asserts the expanded floor; if pruning somehow empties
 *  the whole layout (unreachable today — `addPanelAsTopSection` always co-persists
 *  the built-ins), fall back to the default rather than an empty pane. */
export function reconcileLayoutToBuiltins(layout: RightPaneLayout): RightPaneLayout {
  const foreignIds = layout.sections
    .flatMap((s) => s.group.panelIds)
    .filter((id) => {
      // A durable `pane:` section is NOT foreign — retain it so full arrangement
      // survives a reload (the render-time `selectRenderableLayout` filter, not
      // this prune, hides an unregistered pane). Any other unknown id — e.g. a
      // retired panel kind a stale layout still names — is foreign and dropped
      // with its section.
      if (isDurablePaneId(id)) return false;
      return !BUILTIN_PANEL_IDS.has(id);
    });
  if (foreignIds.length === 0) return layout;
  let next = layout;
  for (const id of foreignIds) next = removePanel(next, id);
  if (next.sections.length > 0) return next;
  return defaultRightPaneLayout();
}

/**
 * The splitter that belongs BELOW section `i`: it resizes the nearest expanded
 * section at or above `i` against the nearest expanded section at or below
 * `i + 1`. Collapsed sections in between are fixed-height headers that simply
 * ride along, so every boundary between two expanded regions stays draggable —
 * a collapsed section no longer severs the resize. `null` when either side has
 * no expanded section (nothing to trade space with).
 */
export function resizePairAt(
  sections: RightPaneSection[],
  i: number,
): { above: RightPaneSection; below: RightPaneSection } | null {
  if (i >= sections.length - 1) return null;
  let above: RightPaneSection | undefined;
  for (let a = i; a >= 0; a--) {
    if (!sections[a].collapsed) {
      above = sections[a];
      break;
    }
  }
  const below = sections.slice(i + 1).find((s) => !s.collapsed);
  return above && below ? { above, below } : null;
}
