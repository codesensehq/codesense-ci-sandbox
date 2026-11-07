// SPDX-License-Identifier: MIT
import { create } from "zustand";

import type { SectionKey } from "@/features/settings/lib/types";

/**
 * Ephemeral UI state for app-wide dialogs and panels. Not persisted —
 * dialog open-state does not survive a reload. Centralizing these flags
 * here keeps the status bar, keyboard-shortcut handler, and the dialogs
 * themselves decoupled (no prop drilling, no lifted state in App.tsx).
 *
 * Mutual exclusion: at most one switcher surface is visibly open at any
 * time. Setting any flag to its "open" value closes the others so two
 * Radix focus traps never stack. The siblings clear silently; components
 * react via normal selector subscriptions.
 */

/** T.7: target identifying which team's source-repo list to attach into.
 *  When non-null the lifted `AttachRepoDialog` is open and the dialog
 *  uses these values instead of falling back to active workspace state. */
export interface AttachRepoTarget {
  orgSlug: string;
  teamName: string;
}

/** SW-RM-UI-1: the machine reserve panel open in the Connections tab (null =
 *  none). Store-held so the picker / palette / canvas can open the tab straight
 *  onto it. `setUp` covers Set up / Reconnect / Edit address / Install (the same
 *  panel, prefilled). */
export type MachinesPanel =
  | { mode: "add" }
  | { mode: "setUp"; machineId: string; privateAccount?: boolean }
  | { mode: "reviewKey"; machineId: string };

interface UiState {
  /** SW-OC-28: the title bar's identity menu, opened by a click (focus lands on the
   *  first section). The two flags below open the SAME menu with focus in their
   *  section — the switch shortcuts set them. */
  identityMenuOpen: boolean;
  /** The identity menu, open at its Context repo section (`nav.switch-org`). */
  orgSwitcherOpen: boolean;
  /** The identity menu, open at its Team section (`nav.switch-team`). */
  teamSwitcherOpen: boolean;
  machinesPanel: MachinesPanel | null;
  /** SW-UX (app-flow redesign): how the team-management tab (`virtual://manage-teams`)
   *  should open — `"create"` focuses the new-team field, `"list"` just focuses it.
   *  `null` once the tab has consumed it. Not a dialog flag (the tab lives in the
   *  viewer); mirrors `sourceReposIntent`. */
  manageTeamsIntent: "list" | "create" | null;
  /** SW-FU-B7: the management tabs (`virtual://…` paths) open while NO team is
   *  active — the first-run / failed-join / org-without-teams floor. A team's tabs
   *  live in the viewer store keyed by its teamKey; with no team there is no such
   *  bucket, so the minimal no-team tab host renders these instead. Session-only. */
  noTeamTabs: string[];
  /** SW-FU-B7: the active no-team tab, or `null` to show the no-team empty state. */
  noTeamActiveTab: string | null;
  /** SW-EW-33: the workspace quick-open/switch palette (Cmd+K, re-keyed SW-FX-49). Global so
   *  the registry shortcut action can open it; the active team's
   *  `TeamWorkspaceView` renders the dialog (its items are team-scoped). */
  workspacePaletteOpen: boolean;
  /** The file quick-open (Cmd+P). Global so the registry shortcut opens it; the active
   *  team's `TeamWorkspaceView` renders the dialog (its files are team-scoped). */
  fileQuickOpenOpen: boolean;
  /** SW-SN-3: the note quick-switcher (Cmd+O) — the keyboard accelerator over
   *  the SN-2 rail's list. Global so the registry shortcut can open it; the
   *  active team's `TeamWorkspaceView` renders the dialog (notes are global, but
   *  the mount is gated to the active view like the workspace palette). */
  noteSwitcherOpen: boolean;
  /** SW-SN-6: the go-to-heading fuzzy jump (Cmd+Shift+O) — the keyboard sibling
   *  of the note switcher, scoped to the active note's headings. Global so the
   *  registry shortcut can open it; the active team's `TeamWorkspaceView` renders
   *  the dialog (mirrors `noteSwitcherOpen`). */
  headingJumpOpen: boolean;
  /** SW-HB-6: the ⌘K command palette over local objects (teams / source repos /
   *  epic-files) + setup actions. Global AND mounted app-globally (unlike the
   *  team-scoped palettes above) because its registry action is `scope: "global"`
   *  — it must open from the Home Base hub as well as inside a team. */
  commandPaletteOpen: boolean;
  settingsPanelOpen: boolean;
  settingsInitialSection: SectionKey | null;
  /** A Settings row to scroll to and focus once it shows (`SettingRow`'s `target`), then cleared. */
  settingsInitialRow: string | null;
  /** Counts Settings opens, so a repeat request for the same section still jumps. */
  settingsJump: number;
  /** SW-EW-34 v2: the title-bar slot element the (single) `WorkspaceTray` is
   *  portaled into when `layoutMode === "console"` — the ONE tray stays in the
   *  terminal view's `DndContext` (drag-to-split intact) while visually
   *  occupying the top chrome. `null` when no title bar is mounted / not
   *  console. Deliberately NOT in CLOSED_SIBLINGS — opening a switcher dialog
   *  must not clear the slot. */
  consoleSwitcherSlot: HTMLElement | null;
  /** SW-UX (app-flow redesign): how the source-repos tab (`virtual://source-repos`)
   *  should open when an entry point requests it — `"add"` expands the add-repo
   *  reserve, `"list"` just focuses the list. `null` once the tab has consumed it.
   *  Not a dialog flag: the tab itself lives in the viewer, keyed by its sentinel
   *  path; this only seeds its reserve state. */
  sourceReposIntent: "list" | "add" | null;
  /** SW-UX (app-flow redesign): which screen the Workspaces tab
   *  (`virtual://workspaces`) should open to — the OrgPicker's home / create / join.
   *  Seeds the tab's initial screen (mirrors `sourceReposIntent`). */
  workspacesScreen: "home" | "create" | "join" | null;
  /** SW-LNS-1: monotonic nonce bumped by `requestFocusLeftNavSearch`. The active
   *  team's `PipelineStatusNav` subscribes and re-focuses its search input each
   *  time it changes — a counter (not a boolean) so repeated hotkey presses
   *  re-focus even when the handle hasn't lost focus in between. */
  leftNavSearchFocusNonce: number;

