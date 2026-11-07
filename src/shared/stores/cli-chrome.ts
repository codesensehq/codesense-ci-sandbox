// SPDX-License-Identifier: MIT
/**
 * The CLI's chrome, saved as one `cliChrome` value for the whole app, whichever team is on
 * screen (D39, G2): shown or hidden, full-screen or a pane beside the viewer, the last terminal
 * layout, the split size, expanded or the grid, and the Agents dock's fold and width. The viewer
 * store holds each field flat (`terminalVisible`, `layoutMode`, `lastTerminalMode`,
 * `terminalSplitRatio`, `cliExpanded`, `workspacePanelCollapsed`, `workspacePanelWidth`). What
 * each team keeps of its own (its focused workspace, tabs, tile order, grid preset, tray mode)
 * stays in the team-keyed slices.
 *
 * Pure: the saved shape, its defaults, a saved value's validation and the one-time seed from the
 * per-team keys a state file saved before the chrome was global.
 */

import { parseSyntheticRepoKey } from "@/shared/lib/build-synthetic-repo";
import { buildTeamKey, parseTeamKey } from "@/shared/lib/team-key";

/** The CLI's layout: full-screen (`console`), a pane under the viewer (`shared`), or folded
 *  away under the viewer (`viewer`). */
export type LayoutMode = "console" | "shared" | "viewer";

export interface CliChrome {
  /** The CLI is on screen. Follows a full-screen or folded layout (see {@link sanitizeCliChrome}). */
  visible: boolean;
  /** Null until the Workbench settles it (`settleInitialLayout`). */
  layoutMode: LayoutMode | null;
  /** The terminal layout a reopen of the folded CLI and a workspace open come back to. */
  lastTerminalMode: "console" | "shared";
  /** The viewer's share of the column above the CLI pane (0.1..0.9). */
  splitRatio: number;
  /** Expanded: the focused workspace's tile fills the CLI. Off, the team's grid shows. */
  expanded: boolean;
  /** The Agents dock beside the CLI is folded away. */
  agentsDockCollapsed: boolean;
  /** The Agents dock's dragged width in px; null for its default. */
  agentsDockWidth: number | null;
}

/** The viewer's share when no split is saved. */
export const DEFAULT_TERMINAL_SPLIT_RATIO = 0.55;

export const DEFAULT_CLI_CHROME: CliChrome = {
  visible: false,
  layoutMode: null,
  lastTerminalMode: "shared",
  splitRatio: DEFAULT_TERMINAL_SPLIT_RATIO,
  expanded: false,
  agentsDockCollapsed: false,
  agentsDockWidth: null,
};

const LAYOUT_MODES: readonly string[] = ["console", "shared", "viewer"];

const isLayoutMode = (v: unknown): v is LayoutMode =>
  typeof v === "string" && LAYOUT_MODES.includes(v);
const clamp = (v: number, min: number, max: number) => Math.min(max, Math.max(min, v));
const isRatio = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v);
const isWidth = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v);
const isRecord = (v: unknown): v is Record<string, unknown> =>
  !!v && typeof v === "object" && !Array.isArray(v);

/**
 * A saved chrome with every field checked: a missing or malformed field loads as its default and
 * the others are kept. Visibility follows a full-screen or folded layout. The reopen mode
 * defaults to a shown console's mode, while an explicit saved or agent-supplied choice is kept.
 */
export function sanitizeCliChrome(raw: unknown): CliChrome {
  const r = isRecord(raw) ? raw : {};
  const layoutMode = isLayoutMode(r.layoutMode) ? r.layoutMode : null;
  return {
    visible: layoutMode === "console" || (layoutMode !== "viewer" && r.visible === true),
    layoutMode,
    lastTerminalMode:
      r.lastTerminalMode === "shared" || r.lastTerminalMode === "console"
        ? r.lastTerminalMode
        : layoutMode === "console"
          ? "console"
          : "shared",
    splitRatio: isRatio(r.splitRatio)
      ? clamp(r.splitRatio, 0.1, 0.9)
      : DEFAULT_TERMINAL_SPLIT_RATIO,
    expanded: r.expanded === true,
    agentsDockCollapsed: r.agentsDockCollapsed === true,
    agentsDockWidth: isWidth(r.agentsDockWidth) ? clamp(r.agentsDockWidth, 236, 400) : null,
  };
}

