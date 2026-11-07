// SPDX-License-Identifier: MIT
// SW-HB-6 — the ⌘⇧P command-palette index (pure core).
//
// This module is the testable heart of the palette: it maps local, on-disk
// objects (teams / source repos / epic-files) and setup actions onto the
// generic `QuickPickerItem` shape the incumbent `QuickOpenDialog` renders, and
// resolves a picked item back to a typed `CommandPaletteSelection` the mount
// dispatches. It reaches NO store and issues NO I/O — the mount injects live
// data and owns the async epic-file walk (SW-HB-6 AC-6, on-disk-only).
//
// Reuse (SW-HB-6 AC-7): the palette is built on `QuickOpenDialog` +
// `QuickPickerItem` + `fuzzyScore`, exactly like ⌘P / ⌘⇧K / ⌘O. The id-encode
// pattern mirrors `workspace-picker-items.ts` (a NUL separator that never
// appears in a name/path); the dialog carries the dispatch kind in `id`.

import type { QuickPickerItem } from "@/features/viewer/components/quick-open-dialog";
import { basename } from "@/shared/lib/basename";
import type { Team } from "@/shared/types/domain";

// NUL — never appears in ids/paths; a safe field separator (mirrors
// `workspace-picker-items.ts`). Written as an explicit escape so it stays
// visible in source/diffs.
const SEP = "\u0000";

/** Actions stay grouped above objects during search; objects inter-rank by
 *  match score. Empty-query order comes from insertion: recent files, actions,
 *  then objects. Exact score ties retain that insertion order. */
export const CP_TIER = {
  action: 1,
  object: 2,
} as const;

/** A typed intent a palette row dispatches. Encoded into the row `id`; decoded
 *  on select. The mount routes each through an EXISTING handler (SW-HB-6 AC-5 —
 *  the palette wires intents, it does not reimplement flows). */
export type CommandPaletteSelection =
  | { kind: "team"; teamName: string }
  | { kind: "repo"; teamName: string; owner: string; repo: string }
  | { kind: "epic"; teamName: string; filePath: string }
  | { kind: "account"; userId: number }
  | { kind: "create-team" }
  | { kind: "attach-repo" }
  | { kind: "open-pane"; panelId: string }
  | { kind: "command"; actionId: string }
  | { kind: "open-shortcuts" }
  | { kind: "open-routines" }
  | { kind: "open-agents" }
  | { kind: "open-scratch-space" }
  | { kind: "add-machine" };

export function encodeSelection(sel: CommandPaletteSelection): string {
  switch (sel.kind) {
    case "team":
      return `team${SEP}${sel.teamName}`;
    case "repo":
      return `repo${SEP}${sel.teamName}${SEP}${sel.owner}/${sel.repo}`;
    case "epic":
      return `epic${SEP}${sel.teamName}${SEP}${sel.filePath}`;
    case "account":
      return `account${SEP}${sel.userId}`;
    case "create-team":
      return "create-team";
    case "attach-repo":
      return "attach-repo";
    case "open-pane":
      return `pane${SEP}${sel.panelId}`;
    case "command":
      return `command${SEP}${sel.actionId}`;
    case "open-shortcuts":
      return "open-shortcuts";
    case "open-routines":
      return "open-routines";
    case "open-agents":
      return "open-agents";
    case "open-scratch-space":
      return "open-scratch-space";
    case "add-machine":
      return "add-machine";
  }
}

