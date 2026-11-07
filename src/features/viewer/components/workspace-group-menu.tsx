// SPDX-License-Identifier: MIT
import { useState, type ReactNode } from "react";
import { queryAllShown } from "@/shared/lib/warm-scopes";
import { toast } from "sonner";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import { InlineRename } from "@/shared/components/inline-rename";
import {
  PositionedContextMenu,
  PositionedMenuItem,
  PositionedSubmenu,
  type ContextMenuPosition,
} from "@/shared/components/positioned-context-menu";
import { DropdownMenuRadioGroup, DropdownMenuRadioItem } from "@/components/ui/dropdown-menu";
import {
  customRailKey,
  validateGroupName,
  type GroupEdit,
  type WorkspaceGroup,
} from "../lib/workspace-groups";

/** Both menus use the store's edit boundary, so a remote edit takes the same path. */
export function WorkspaceGroupMenu({
  teamKey,
  workspace,
  groupId,
  position,
  owner,
  onClose,
  children,
  footer,
}: {
  teamKey: string;
  workspace?: Workspace | null;
  groupId?: string;
  position: ContextMenuPosition;
  owner?: object;
  onClose: () => void;
  children?: ReactNode;
  footer?: ReactNode;
}) {
  const groups = useViewerStore((s) => s.customGroupsByTeam[teamKey]);
  const list = groups ?? [];
  const group = list.find(
    (g) => g.id === groupId || (!groupId && workspace && g.workspaceIds.includes(workspace.id)),
  );
  const [naming, setNaming] = useState(false);
  const [busy, setBusy] = useState(false);
  const railMenu = groupId !== undefined;
  const label = railMenu ? "Rename group" : "New group";
  const focusWorkspace = () => {
    if (owner || !workspace) return;
    requestAnimationFrame(() => {
      const chip = queryAllShown("[data-tray-chip]").find(
        (el) => el.dataset.trayChip === workspace.id,
      );
      chip
        ?.querySelector<HTMLButtonElement>("button[data-pill-select]")
        ?.focus({ preventScroll: true });
    });
  };
  const edit = async (input: GroupEdit) => {
    if (busy) return null;
    const source =
      group && ["create", "add", "remove", "delete"].includes(input.action)
        ? {
            group,
            index: list.indexOf(group),
            layout:
              useViewerStore.getState().foregroundLayoutByRail[teamKey]?.[customRailKey(group.id)],
          }
        : null;
    setBusy(true);
    const result = await useViewerStore.getState().editWorkspaceGroup(teamKey, input);
    setBusy(false);
    if (result.status === "validation") return result.message;
    onClose();
    focusWorkspace();
    if (source && !result.groups.some((g) => g.id === source.group.id)) {
      const removed = input.action === "delete" ? (result.group ?? source.group) : source.group;
      undoToast(
        removed,
        source.index,
        source.layout,
        input.action === "delete"
          ? `Deleted "${removed.name}". Its workspaces returned to their epic groups.`
          : `Removed from "${removed.name}". "${removed.name}" had no other workspaces, so it's deleted.`,
        input.action === "create" ? result.group?.id : undefined,
      );
    }
    return null;
  };
  const undoToast = (
    removed: WorkspaceGroup,
    index: number,
    layout: unknown,
    message: string,
    pruneGroupId?: string,
  ) => {
    toast(message, {
      duration: 10000,
      action: {
        label: "Undo",
        onClick: () => {
          void useViewerStore
            .getState()
            .editWorkspaceGroup(teamKey, {
              action: "restore",
              group: removed,
              index,
              ...(pruneGroupId ? { pruneGroupId } : {}),
            })
            .then((result) => {
              if (result.status === "validation") {
                toast.error(result.message);
                return;
              }
              if (layout)
                useViewerStore.setState((s) => ({
                  foregroundLayoutByRail: {
                    ...s.foregroundLayoutByRail,
                    [teamKey]: {
                      ...s.foregroundLayoutByRail[teamKey],
                      [customRailKey(removed.id)]: layout as NonNullable<
                        (typeof s.foregroundLayoutByRail)[string]
                      >[string],
                    },
                  },
                }));
              useViewerStore.getState().persistState();
            });
        },
      },
    });
  };
  const remove = async (deleting: boolean) => {
    if (!group) return;
    const error = await edit(
      deleting
        ? { action: "delete", groupId: group.id }
        : { action: "remove", groupId: group.id, workspaceId: workspace!.id, pruneEmpty: true },
    );
    if (error) toast.error(error);
  };

  const canGroup = workspace && workspace.kind !== "root";
  return (
    <PositionedContextMenu
      owner={owner}
      position={position}
      onClose={onClose}
      role={naming ? "dialog" : "menu"}
      label={naming ? label : railMenu ? "Group actions" : "Workspace actions"}
    >
      {naming ? (
        <div className="p-1">
          <label className="block px-2 py-1 text-xs text-muted-foreground">{label}</label>
          <InlineRename
            value={railMenu ? (group?.name ?? "") : ""}
            ariaLabel="Group name"
            commitOnBlur={false}
            className="w-64 rounded border border-input bg-background px-2 py-1 text-sm outline-none focus-visible:ring-1 focus-visible:ring-ring"
            validate={(next) => validateGroupName(list, next, railMenu ? group?.id : undefined)}
            onCancel={onClose}
            onSubmit={(name) =>
              edit(
                railMenu
                  ? { action: "rename", groupId: groupId!, name }
                  : {
                      action: "create",
                      name,
                      workspaceIds: workspace ? [workspace.id] : [],
                      pruneEmpty: true,
                    },
              )
            }
          />
        </div>
      ) : railMenu ? (
        <>
          <PositionedMenuItem disabled={busy} onClick={() => setNaming(true)}>
            Rename…
          </PositionedMenuItem>
          <PositionedMenuItem disabled={busy} onClick={() => void remove(true)}>
            Delete group
          </PositionedMenuItem>
        </>
      ) : (
        <>
          {children}
          {canGroup && (
            <>
              <div role="separator" className="my-1 border-t border-border" />
              {list.some((g) => g.id !== group?.id) && (
                <PositionedSubmenu label="Move to group">
                  <DropdownMenuRadioGroup
                    value={group?.id ?? ""}
                    onValueChange={(id) => {
                      if (id === group?.id) return;
                      const target = list.find((g) => g.id === id);
                      if (target) {
                        void edit({
                          action: "add",
                          groupId: id,
                          workspaceId: workspace.id,
                          pruneEmpty: true,
                        }).then((error) => {
                          if (error) toast.error(error);
                        });
                      }
                    }}
                  >
                    {list.map((g) => (
                      <DropdownMenuRadioItem
                        key={g.id}
                        value={g.id}
                        title={g.name}
                        aria-label={`${g.name}, ${g.workspaceIds.length} workspace${g.workspaceIds.length === 1 ? "" : "s"}`}
                      >
                        <span className="max-w-64 truncate">{g.name}</span>
                        <span className="ml-auto pl-3 text-xs text-muted-foreground">
                          {g.workspaceIds.length || "empty"}
                        </span>
                      </DropdownMenuRadioItem>
                    ))}
                  </DropdownMenuRadioGroup>
                </PositionedSubmenu>
              )}
              <PositionedMenuItem disabled={busy} onClick={() => setNaming(true)}>
                New group…
              </PositionedMenuItem>
              {group && (
                <PositionedMenuItem disabled={busy} onClick={() => void remove(false)}>
                  Remove from group
                </PositionedMenuItem>
              )}
            </>
          )}
          {footer}
        </>
      )}
    </PositionedContextMenu>
  );
}
