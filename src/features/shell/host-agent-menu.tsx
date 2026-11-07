// SPDX-License-Identifier: MIT
import { useContext, useMemo } from "react";
import { flattenNodes } from "@/features/agents/lib/agent-list-layout";
import { launchUnderAgent } from "@/features/agents/lib/launch-sheet";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import { frozenTabMachineIn, isRemoteMachineId } from "@/features/machines/lib/machine-offline";
import { RowMenu } from "@/features/viewer/components/agent-row-menu";
import { Identity } from "@/features/viewer/components/agent-row-identity-context";
import { DROPDOWN_MENU_KIT } from "@/features/viewer/components/agent-identity-menus";
import { QuickSendPopover } from "@/features/viewer/components/agents-quick-send";
import { InboxChatPopover } from "@/features/viewer/components/agents-inbox-chat";
import { useFaceTags } from "@/features/viewer/hooks/use-face-tags";
import { useAgentDrill } from "@/features/viewer/hooks/use-agent-drill";
import { useRoleHoldings } from "@/features/viewer/hooks/use-role-holdings";
import {
  managerOptions,
  teamAgentIdentitiesOf,
} from "@/features/viewer/hooks/use-team-agent-identities";
import {
  useAgentListProjectionStore,
  useAgentListTrees,
} from "@/features/viewer/lib/agent-list-projection";
import { questionFor } from "@/shared/lib/managed-session";
import { useManagedSessionStore } from "@/shared/stores/use-managed-session-store";
import { markOfAgentStatus } from "@/shared/lib/status-mark";
import type { PopoverBounds } from "@/features/viewer/components/agents-popover";
import { pinForRow, pinOfRow, resolveAgentFace } from "@/features/viewer/lib/agent-list-view";
import { agentRowLabel, isHeadlessSession } from "@/features/viewer/lib/agent-row-presentation";
import {
  useTerminalTabIndex,
  useMinimizedTerminalTabIndex,
} from "@/features/viewer/lib/cli-workspace/store/terminal-selectors";
import { hideTab } from "@/features/viewer/lib/terminal-close";
import { openAgentInboxForRow } from "@/features/viewer/lib/agent-inbox-actions";
import { useTeamAvatarPacks } from "@/shared/hooks/use-avatar-packs";
import { agentPinId } from "@/shared/lib/agent-pins";
import { useAgentStatusOf, useAgentSubagentsOf } from "@/features/viewer/hooks/use-agent-status-of";
import { agentStatusWord, agentStatusWords } from "@/shared/lib/terminal-dot-words";
import { getActiveTeamKey } from "@/shared/lib/team-key";
import {
  findTabLocationBySessionId,
  selectHiddenTerminalTabIndex,
  useViewerStore,
} from "@/shared/stores/viewer-store";