export function decodeSelection(id: string): CommandPaletteSelection | null {
  if (id === "create-team") return { kind: "create-team" };
  if (id === "attach-repo") return { kind: "attach-repo" };
  if (id === "open-shortcuts") return { kind: "open-shortcuts" };
  if (id === "open-routines") return { kind: "open-routines" };
  if (id === "open-agents") return { kind: "open-agents" };
  if (id === "open-scratch-space") return { kind: "open-scratch-space" };
  if (id === "add-machine") return { kind: "add-machine" };
  const p = id.split(SEP);
  if (p[0] === "team" && p[1] != null) return { kind: "team", teamName: p[1] };
  if (p[0] === "repo" && p[1] != null && p[2] != null) {
    const slash = p[2].indexOf("/");
    if (slash <= 0) return null;
    return {
      kind: "repo",
      teamName: p[1],
      owner: p[2].slice(0, slash),
      repo: p[2].slice(slash + 1),
    };
  }
  if (p[0] === "epic" && p[1] != null && p[2] != null) {
    return { kind: "epic", teamName: p[1], filePath: p[2] };
  }
  if (p[0] === "pane" && p[1] != null) return { kind: "open-pane", panelId: p[1] };
  if (p[0] === "command" && p[1] != null) return { kind: "command", actionId: p[1] };
  if (p[0] === "account" && p[1] != null) {
    const userId = Number(p[1]);
    if (!Number.isFinite(userId)) return null;
    return { kind: "account", userId };
  }
  return null;
}

/** Deduped source-repo count for a team — two configs of the same owner/repo
 *  count once, matching the teams-panel dedupe. */
function repoCount(team: Team): number {
  return new Set(team.source_repos.map((r) => `${r.owner}/${r.repo}`.toLowerCase())).size;
}

/** A signed-in, INACTIVE GitHub account offered as a switch target. The active
 *  account is filtered out upstream (switching to it is a no-op). */
export interface SwitchableAccount {
  userId: number;
  label: string;
}

/** A recently-opened file (session MRU, SW-HB-6 R2) tagged with its team. */
export interface RecentEpicEntry {
  teamName: string;
  fullPath: string;
  relativePath: string;
}

/** One context file under a team's content tree (`listContextFiles` output +
 *  its owning team). */
export interface EpicFileEntry {
  teamName: string;
  name: string;
  relativePath: string;
  fullPath: string;
}

export interface StaticIndexInputs {
  /** The active org's FULL team list — `org.teams`, NOT the curated
   *  `visibleTeams` (SW-HB-6 G1/AC-2: curation must not hide a team from the
   *  palette). */
  teams: Team[];
  /** Inactive accounts to offer as switch targets (SW-HB-6 AC-5). */
  accounts: SwitchableAccount[];
  /** Session-recent files, most-recent first (SW-HB-6 AC-4/R2). */
  recentEpics: RecentEpicEntry[];
  /** Whether an attach target exists — the generic "Attach a source repo…"
   *  action is only offered when the org has at least one team to attach into. */
  canAttach: boolean;
  /** SW-RM-UI-1: the machine registry is available, so "Add machine…" can run —
   *  a palette command renders nothing until searched, so it adds no friction. */
  canAddMachine: boolean;
  /** How many of the user's routines need them; shown on the Routines row above 0. */
  routinesNeedingYou: number;
  /** The custom right panes the active team sees, offered as "Open pane: …" (empty
   *  outside a team view). */
  panes?: PalettePane[];
  /** Active slot-dashboard commands, ranked with the other actions. */
  commands?: PaletteCommand[];
}

/** An active slot-dashboard command the palette can run. */
export interface PaletteCommand {
  /** The namespaced registry action id. */
  actionId: string;
  label: string;
  /** The owning dashboard's name; search matches it, so "inbox" lists every Inbox command. */
  dashboardTitle: string;
  icon?: QuickPickerItem["icon"];
  /** Its current chord for display; empty when it has none. */
  chord: string;
}

/** A custom right pane the palette can open. */
export interface PalettePane {
  panelId: string;
  /** The panel title (with its layer cue). */
  title: string;
  /** Where it lives: a team name or "Private". */
  layer: string;
}

/**
 * Build every SYNCHRONOUS palette row — recent epic-files, setup actions, and
 * the team / source-repo objects — all from in-memory state (SW-HB-6 AC-6, no
 * I/O). Epic-file rows are appended asynchronously by the mount (see
 * {@link buildEpicFileItems}); returning the sync rows first is what lets the
 * palette open INSTANTLY (SW-HB-6 G4, non-blocking).
 */