/** The per-team keys a state file saved the chrome under before it was global, each with the
 *  `cliChrome` field it seeds. Read once by {@link seedCliChrome}, then dropped. */
const LEGACY_CHROME_KEYS = {
  terminalVisible: "visible",
  layoutMode: "layoutMode",
  lastTerminalMode: "lastTerminalMode",
  terminalSplitRatio: "splitRatio",
  workspacePanelCollapsed: "agentsDockCollapsed",
  workspacePanelWidth: "agentsDockWidth",
} as const satisfies Record<string, keyof CliChrome>;

/** Keys of the old per-team chrome that are dropped with no field to seed. */
const RETIRED_KEYS = ["previousSplitRatio", "terminalReady", "cliTeamKey"] as const;

const zoomed = (layout: unknown): boolean =>
  isRecord(layout) && typeof layout.zoomedTileId === "string";

/**
 * Seed `viewer.cliChrome` from the per-team chrome a state file saved before it was global, and
 * drop the old keys (and every saved zoom) so nothing reads them again. The seed team is the one
 * open when the app last closed (`lastTeamKey`) when any old key holds an entry for it, else the
 * first saved team (in `openedTeams` order, then the maps' own) that has one. A field that team
 * never saved takes its default. Expanded is whether that team's layout was zoomed (its flat
 * layout, or in the grouped tray an epic's). A `cliChrome` already saved is left as it is.
 */
export function seedCliChrome(
  viewer: Record<string, unknown>,
  lastTeamKey: string | null | undefined,
  legacyTeamKeyOf?: (key: string) => string | null,
): void {
  const canonicalTeam = (key: string): string | null => {
    const resolved = legacyTeamKeyOf?.(key);
    if (resolved) return resolved;
    const synthetic = parseSyntheticRepoKey(key);
    if (synthetic) return buildTeamKey(synthetic.orgSlug, synthetic.team);
    const { orgSlug, teamName } = parseTeamKey(key);
    return orgSlug && teamName && !teamName.includes(":") ? key : null;
  };
  const teamMap = (raw: unknown): Record<string, unknown> => {
    const result: Record<string, unknown> = {};
    if (isRecord(raw)) {
      for (const [key, value] of Object.entries(raw)) {
        const team = canonicalTeam(key);
        if (team) result[team] = value;
      }
    }
    return result;
  };
  const legacy = Object.keys(LEGACY_CHROME_KEYS) as (keyof typeof LEGACY_CHROME_KEYS)[];
  const maps = legacy.flatMap((key) =>
    isRecord(viewer[key]) ? [[key, teamMap(viewer[key])] as const] : [],
  );
  const flat = teamMap(viewer.foregroundLayout);
  const byRail = teamMap(viewer.foregroundLayoutByRail);
  if (
    !isRecord(viewer.cliChrome) &&
    (maps.length > 0 || Object.keys(flat).length > 0 || Object.keys(byRail).length > 0)
  ) {
    const has = (team: string) =>
      maps.some(([, map]) => Object.prototype.hasOwnProperty.call(map, team)) ||
      zoomed(flat[team]) ||
      (isRecord(byRail[team]) && Object.values(byRail[team]).some(zoomed));
    const opened = Array.isArray(viewer.openedTeams) ? viewer.openedTeams : [];
    const candidates = [
      lastTeamKey,
      ...opened.flatMap((key) => (typeof key === "string" ? [canonicalTeam(key)] : [])),
      ...maps.flatMap(([, map]) => Object.keys(map)),
      ...Object.keys(flat),
      ...Object.keys(byRail),
    ];
    const seed = candidates.find(
      (t): t is string =>
        typeof t === "string" && !!parseTeamKey(t).orgSlug && !!parseTeamKey(t).teamName && has(t),
    );
    const chrome: Record<string, unknown> = {};
    if (seed !== undefined) {
      for (const [key, map] of maps) {
        if (Object.prototype.hasOwnProperty.call(map, seed)) {
          chrome[LEGACY_CHROME_KEYS[key]] = map[seed];
        }
      }
    }
    const expandTeam = seed;
    if (expandTeam !== undefined) {
      const rails = byRail[expandTeam];
      const tray = teamMap(viewer.trayMode)[expandTeam];
      const selected = teamMap(viewer.selectedEpicDirName)[expandTeam];
      const focused = teamMap(viewer.activeWorkspace)[expandTeam];
      // Rail selection was runtime-only in real legacy saves. A zoom belongs
      // to the focused workspace, so recover its rail from that saved identity.
      const activeRail = isRecord(rails)
        ? typeof selected === "string"
          ? rails[selected]
          : typeof focused === "string"
            ? Object.values(rails).find(
                (layout) => isRecord(layout) && layout.zoomedTileId === focused,
              )
            : rails[""]
        : undefined;
      chrome.expanded = tray === "grouped" ? zoomed(activeRail) : zoomed(flat[expandTeam]);
    }
    viewer.cliChrome = chrome;
  }
  for (const key of [...legacy, ...RETIRED_KEYS]) delete viewer[key];
  const unzoom = (layout: unknown) => {
    if (isRecord(layout)) delete layout.zoomedTileId;
  };
  Object.values(flat).forEach(unzoom);
  Object.values(byRail).forEach((rails) => {
    if (isRecord(rails)) Object.values(rails).forEach(unzoom);
  });
}

