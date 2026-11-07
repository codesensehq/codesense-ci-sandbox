// SPDX-License-Identifier: MIT
import { INBOX_PANEL_ID } from "@/features/shell/right-pane/right-pane-layout";
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";

import { paneOnScreen } from "@/features/shell/right-pane/panes/pane-visibility";
import { usePaneStore } from "@/features/shell/right-pane/panes/pane-store";
import { revealSlot, slotDashboards } from "@/features/shell/slots/reveal-slot";
import type { SlotId } from "@/features/shell/slots/slot-registry";
import { logger } from "@/shared/lib/logger";
import {
  dashboardCommandActionId,
  resolveBindings,
  setDynamicShortcutActions,
  type ShortcutAction,
} from "@/shared/lib/shortcuts/registry";
import { createListenerGroup } from "@/shared/lib/tauri-listeners";
import { useOrgStore } from "@/shared/stores/org-store";
import { useShortcutsStore } from "@/shared/stores/shortcuts-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { isConsoleLayout, isTerminalColumnShown } from "@/features/viewer/lib/terminal-column";
import {
  commandsSync,
  type DashboardCommand,
  type PaneEntry,
  type SlotCommand,
} from "@/tauri/commands";
import { COMMAND_RUN_EVENT, type RunCommandPayload } from "@/tauri/events";

import { useDashboardProfileStore } from "./dashboard-profile";
import { dashboardRefKey } from "./dashboard-ref";
import { hasLiveFrames, isDashboardFrameFocused, postDashboardCommand } from "./frame-lifecycle";

/** Slots whose dashboards own commands, in the order Settings groups them. */
const COMMAND_SLOTS: readonly SlotId[] = ["right-pane", "dock", "file-nav"];

/** Each dashboard's last `commands.register` set; absent means every declared command. */
const activeSets = new Map<string, ReadonlySet<string>>();
interface SlotDashboard {
  slot: SlotId;
  pane: PaneEntry;
  key: string;
}

function slotShown({ slot, pane, key }: SlotDashboard): boolean {
  const viewer = useViewerStore.getState();
  if (slot === "right-pane") {
    return paneOnScreen(pane.panelId, viewer.rightPaneOpen, viewer.rightPaneLayout);
  }
  if (slot === "file-nav") {
    const { activeOrgSlug: org, activeTeamName: team } = useOrgStore.getState();
    return (
      !!org &&
      !!team &&
      !isConsoleLayout(viewer) &&
      !viewer.navCollapsed[`${org}:${team}`] &&
      hasLiveFrames((f) => f.dashboard === key && f.slotId === slot)
    );
  }
  return (
    isTerminalColumnShown(viewer) &&
    !viewer.workspacePanelCollapsed &&
    hasLiveFrames((f) => f.dashboard === key && f.slotId === slot)
  );
}

function scopeMatches(command: DashboardCommand, dashboard: SlotDashboard): boolean {
  switch (command.scope) {
    case "global":
      return true;
    case "slot-visible":
      return slotShown(dashboard);
    case "slot-focused":
      return isDashboardFrameFocused(dashboard.key);
  }
}

/**
 * Deliver a command to its dashboard's page: reveal the slot when it is closed or
 * hidden, then post `codesense:command` once the page has called `ready()`.
 */
function dispatchDashboardCommand(
  dashboard: SlotDashboard,
  command: DashboardCommand,
  args?: unknown,
): void {
  if (
    !slotShown(dashboard) ||
    (dashboard.slot === "dock" &&
      command.scope === "global" &&
      !isDashboardFrameFocused(dashboard.key))
  )
    revealSlot(dashboard.slot, {
      dashboard: dashboard.pane.dashboardRef,
      ...(dashboard.pane.panelId === INBOX_PANEL_ID ? { surface: "inbox" } : {}),
      ...(dashboard.slot === "dock" ? { focus: true } : {}),
    });
  postDashboardCommand(dashboard.key, dashboard.slot, {
    id: command.id,
    ...(dashboard.pane.panelId === INBOX_PANEL_ID ? { panelId: dashboard.pane.panelId } : {}),
    args,
    label: command.label,
    dashboardTitle: dashboard.pane.title,
  });
}

function commandAction(
  id: string,
  dashboard: SlotDashboard,
  command: DashboardCommand,
): ShortcutAction {
  const active = activeSets.get(dashboard.key)?.has(command.id) ?? true;
  return {
    id,
    label: command.label,
    defaultKeystroke: command.defaultChord ?? "",
    scope: "global",
    command: {
      dashboard: dashboard.key,
      dashboardTitle: dashboard.pane.title,
      dashboardScope: dashboard.pane.dashboardRef!.scope,
      icon: command.icon,
      humanOnly: !!command.humanOnly,
      active,
    },
    available: () => active && scopeMatches(command, dashboard),
    run: () => dispatchDashboardCommand(dashboard, command),
  };
}