export function buildStaticItems(inputs: StaticIndexInputs): QuickPickerItem[] {
  const items: QuickPickerItem[] = [];

  // Recent epic-files are inserted first for an empty query. Reuses the viewer's
  // existing recent-paths MRU rather than a new tracker (SW-HB-6 R2). (That MRU
  // is in the viewer-store persist allowlist, so recent rows survive a restart —
  // a bonus; a stale entry for a since-deleted file just opens an empty tab, the
  // same as any other stale viewer tab.)
  for (const r of inputs.recentEpics) {
    items.push(epicFileItem({ ...r, name: basename(r.fullPath) }));
  }

  // Setup actions (tier 1). Leading verb distinguishes them from objects (G3).
  items.push({
    id: encodeSelection({ kind: "create-team" }),
    label: "Create team…",
    sublabel: "action · create a new team",
    tier: CP_TIER.action,
  });
  // Discoverability launcher (OC-17): opens Settings → Shortcuts, where the
  // keyboard-nav axes are listed with their live bindings. Always offered — the
  // shortcuts surface is global, so it works from the hub and inside a team.
  items.push({
    id: encodeSelection({ kind: "open-shortcuts" }),
    // Leading verb + sentence case, matching the sibling action rows (G3).
    label: "View keyboard shortcuts…",
    sublabel: "action · view & rebind keyboard shortcuts",
    tier: CP_TIER.action,
  });
  // Routines are global (personal and every team's) and live in the Agents tab: this
  // opens Agents → Schedule (SPEC §10). The sublabel carries the words people search by.
  items.push({
    id: encodeSelection({ kind: "open-routines" }),
    label: "Schedule",
    sublabel:
      inputs.routinesNeedingYou > 0
        ? `action · routines, schedule, timers · ${inputs.routinesNeedingYou} need you`
        : "action · routines, schedule, timers",
    tier: CP_TIER.action,
  });
  // Agents are global too (yours and every synced team's).
  items.push({
    id: encodeSelection({ kind: "open-agents" }),
    label: "Agents",
    sublabel: "action · open agents",
    tier: CP_TIER.action,
  });
  // The one place that lists what only you see (SLW-PT-21).
  items.push({
    id: encodeSelection({ kind: "open-scratch-space" }),
    label: "Private",
    sublabel: "action · open what only you see",
    tier: CP_TIER.action,
  });
  if (inputs.canAttach) {
    items.push({
      id: encodeSelection({ kind: "attach-repo" }),
      label: "Attach a source repo…",
      sublabel: "action · add / attach / re-bind a source repo",
      tier: CP_TIER.action,
    });
  }
  for (const acct of inputs.accounts) {
    items.push({
      id: encodeSelection({ kind: "account", userId: acct.userId }),
      label: `Switch to ${acct.label}`,
      sublabel: "action · switch GitHub account",
      tier: CP_TIER.action,
    });
  }
  for (const pane of inputs.panes ?? []) {
    items.push({
      id: encodeSelection({ kind: "open-pane", panelId: pane.panelId }),
      label: `Open pane: ${pane.title}`,
      sublabel: `pane · ${pane.layer}`,
      tier: CP_TIER.action,
    });
  }
  for (const command of inputs.commands ?? []) {
    items.push({
      id: encodeSelection({ kind: "command", actionId: command.actionId }),
      label: command.label,
      sublabel: command.dashboardTitle,
      tier: CP_TIER.action,
      icon: command.icon,
      chord: command.chord || undefined,
    });
  }
  if (inputs.canAddMachine) {
    items.push({
      id: encodeSelection({ kind: "add-machine" }),
      label: "Add machine…",
      sublabel: "action · connect an SSH machine",
      tier: CP_TIER.action,
    });
  }

  // Objects (tier 2 — one tier so teams/repos/epic-files inter-rank by fuzzy).
  for (const team of inputs.teams) {
    const n = repoCount(team);
    items.push({
      id: encodeSelection({ kind: "team", teamName: team.name }),
      label: team.name,
      sublabel: `team · ${n} ${n === 1 ? "repo" : "repos"}`,
      tier: CP_TIER.object,
    });
    // Deduped source repos (AC-3).
    const seen = new Set<string>();
    for (const slug of team.source_repos) {
      const key = `${slug.owner}/${slug.repo}`.toLowerCase();
      if (seen.has(key)) continue;
      seen.add(key);
      items.push({
        id: encodeSelection({
          kind: "repo",
          teamName: team.name,
          owner: slug.owner,
          repo: slug.repo,
        }),
        label: `${slug.owner}/${slug.repo}`,
        sublabel: `repo · ${team.name}`,
        tier: CP_TIER.object,
      });
    }
  }

  return items;
}

