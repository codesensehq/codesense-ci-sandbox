// SPDX-License-Identifier: MIT
import type { Workspace } from "@/shared/stores/viewer-store";

export interface WorkspaceGroup {
  id: string;
  name: string;
  workspaceIds: string[];
}
export const CUSTOM_RAIL_PREFIX = " group:";
export const customRailKey = (id: string) => `${CUSTOM_RAIL_PREFIX}${id}`;
export const customGroupId = (key: string) =>
  key.startsWith(CUSTOM_RAIL_PREFIX) ? key.slice(CUSTOM_RAIL_PREFIX.length) : null;

export function validateGroupName(
  groups: readonly WorkspaceGroup[],
  name: string,
  id?: string,
): string | null {
  const trimmed = name.trim();
  if (!trimmed) return "Type a name, or press Esc to cancel.";
  if (Array.from(trimmed).length > 80) return "Keep the name under 80 characters.";
  const duplicate = groups.find(
    (g) => g.id !== id && g.name.toLowerCase() === trimmed.toLowerCase(),
  );
  return duplicate ? `There's already a group called "${duplicate.name}".` : null;
}

/** Keep closed workspace ids: reopening the same story restores its membership. */
export function sanitizeWorkspaceGroups(value: unknown): Record<string, WorkspaceGroup[]> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return Object.fromEntries(
    Object.entries(value).map(([team, entries]) => {
      const groups: WorkspaceGroup[] = [];
      const members = new Set<string>();
      if (Array.isArray(entries))
        for (const entry of entries) {
          if (
            !entry ||
            typeof entry.id !== "string" ||
            !entry.id.trim() ||
            typeof entry.name !== "string" ||
            !Array.isArray(entry.workspaceIds) ||
            groups.some((g) => g.id === entry.id) ||
            validateGroupName(groups, entry.name)
          )
            continue;
          const workspaceIds = entry.workspaceIds.filter((id: unknown): id is string => {
            if (typeof id !== "string" || !id || id.startsWith("root:") || members.has(id))
              return false;
            members.add(id);
            return true;
          });
          groups.push({ id: entry.id, name: entry.name.trim(), workspaceIds });
        }
      return [team, groups];
    }),
  );
}

export type GroupEdit =
  | { action: "create"; name: string; workspaceIds?: string[]; pruneEmpty?: boolean }
  | { action: "rename"; groupId: string; name: string }
  | { action: "set_members"; groupId: string; workspaceIds: string[]; pruneEmpty?: boolean }
  | { action: "add" | "remove"; groupId: string; workspaceId: string; pruneEmpty?: boolean }
  | { action: "delete"; groupId: string }
  | { action: "restore"; group: WorkspaceGroup; index: number; pruneGroupId?: string };
export interface GroupSnapshot {
  teamKey: string;
  groups: WorkspaceGroup[];
  revision: number;
}
export type GroupResult =
  | ({ status: "ok"; group?: WorkspaceGroup } & GroupSnapshot)
  | { status: "validation"; message: string };

export function validateGroupEdit(
  groups: WorkspaceGroup[],
  edit: GroupEdit,
  workspaces: readonly Workspace[],
): string | null {
  if ("groupId" in edit && !groups.some((g) => g.id === edit.groupId)) return "Unknown group.";
  if ("name" in edit) {
    const problem = validateGroupName(
      groups,
      edit.name,
      "groupId" in edit ? edit.groupId : undefined,
    );
    if (problem) return problem;
  }
  if (edit.action === "restore") {
    const problem = validateGroupName(groups, edit.group.name);
    if (problem) return problem;
  }
  const previous =
    "groupId" in edit ? (groups.find((g) => g.id === edit.groupId)?.workspaceIds ?? []) : [];
  const added =
    edit.action === "add"
      ? [edit.workspaceId]
      : "workspaceIds" in edit
        ? (edit.workspaceIds ?? [])
        : [];
  for (const id of added) {
    if (previous.includes(id)) continue;
    const ws = workspaces.find((w) => w.id === id);
    if (!ws) return "Unknown workspace.";
    if (ws.kind === "root") return "Repositories stay in their own group.";
  }
  return null;
}