/** Malformed or unknown fields an MCP patch will ignore. */
export function invalidCliChromePatchFields(patch: unknown): string[] {
  if (!isRecord(patch)) return ["cliChrome"];
  const valid: Record<string, (value: unknown) => boolean> = {
    visible: (v) => typeof v === "boolean",
    layoutMode: isLayoutMode,
    lastTerminalMode: (v) => v === "shared" || v === "console",
    splitRatio: isRatio,
    expanded: (v) => typeof v === "boolean",
    agentsDockCollapsed: (v) => typeof v === "boolean",
    agentsDockWidth: (v) => v === null || isWidth(v),
  };
  return Object.entries(patch)
    .filter(
      ([key, value]) => !Object.prototype.hasOwnProperty.call(valid, key) || !valid[key](value),
    )
    .map(([key]) => key);
}

/**
 * An agent's `set_viewer_state {cliChrome: …}` write, merged field by field into `current` and
 * honoured as asked: the agent decides, CodeSense keeps only the pairing `setLayoutMode` keeps.
 * A layout brings visibility along (full screen and a pane show the CLI, folded hides it; a pane
 * written with `visible: false` stays hidden) and becomes the last terminal layout unless the
 * write names one (a hidden pane keeps it). Visibility written alone is the user's ⌘\: `false`
 * folds the CLI, `true` brings it back in its last terminal layout. A field of the wrong type is
 * ignored.
 */
export function applyCliChromePatch(current: CliChrome, patch: unknown): CliChrome {
  if (!isRecord(patch)) return current;
  const next: CliChrome = { ...current };
  const lastAsked =
    patch.lastTerminalMode === "console" || patch.lastTerminalMode === "shared"
      ? patch.lastTerminalMode
      : null;
  if (isLayoutMode(patch.layoutMode)) {
    const mode = patch.layoutMode;
    next.layoutMode = mode;
    next.visible =
      mode === "shared" && typeof patch.visible === "boolean" ? patch.visible : mode !== "viewer";
    if (lastAsked) next.lastTerminalMode = lastAsked;
    else if (mode === "console" || (mode === "shared" && next.visible))
      next.lastTerminalMode = mode;
  } else {
    if (typeof patch.visible === "boolean") {
      if (!patch.visible) {
        next.layoutMode = "viewer";
        next.visible = false;
      } else {
        if (current.layoutMode === "viewer" || current.layoutMode === null) {
          next.layoutMode = lastAsked ?? current.lastTerminalMode;
        }
        next.visible = true;
        if (!lastAsked && (next.layoutMode === "shared" || next.layoutMode === "console")) {
          next.lastTerminalMode = next.layoutMode;
        }
      }
    }
    if (lastAsked) next.lastTerminalMode = lastAsked;
  }
  if (isRatio(patch.splitRatio)) next.splitRatio = patch.splitRatio;
  if (typeof patch.expanded === "boolean") next.expanded = patch.expanded;
  if (typeof patch.agentsDockCollapsed === "boolean") {
    next.agentsDockCollapsed = patch.agentsDockCollapsed;
  }
  if (patch.agentsDockWidth === null || isWidth(patch.agentsDockWidth)) {
    next.agentsDockWidth = patch.agentsDockWidth;
  }
  return sanitizeCliChrome(next);
}