function epicFileItem(e: EpicFileEntry): QuickPickerItem {
  return {
    id: encodeSelection({ kind: "epic", teamName: e.teamName, filePath: e.fullPath }),
    label: e.name,
    sublabel: `${e.teamName} · ${e.relativePath}`,
    tier: CP_TIER.object,
  };
}

/**
 * Build the ASYNC epic-file rows (SW-HB-6 AC-4). `exclude` carries the ids
 * already present as recent rows so a recently-opened epic-file isn't listed
 * twice. Duplicate ids within the walk (same path surfaced by two content
 * roots) are also collapsed.
 */
export function buildEpicFileItems(
  entries: EpicFileEntry[],
  exclude: ReadonlySet<string>,
): QuickPickerItem[] {
  const items: QuickPickerItem[] = [];
  const seen = new Set(exclude);
  for (const e of entries) {
    const id = encodeSelection({ kind: "epic", teamName: e.teamName, filePath: e.fullPath });
    if (seen.has(id)) continue;
    seen.add(id);
    items.push(epicFileItem(e));
  }
  return items;
}

/** Handlers the mount supplies — each is an EXISTING app seam (SW-HB-6 AC-5). */
export interface CommandPaletteHandlers {
  openTeam: (teamName: string) => void;
  attachRepo: (teamName: string) => void;
  openEpicFile: (teamName: string, filePath: string) => void;
  switchAccount: (userId: number) => void;
  createTeam: () => void;
  openPane: (panelId: string) => void;
  runCommand: (actionId: string) => void;
  openShortcuts: () => void;
  openRoutines: () => void;
  openAgents: () => void;
  openScratchSpace: () => void;
  addMachine: () => void;
}

/**
 * Decode a picked row id and route it to the matching handler. Returns whether
 * a handler ran (false for an unrecognized id, e.g. a stale encoding). The
 * "attach-repo" generic action needs a team to target; the mount injects the
 * resolved team via `attachTargetTeam` (active team, else first team), and the
 * action no-ops when none exists.
 */
export function dispatchSelection(
  id: string,
  handlers: CommandPaletteHandlers,
  attachTargetTeam: string | null,
): boolean {
  const sel = decodeSelection(id);
  if (sel === null) return false;
  switch (sel.kind) {
    case "team":
      handlers.openTeam(sel.teamName);
      return true;
    case "repo":
      handlers.attachRepo(sel.teamName);
      return true;
    case "epic":
      handlers.openEpicFile(sel.teamName, sel.filePath);
      return true;
    case "account":
      handlers.switchAccount(sel.userId);
      return true;
    case "create-team":
      handlers.createTeam();
      return true;
    case "attach-repo":
      if (attachTargetTeam === null) return false;
      handlers.attachRepo(attachTargetTeam);
      return true;
    case "open-pane":
      handlers.openPane(sel.panelId);
      return true;
    case "command":
      handlers.runCommand(sel.actionId);
      return true;
    case "open-shortcuts":
      handlers.openShortcuts();
      return true;
    case "open-routines":
      handlers.openRoutines();
      return true;
    case "open-agents":
      handlers.openAgents();
      return true;
    case "open-scratch-space":
      handlers.openScratchSpace();
      return true;
    case "add-machine":
      handlers.addMachine();
      return true;
  }
}
