// SPDX-License-Identifier: MIT
import {
  Square,
  UserCog,
  X,
  RotateCw,
  PanelTop,
  FileCode,
  Pencil,
  Trash2,
  FilePlus2,
} from "lucide-react";
import type { HostMenuItem, HostMenuRun } from "./host-menu-context";

export const BUILTIN_MENU_ITEMS = {
  "builtin:rename": { label: "Rename…", icon: Pencil },
  "builtin:delete": { label: "Delete", icon: Trash2, variant: "destructive" as const },
  "builtin:new-file": { label: "New file…", icon: FilePlus2 },
  "builtin:reload-pane": { label: "Reload pane", icon: RotateCw },
  "builtin:open-as-tab": { label: "Open as tab", icon: PanelTop },
  "builtin:edit-source": { label: "Edit source", icon: FileCode },
  "builtin:stop": { label: "Stop…", icon: Square, variant: "destructive" as const },
  "builtin:close-tab": { label: "Close tab…", icon: X },
  "builtin:set-role": { label: "Role", icon: UserCog },
};
export const DASHBOARD_MENU_ITEMS = [
  "builtin:reload-pane",
  "builtin:open-as-tab",
  { id: "source-separator", separator: true },
  "builtin:edit-source",
];
export function dashboardMenuAction(
  id: string,
  dashboard: import("./host-menu-context").HostMenuRequest["dashboard"],
) {
  if (id === "builtin:reload-pane") return dashboard?.reload;
  if (id === "builtin:open-as-tab") return dashboard?.openAsTab;
  if (id === "builtin:edit-source") return dashboard?.editSource;
}
export function isDashboardMenuId(id: string) {
  return ["builtin:reload-pane", "builtin:open-as-tab", "builtin:edit-source"].includes(id);
}
export type BuiltinMenuId = keyof typeof BUILTIN_MENU_ITEMS;
export function builtinMenuId(id: string): BuiltinMenuId | null {
  if (!id.startsWith("builtin:")) return null;
  if (!Object.prototype.hasOwnProperty.call(BUILTIN_MENU_ITEMS, id))
    throw new Error(`unknown built-in "${id}"`);
  return id as BuiltinMenuId;
}
export function validateMenuItems(items: Array<HostMenuItem | string>, grouped = false): void {
  for (const item of items) {
    const value = typeof item === "string" ? { id: item } : item;
    const builtin = builtinMenuId(value.id);
    if (builtin && (grouped || value.radioGroup))
      throw new Error(`built-in "${value.id}" cannot be used in grouped choices`);
    if (!builtin) {
      if (value.submenu) validateMenuItems(value.submenu);
      if (value.segmented) validateMenuItems(value.segmented, true);
    }
  }
}
export function builtinDisabledReason(
  id: BuiltinMenuId,
  target: { row?: { alive: boolean } | null; offlineMachine?: string | null; tab?: unknown } | null,
): string | undefined {
  if (
    isDashboardMenuId(id) ||
    id === "builtin:rename" ||
    id === "builtin:delete" ||
    id === "builtin:new-file"
  )
    return;
  if (!target?.row) return "This session is unavailable.";
  if (id === "builtin:stop") {
    if (target.offlineMachine)
      return `${target.offlineMachine} is offline: it can be stopped once it reconnects.`;
    if (!target.row.alive) return "This session has ended.";
  }
  if (id === "builtin:close-tab" && !target.tab) return "This session has no terminal tab.";
}
export function runBuiltinMenuItem(
  id: "builtin:stop" | "builtin:close-tab",
  target: { row: { handle: string }; tab: { tab: { id: string } } | null },
  run: HostMenuRun,
): Promise<unknown> {
  return id === "builtin:stop"
    ? run("stopSession", { handle: target.row.handle }, { confirmInHost: true })
    : run("closeTerminalTab", { handle: target.tab!.tab.id }, { confirmInHost: true });
}
