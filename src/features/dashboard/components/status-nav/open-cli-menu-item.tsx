// SPDX-License-Identifier: MIT
import { useState } from "react";
import { Monitor, Server, SquareTerminal } from "lucide-react";

import { DropdownMenuItem } from "@/components/ui/dropdown-menu";
import { machineColor } from "@/features/machines/lib/machine-color";
import { workspaceMachineChoices } from "@/features/machines/lib/machine-choices";
import { useTeamMachines } from "@/features/machines/stores/machines-store";
import { PositionedMenuItem, PositionedSubmenu } from "@/shared/components/positioned-context-menu";
import type { WorkspaceLocationChoice } from "@/shared/lib/workspace-location";
import { storyIdFromPath } from "@/shared/lib/worktree/resolve";
import { readTeamKeyed, useViewerStore } from "@/shared/stores/viewer-store";

/** Whether the story's workspace already exists (open or minimized): it then opens
 *  where it lives, with no choice of machine. */
function useStoryWorkspaceExists(teamKey: string, storyFilePath: string): boolean {
  const workspaceId = storyIdFromPath(storyFilePath);
  return useViewerStore(
    (s) =>
      readTeamKeyed(s.workspaces, teamKey, []).some((w) => w.id === workspaceId) ||
      s.softClosedWorkspaces[teamKey]?.[workspaceId] !== undefined,
  );
}

/**
 * "Open CLI" on a story row. A story whose workspace exists opens it where it lives.
 * Otherwise, on a team with machines, a submenu picks where it is created: This Mac or
 * one of the team's machines (one that can't take it is shown disabled, with why); the
 * workspace is created there with a terminal. A team without machines (or whose machines
 * are not listed yet) opens the plain way, which asks Where only when there is a choice.
 */
export function OpenCliMenuItem({
  teamKey,
  storyFilePath,
  onOpen,
  onClose,
}: {
  teamKey: string;
  storyFilePath: string;
  onOpen: (storyFilePath: string, location?: WorkspaceLocationChoice) => void;
  onClose: () => void;
}): React.JSX.Element {
  const exists = useStoryWorkspaceExists(teamKey, storyFilePath);
  const machines = useTeamMachines(teamKey);
  const [shownAt] = useState(() => Date.now());
  const choices = workspaceMachineChoices(machines, shownAt);
  const open = (location?: WorkspaceLocationChoice) => {
    onClose();
    onOpen(storyFilePath, location);
  };
  if (exists || choices.length === 0)
    return (
      <PositionedMenuItem
        className="flex items-center gap-2"
        data-testid="nav-open-cli"
        onClick={() => open()}
      >
        <SquareTerminal size={14} className="flex-shrink-0" />
        Open CLI
      </PositionedMenuItem>
    );
  return (
    <PositionedSubmenu
      label={
        <span className="flex items-center gap-2" data-testid="nav-open-cli">
          <SquareTerminal size={14} className="flex-shrink-0" />
          Open CLI
        </span>
      }
      contentClassName="min-w-56"
    >
      <DropdownMenuItem
        className="gap-2"
        data-testid="nav-open-cli-local"
        onSelect={() => open({ kind: "local" })}
      >
        <Monitor className="size-3.5 text-muted-foreground" aria-hidden />
        <span className="flex-1">This Mac</span>
      </DropdownMenuItem>
      {choices.map((choice) => (
        <DropdownMenuItem
          key={choice.machineId}
          className="gap-2"
          data-testid={`nav-open-cli-${choice.machineId}`}
          disabled={choice.blocked !== null}
          title={choice.blocked ?? undefined}
          onSelect={() => open({ kind: "machine", machineId: choice.machineId })}
        >
          <Server className="size-3.5 text-muted-foreground" aria-hidden />
          <span
            aria-hidden
            className="size-1.5 flex-shrink-0 rounded-full"
            style={{ backgroundColor: machineColor(choice.machineId) }}
          />
          <span className="min-w-0 flex-1 truncate">{choice.label}</span>
          {choice.note && (
            <span className="shrink-0 text-[11px] text-muted-foreground">{choice.note}</span>
          )}
        </DropdownMenuItem>
      ))}
    </PositionedSubmenu>
  );
}
