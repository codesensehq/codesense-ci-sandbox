// SPDX-License-Identifier: MIT
import { useContext } from "react";
import { openAgentEditor } from "@/features/agents/agents-virtual-tab";
import { useAllAgentRowsStore } from "@/features/agents/lib/agent-names";
import { agentKeyOfRevision } from "@/features/viewer/lib/agent-rows";
import { useRunningAgent } from "@/features/viewer/lib/hand-agents";
import type { AgentTreeNode } from "@/features/viewer/lib/agent-list-view";
import type { RoleHolding } from "@/features/viewer/hooks/use-role-holdings";
import type { TeamAgentIdentity } from "@/features/viewer/hooks/use-team-agent-identities";
import type { AgentIdentityChoice, AgentIdentityPatch } from "@/shared/lib/agent-identity-choice";
import type { AvatarPacks } from "@/shared/hooks/use-avatar-packs";
import type { AgentSessionRow } from "@/tauri/commands";
import { Identity } from "./agent-row-identity-context";
import {
  CONTEXT_MENU_KIT,
  FaceChoices,
  ManagerChoices,
  RoleChoices,
  type IdentityMenuKit,
} from "./agent-identity-menus";

/** A row's context menu: what can be done with the agent, in its own team. */
export function RowMenu({
  node,
  rowTeam,
  label,
  choice,
  holds,
  hasTab,
  headless,
  offlineMachine,
  viaName,
  pinLabel,
  pinHint,
  onQuickView,
  onReveal,
  onSelectHandle,
  onHideTab,
  onTogglePin,
  onMessage,
  onOpenInbox,
  onOpenStory,
  onLaunchUnder,
  onStop,
  kit = CONTEXT_MENU_KIT,
}: {
  kit?: IdentityMenuKit;
  node: AgentTreeNode;
  rowTeam: string;
  label: string;
  choice: AgentIdentityChoice | undefined;
  holds: readonly RoleHolding[];
  hasTab: boolean;
  headless: boolean;
  offlineMachine: string | null;
  viaName: string;
  pinLabel: string;
  pinHint: string;
  /** Open the quick view; absent while the agent's machine is offline. */
  onQuickView?: () => void;
  onReveal: (session: AgentSessionRow) => void;
  onSelectHandle?: (handle: string, teamKey: string) => void;
  onHideTab?: (session: AgentSessionRow) => void;
  onTogglePin: (teamKey: string, row: AgentSessionRow, label: string) => void;
  onMessage?: (session: AgentSessionRow) => void;
  onOpenInbox?: (session: AgentSessionRow) => void;
  onOpenStory?: () => void;
  onLaunchUnder?: (session: AgentSessionRow, teamKey: string) => void;
  onStop: (session: AgentSessionRow) => void;
}): React.JSX.Element {
  const {
    Item: ContextMenuItem,
    Label: ContextMenuLabel,
    Separator: ContextMenuSeparator,
    Shortcut: ContextMenuShortcut,
  } = kit;
  const identity = useContext(Identity);
  const { row } = node;
  const agentKey = agentKeyOfRevision(row.agentRevision);
  const definition = useAllAgentRowsStore((s) => s.rows?.find((agent) => agent.key === agentKey));
  const inboxEligible = useRunningAgent(row.sessionId ?? "");
  return (
    <>
      <ContextMenuLabel>Session</ContextMenuLabel>
      {onQuickView && (
        <ContextMenuItem aria-keyshortcuts="Q" onSelect={onQuickView}>
          Quick view
          <ContextMenuShortcut aria-hidden>Q</ContextMenuShortcut>
        </ContextMenuItem>
      )}
      <ContextMenuItem onSelect={() => onReveal(row)}>
        {headless ? "Open run output" : hasTab ? "Go to terminal" : "Show"}
      </ContextMenuItem>
      {row.launchedBy && onSelectHandle && (
        <ContextMenuItem onSelect={() => onSelectHandle(row.launchedBy!, rowTeam)}>
          Go to {viaName}
        </ContextMenuItem>
      )}
      {/* Hide acts on the tab, never the row: the row stays, marked hidden. */}
      {onHideTab && hasTab && (
        <ContextMenuItem onSelect={() => onHideTab(row)}>Hide tab</ContextMenuItem>
      )}
      <ContextMenuItem title={pinHint} onSelect={() => onTogglePin(rowTeam, row, label)}>
        {pinLabel}
      </ContextMenuItem>
      {/* Running supported agents have an inbox, including hand-started agents. */}
      {inboxEligible && onMessage && (
        <ContextMenuItem onSelect={() => onMessage(row)}>Message in inbox</ContextMenuItem>
      )}
      {inboxEligible && onOpenInbox && (
        <ContextMenuItem onSelect={() => onOpenInbox(row)}>Open inbox</ContextMenuItem>
      )}
      {onOpenStory && <ContextMenuItem onSelect={onOpenStory}>Open story</ContextMenuItem>}
      {onLaunchUnder && (
        <ContextMenuItem onSelect={() => onLaunchUnder(row, rowTeam)}>
          Launch agent under {label}…
        </ContextMenuItem>
      )}
      <ContextMenuItem
        variant="destructive"
        disabled={offlineMachine !== null || !row.alive}
        title={
          offlineMachine
            ? `${offlineMachine} is offline: it can be stopped once it reconnects.`
            : !row.alive
              ? "This session has ended."
              : undefined
        }
        onSelect={() => onStop(row)}
      >
        Stop…
      </ContextMenuItem>
      <ContextMenuSeparator />
      <ContextMenuLabel>Session appearance and manager</ContextMenuLabel>
      <IdentityMenus
        kit={kit}
        choice={choice}
        avatars={identity.avatarsOf(rowTeam)}
        onChoose={(patch) => identity.choose(rowTeam, row, patch)}
      />
      <ManagerMenu
        kit={kit}
        choice={choice}
        launchedBy={row.launchedBy ?? null}
        options={identity.managerOptions(rowTeam, row.handle)}
        onChoose={(manager) => identity.choose(rowTeam, row, { manager })}
      />
      <ReleaseRoleMenu kit={kit} holds={holds} onRelease={identity.releaseRole} />
      {agentKey && definition?.scope !== "builtin" && (
        <>
          <ContextMenuSeparator />
          <ContextMenuLabel>Saved agent definition</ContextMenuLabel>
          <ContextMenuItem
            onSelect={() => openAgentEditor(agentKey, definition?.archived ?? false)}
          >
            Edit saved agent
          </ContextMenuItem>
        </>
      )}
    </>
  );
}