export function useHostAgentMenu(requestedSessionId: unknown, agentId?: unknown) {
  const trees = useAgentListTrees();
  const nodes = useMemo(() => flattenNodes([...trees.values()].flat()), [trees]);
  const sessions = useAgentListProjectionStore((s) => s.sessions);
  const sessionId =
    typeof requestedSessionId === "string"
      ? requestedSessionId
      : typeof agentId === "string"
        ? sessions?.find((row) => row.handle === agentId && row.alive)?.sessionId
        : undefined;
  const node =
    typeof sessionId === "string"
      ? nodes.find((node) => node.row.sessionId === sessionId)
      : undefined;
  const row =
    node?.row ??
    (typeof sessionId === "string"
      ? sessions?.find((row) => row.sessionId === sessionId)
      : undefined);
  const teamKey = node?.teamKey ?? row?.launchTeamKey ?? getActiveTeamKey() ?? "";
  const teams = [teamKey];
  const avatarsOf = useTeamAvatarPacks(teams);
  const roles = useRoleHoldings(teams, !!row);
  const baseIdentity = useContext(Identity);
  const choices = useViewerStore((s) => s.agentIdentity);
  const tags = useFaceTags(teamKey);
  const identities = teamAgentIdentitiesOf(trees.get(teamKey) ?? [], {
    choices: choices[teamKey] ?? {},
    faces: avatarsOf(teamKey).faces,
    tags,
  });
  const pins = useViewerStore((s) => s.agentPins);
  const setIdentity = useViewerStore((s) => s.setAgentIdentity);
  const tabIndex = useTerminalTabIndex();
  const minimized = useMinimizedTerminalTabIndex();
  const hidden = useViewerStore(selectHiddenTerminalTabIndex);
  const machines = useMachinesStore((s) => s.byTeam);
  const workspaces = useViewerStore((s) => s.workspaces);
  const tab =
    typeof sessionId === "string"
      ? (findTabLocationBySessionId(tabIndex, sessionId) ??
        findTabLocationBySessionId(minimized, sessionId) ??
        findTabLocationBySessionId(hidden, sessionId))
      : null;
  const origin = tab
    ? tab.tab.origin
    : isRemoteMachineId(row?.machineId)
      ? { machineId: row.machineId }
      : null;
  const offlineMachine = frozenTabMachineIn(machines, origin, tab?.teamKey ?? teamKey);
  if (!row) return null;
  const choice = choices[teamKey]?.[row.handle];
  const choose = (patch: Parameters<typeof setIdentity>[2]) =>
    setIdentity(teamKey, row.handle, patch);
  const label = agentRowLabel(row);
  const heldPin = pinOfRow(
    Object.entries(pins).flatMap(([teamKey, values]) => values.map((pin) => ({ teamKey, pin }))),
    teamKey,
    row,
  );
  const togglePin = () => {
    const viewer = useViewerStore.getState();
    if (heldPin) viewer.unpinAgents(heldPin.teamKey, [agentPinId(heldPin.pin)]);
    else viewer.pinAgent(teamKey, pinForRow(teamKey, row, label));
  };
  const launcher = nodes.find(
    (other) =>
      other.teamKey === teamKey &&
      (other.row.handle === row.launchedBy || other.row.sessionId === row.launchedBy),
  )?.row;
  return {
    launcher,
    row,
    node: node ?? { row, children: [], workspace: null, story: null, teamKey },
    teamKey,
    label,
    choice,
    choose,
    tab: tab ?? null,
    offlineMachine,
    hasTab:
      typeof sessionId === "string" &&
      !!findTabLocationBySessionId(tabIndex, sessionId) &&
      !!tab &&
      (workspaces[tab.teamKey] ?? []).some((workspace) => workspace.id === tab.workspaceId),
    pinned: !!heldPin,
    togglePin,
    viaName: launcher ? agentRowLabel(launcher) : (row.launchedBy ?? "manager"),
    identity: {
      ...baseIdentity,
      avatarsOf,
      choose: (team: string, agent: typeof row, patch: Parameters<typeof setIdentity>[2]) =>
        setIdentity(team, agent.handle, patch),
      managerOptions: () => managerOptions(identities, row.handle),
      releaseRole: (holding: Parameters<typeof roles.release>[0]) => void roles.release(holding),
    },
    holds: roles.holdsOf(teamKey, row),
  };
}
type Target = ReturnType<typeof useHostAgentMenu>;
export function HostAgentRowMenu({
  target,
  onQuickView,
  onClose,
  onStop,
}: {
  target: Target;
  onQuickView: () => void;
  onClose: () => void;
  onStop: () => void;
}) {
  const drill = useAgentDrill(target?.teamKey ?? null);
  if (!target) return null;
  const { row, node, teamKey, label, tab, pinned } = target;
  const reveal = () => {
    drill.onLocate(row);
    onClose();
  };
  const inbox = (compose: boolean) => {
    void openAgentInboxForRow(row, drill.onLocate, { compose });
    onClose();
  };
  return (
    <Identity.Provider value={target.identity}>
      <RowMenu
        kit={DROPDOWN_MENU_KIT}
        node={node}
        rowTeam={teamKey}
        label={label}
        choice={target.choice}
        holds={target.holds}
        hasTab={target.hasTab}
        headless={isHeadlessSession(row)}
        offlineMachine={target.offlineMachine}
        viaName={target.viaName}
        pinLabel={pinned ? "Unpin" : "Pin"}
        pinHint={pinned ? `Unpin ${label}` : `Pin ${label}: it stays on top until it ends`}
        onQuickView={target.offlineMachine ? undefined : onQuickView}
        onReveal={reveal}
        onSelectHandle={
          target.launcher
            ? () => {
                drill.onLocate(target.launcher!);
                onClose();
              }
            : undefined
        }
        onHideTab={() => {
          if (tab) hideTab(tab.teamKey, tab.workspaceId, tab.tab.id);
          onClose();
        }}
        onTogglePin={target.togglePin}
        onMessage={() => inbox(true)}
        onOpenInbox={() => inbox(false)}
        onOpenStory={
          node.story
            ? () => {
                drill.onOpenStory(node.story!);
                onClose();
              }
            : undefined
        }
        onLaunchUnder={() => {
          launchUnderAgent(row, teamKey);
          onClose();
        }}
        onStop={onStop}
      />
    </Identity.Provider>
  );
}
export function HostAgentPopover({
  target,
  anchor,
  bounds,
  onClose,
}: {
  target: Target;
  anchor: HTMLElement;
  bounds?: () => PopoverBounds;
  onClose: () => void;
}) {
  const drill = useAgentDrill(target?.teamKey ?? null);
  const rows = target ? [target.row] : [];
  const statusOf = useAgentStatusOf(rows);
  const subagentsOf = useAgentSubagentsOf(rows);
  const ask = useManagedSessionStore((s) =>
    target?.row.sessionId ? questionFor(s.pendingAsks, target.row.sessionId) : undefined,
  );
  if (!target) return null;
  return (
    <QuickSendPopover
      name={target.label}
      face={resolveAgentFace(target.row, {
        name: target.label,
        choice: target.choice,
        faces: target.identity.avatarsOf(target.teamKey).faces,
      })}
      mark={target.offlineMachine ? "offline" : markOfAgentStatus(statusOf(target.row))}
      bounds={bounds}
      ask={ask ?? (statusOf(target.row) === "paused" ? "Waiting for your approval" : null)}
      statusWord={agentStatusWord(statusOf(target.row), subagentsOf(target.row))}
      nativeSubagents={subagentsOf(target.row)}
      workspace={target.node.workspace?.label ?? null}
      sessionId={target.offlineMachine ? null : target.row.sessionId}
      offlineMachine={target.offlineMachine}
      anchor={anchor}
      onClose={onClose}
      onShowInCli={() => {
        onClose();
        drill.onLocate(target.row);
      }}
    />
  );
}

/** The native row's M popup (the agent's inbox chat), opened for a dashboard row. */
export function HostAgentInboxChat({
  target,
  anchor,
  onClose,
}: {
  target: Target;
  anchor: HTMLElement;
  onClose: () => void;
}) {
  const drill = useAgentDrill(target?.teamKey ?? null);
  const rows = target ? [target.row] : [];
  const statusOf = useAgentStatusOf(rows);
  const subagentsOf = useAgentSubagentsOf(rows);
  if (!target) return null;
  const { row } = target;
  return (
    <InboxChatPopover
      name={target.label}
      statusWord={agentStatusWords(statusOf(row), subagentsOf(row))}
      workspace={target.node.workspace?.label ?? null}
      handle={row.handle}
      teamKey={target.teamKey}
      offlineMachine={target.offlineMachine}
      anchor={anchor}
      onClose={onClose}
      onOpenInCli={
        row.sessionId
          ? () => {
              onClose();
              void openAgentInboxForRow(row, drill.onLocate);
            }
          : null
      }
    />
  );
}