/** The slot dashboards' commands by namespaced id, as last synced. */
let slotCommands = new Map<string, { dashboard: SlotDashboard; command: DashboardCommand }>();
/** The host registry starts empty, so there's nothing to push until commands exist. */
let pushed = "[]";

/** Tell the host registry the slot commands and their effective chords, so agents'
 *  `command_list` and the page's `commands.list` see what the person sees. */
function pushCommandsToHost(): void {
  if (slotCommands.size === 0 && pushed === "[]") return;
  const bindings = resolveBindings(useShortcutsStore.getState().shortcuts);
  const commands: SlotCommand[] = [...slotCommands].map(([id, { dashboard, command }]) => ({
    id,
    dashboard: dashboard.key,
    dashboardTitle: dashboard.pane.title,
    dashboardScope: dashboard.pane.dashboardRef!.scope,
    slot: dashboard.slot,
    capabilities: dashboard.pane.declaration?.capabilities ?? [],
    command: command.id,
    label: command.label,
    scope: command.scope,
    binding: bindings.get(id)?.keystroke ?? "",
    humanOnly: !!command.humanOnly,
  }));
  const next = JSON.stringify(commands);
  if (next === pushed) return;
  pushed = next;
  commandsSync(commands).catch((e: unknown) =>
    logger.warn("dashboard-commands", `commands_sync failed — ${String(e)}`),
  );
}

/** Run a slot command the host asked for (an agent's `command_run` or a page's
 *  `commands.run`) through the same dispatch as a chord. */
export function runDashboardCommand(id: string, args?: unknown): void {
  const entry = slotCommands.get(id);
  if (entry) dispatchDashboardCommand(entry.dashboard, entry.command, args);
}

let signature = "";

/** Rebuild the registry's dynamic layer from the slot dashboards the profile assigns. */
export function syncDashboardCommands(): void {
  const seen = new Set<string>();
  const dashboards: SlotDashboard[] = [];
  for (const slot of COMMAND_SLOTS) {
    for (const pane of slotDashboards(slot)) {
      const key = dashboardRefKey(pane.dashboardRef!);
      if (seen.has(key) || !pane.declaration?.commands?.length) continue;
      seen.add(key);
      dashboards.push({ slot, pane, key });
    }
  }
  // A dashboard that leaves its slot starts over with every command active.
  for (const key of activeSets.keys()) if (!seen.has(key)) activeSets.delete(key);
  slotCommands = new Map(
    dashboards.flatMap((dashboard) =>
      dashboard.pane.declaration!.commands!.map(
        (command) =>
          [dashboardCommandActionId(dashboard.key, command.id), { dashboard, command }] as const,
      ),
    ),
  );
  const next = JSON.stringify(
    dashboards.map((d) => [
      d.slot,
      d.key,
      d.pane.panelId,
      d.pane.title,
      d.pane.declaration?.commands,
      // Never registered (all active) differs from registered with no ids (none active).
      activeSets.has(d.key) ? [...activeSets.get(d.key)!] : null,
    ]),
  );
  if (next !== signature) {
    signature = next;
    setDynamicShortcutActions(
      [...slotCommands].map(([id, { dashboard, command }]) =>
        commandAction(id, dashboard, command),
      ),
    );
  }
  pushCommandsToHost();
}

/** A dashboard's `commands.register` landed: only these of its commands are active. */
export function setActiveDashboardCommands(dashboard: string, ids: readonly string[]): void {
  activeSets.set(dashboard, new Set(ids));
  syncDashboardCommands();
}

/** Keep dashboard commands in the registry while their dashboards sit in a slot.
 *  Mount once near the app root. */
export function useDashboardCommandsSync(): void {
  useEffect(() => {
    syncDashboardCommands();
    // Only the profile, the listing and the active team decide slot dashboards.
    const unsubscribe = [
      useDashboardProfileStore.subscribe((s, p) => {
        if (s.profile !== p.profile || s.org !== p.org) syncDashboardCommands();
      }),
      usePaneStore.subscribe((s, p) => {
        if (
          s.listing !== p.listing ||
          s.knownEntries !== p.knownEntries ||
          s.teamKey !== p.teamKey ||
          s.sourceRows !== p.sourceRows
        )
          syncDashboardCommands();
      }),
      useOrgStore.subscribe((s, p) => {
        if (s.activeOrgSlug !== p.activeOrgSlug || s.activeTeamName !== p.activeTeamName)
          syncDashboardCommands();
      }),
      // A rebind changes the chord the host lists.
      useShortcutsStore.subscribe((s, p) => {
        if (s.shortcuts !== p.shortcuts) pushCommandsToHost();
      }),
    ];
    const listeners = createListenerGroup();
    listeners.track(
      listen<RunCommandPayload>(COMMAND_RUN_EVENT, ({ payload }) =>
        runDashboardCommand(payload.id, payload.args),
      ),
    );
    return () => {
      unsubscribe.forEach((off) => off());
      listeners.dispose();
    };
  }, []);
}