  setIdentityMenuOpen: (open: boolean) => void;
  /** Close the identity menu, whichever section it was opened at. */
  closeIdentityMenu: () => void;
  setOrgSwitcherOpen: (open: boolean) => void;
  toggleOrgSwitcher: () => void;
  setTeamSwitcherOpen: (open: boolean) => void;
  toggleTeamSwitcher: () => void;
  setMachinesPanel: (panel: MachinesPanel | null) => void;
  setManageTeamsIntent: (intent: "list" | "create" | null) => void;
  /** SW-FU-B7: open (or focus) `path` in the no-team tab host. */
  openNoTeamTab: (path: string) => void;
  /** SW-FU-B7: activate an open no-team tab (`null` shows the empty state). */
  setNoTeamActiveTab: (path: string | null) => void;
  /** SW-FU-B7: close a no-team tab; focus moves to its neighbour, else the empty state. */
  closeNoTeamTab: (path: string) => void;
  setWorkspacesScreen: (screen: "home" | "create" | "join" | null) => void;
  setWorkspacePaletteOpen: (open: boolean) => void;
  setFileQuickOpenOpen: (open: boolean) => void;
  setNoteSwitcherOpen: (open: boolean) => void;
  setHeadingJumpOpen: (open: boolean) => void;
  setCommandPaletteOpen: (open: boolean) => void;
  setConsoleSwitcherSlot: (el: HTMLElement | null) => void;
  closeSettings: () => void;
  setSourceReposIntent: (intent: "list" | "add" | null) => void;
  /** SW-LNS-1: bump `leftNavSearchFocusNonce` — the shortcut registry's
   *  `nav.focus-left-search` run() calls this. */
  requestFocusLeftNavSearch: () => void;
}

const CLOSED_SIBLINGS = {
  identityMenuOpen: false,
  orgSwitcherOpen: false,
  teamSwitcherOpen: false,
  workspacePaletteOpen: false,
  fileQuickOpenOpen: false,
  noteSwitcherOpen: false,
  headingJumpOpen: false,
  commandPaletteOpen: false,
  settingsPanelOpen: false,
  settingsInitialSection: null as SectionKey | null,
  settingsInitialRow: null as string | null,
};