/** Release role: free a role the session holds (the same release an agent makes), so its
 *  un-accepted work goes back to the queue. Only offered while it holds one. */
function ReleaseRoleMenu({
  holds,
  onRelease,
  kit,
}: {
  kit: IdentityMenuKit;
  holds: readonly RoleHolding[];
  onRelease: (holding: RoleHolding) => void;
}): React.JSX.Element | null {
  const {
    Item: ContextMenuItem,
    Sub: ContextMenuSub,
    SubTrigger: ContextMenuSubTrigger,
    SubContent: ContextMenuSubContent,
  } = kit;
  if (holds.length === 0) return null;
  if (holds.length === 1) {
    const [only] = holds;
    return (
      <ContextMenuItem onSelect={() => onRelease(only)}>Release role {only.label}</ContextMenuItem>
    );
  }
  return (
    <ContextMenuSub>
      <ContextMenuSubTrigger>Release role</ContextMenuSubTrigger>
      <ContextMenuSubContent>
        {holds.map((h) => (
          <ContextMenuItem key={`${h.role}:${h.generation}`} onSelect={() => onRelease(h)}>
            {h.label}
          </ContextMenuItem>
        ))}
      </ContextMenuSubContent>
    </ContextMenuSub>
  );
}

/** The Role and Face submenus: what a person can make an agent look like. Display only. */
function IdentityMenus({
  kit,
  choice,
  avatars,
  onChoose,
}: {
  kit: IdentityMenuKit;
  choice: AgentIdentityChoice | undefined;
  avatars: AvatarPacks;
  onChoose: (patch: AgentIdentityPatch) => void;
}): React.JSX.Element {
  const {
    Sub: ContextMenuSub,
    SubTrigger: ContextMenuSubTrigger,
    SubContent: ContextMenuSubContent,
  } = kit;
  return (
    <>
      <ContextMenuSub>
        <ContextMenuSubTrigger>Role</ContextMenuSubTrigger>
        <ContextMenuSubContent>
          <RoleChoices kit={kit} choice={choice} onChoose={onChoose} />
        </ContextMenuSubContent>
      </ContextMenuSub>
      <ContextMenuSub>
        <ContextMenuSubTrigger>Face</ContextMenuSubTrigger>
        <ContextMenuSubContent>
          <FaceChoices kit={kit} choice={choice} avatars={avatars} onChoose={onChoose} />
        </ContextMenuSubContent>
      </ContextMenuSub>
    </>
  );
}

/** The Manager submenu: show an agent under another running agent, at the top level, or
 *  (As launched) under whoever launched it. Display only. */
function ManagerMenu({
  kit,
  ...props
}: {
  kit: IdentityMenuKit;
  choice: AgentIdentityChoice | undefined;
  launchedBy: string | null;
  options: readonly TeamAgentIdentity[];
  onChoose: (manager: string | null) => void;
}): React.JSX.Element {
  const {
    Sub: ContextMenuSub,
    SubTrigger: ContextMenuSubTrigger,
    SubContent: ContextMenuSubContent,
  } = kit;
  return (
    <ContextMenuSub>
      <ContextMenuSubTrigger>Manager</ContextMenuSubTrigger>
      <ContextMenuSubContent className="max-h-80 w-64 overflow-y-auto">
        <ManagerChoices kit={kit} {...props} />
      </ContextMenuSubContent>
    </ContextMenuSub>
  );
}