export const useUiStore = create<UiState>((set) => ({
  identityMenuOpen: false,
  orgSwitcherOpen: false,
  teamSwitcherOpen: false,
  machinesPanel: null,
  manageTeamsIntent: null,
  noTeamTabs: [],
  noTeamActiveTab: null,
  workspacesScreen: null,
  workspacePaletteOpen: false,
  fileQuickOpenOpen: false,
  noteSwitcherOpen: false,
  headingJumpOpen: false,
  commandPaletteOpen: false,
  settingsPanelOpen: false,
  settingsInitialSection: null,
  settingsInitialRow: null,
  settingsJump: 0,
  consoleSwitcherSlot: null,
  sourceReposIntent: null,
  leftNavSearchFocusNonce: 0,

  setIdentityMenuOpen: (open) =>
    set(open ? { ...CLOSED_SIBLINGS, identityMenuOpen: true } : { identityMenuOpen: false }),
  closeIdentityMenu: () =>
    set({
      identityMenuOpen: false,
      orgSwitcherOpen: false,
      teamSwitcherOpen: false,
    }),

  setOrgSwitcherOpen: (open) =>
    set(open ? { ...CLOSED_SIBLINGS, orgSwitcherOpen: true } : { orgSwitcherOpen: false }),
  toggleOrgSwitcher: () =>
    set((s) =>
      s.orgSwitcherOpen
        ? { orgSwitcherOpen: false }
        : { ...CLOSED_SIBLINGS, orgSwitcherOpen: true },
    ),

  setTeamSwitcherOpen: (open) =>
    set(open ? { ...CLOSED_SIBLINGS, teamSwitcherOpen: true } : { teamSwitcherOpen: false }),
  toggleTeamSwitcher: () =>
    set((s) =>
      s.teamSwitcherOpen
        ? { teamSwitcherOpen: false }
        : { ...CLOSED_SIBLINGS, teamSwitcherOpen: true },
    ),

  setMachinesPanel: (panel) => set({ machinesPanel: panel }),

  setManageTeamsIntent: (intent) => set({ manageTeamsIntent: intent }),
  openNoTeamTab: (path) =>
    set((s) => ({
      noTeamTabs: s.noTeamTabs.includes(path) ? s.noTeamTabs : [...s.noTeamTabs, path],
      noTeamActiveTab: path,
    })),
  setNoTeamActiveTab: (path) =>
    set((s) => ({
      noTeamActiveTab: path !== null && s.noTeamTabs.includes(path) ? path : null,
    })),
  closeNoTeamTab: (path) =>
    set((s) => {
      const idx = s.noTeamTabs.indexOf(path);
      if (idx === -1) return s;
      const noTeamTabs = s.noTeamTabs.filter((p) => p !== path);
      const noTeamActiveTab =
        s.noTeamActiveTab !== path
          ? s.noTeamActiveTab
          : (noTeamTabs[idx] ?? noTeamTabs[idx - 1] ?? null);
      return { noTeamTabs, noTeamActiveTab };
    }),
  setWorkspacesScreen: (screen) => set({ workspacesScreen: screen }),

  setWorkspacePaletteOpen: (open) =>
    set(
      open ? { ...CLOSED_SIBLINGS, workspacePaletteOpen: true } : { workspacePaletteOpen: false },
    ),

  setFileQuickOpenOpen: (open) =>
    set(open ? { ...CLOSED_SIBLINGS, fileQuickOpenOpen: true } : { fileQuickOpenOpen: false }),

  setNoteSwitcherOpen: (open) =>
    set(open ? { ...CLOSED_SIBLINGS, noteSwitcherOpen: true } : { noteSwitcherOpen: false }),

  setHeadingJumpOpen: (open) =>
    set(open ? { ...CLOSED_SIBLINGS, headingJumpOpen: true } : { headingJumpOpen: false }),

  setCommandPaletteOpen: (open) =>
    set(open ? { ...CLOSED_SIBLINGS, commandPaletteOpen: true } : { commandPaletteOpen: false }),

  setConsoleSwitcherSlot: (el) => set({ consoleSwitcherSlot: el }),

  closeSettings: () =>
    set({
      settingsPanelOpen: false,
      settingsInitialSection: null,
      settingsInitialRow: null,
    }),
  setSourceReposIntent: (intent) => set({ sourceReposIntent: intent }),

  requestFocusLeftNavSearch: () =>
    set((s) => ({ leftNavSearchFocusNonce: s.leftNavSearchFocusNonce + 1 })),
}));
