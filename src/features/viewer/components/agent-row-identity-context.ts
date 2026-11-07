// SPDX-License-Identifier: MIT
import { createContext } from "react";
import type { RemoteMachineIdentity } from "@/features/machines/components/machine-location-badge";
import type { RoleHolding } from "@/features/viewer/hooks/use-role-holdings";
import type { TeamAgentIdentity } from "@/features/viewer/hooks/use-team-agent-identities";
import type { PillCrew } from "@/features/viewer/hooks/use-workspace-crews";
import { THIS_MAC } from "@/features/viewer/lib/agent-list-view";
import { BUILTIN_AVATARS, type AvatarPacks } from "@/shared/hooks/use-avatar-packs";
import type { AgentIdentityChoice, AgentIdentityPatch } from "@/shared/lib/agent-identity-choice";
import { BUILTIN_FACE_SOURCE, type FaceSource } from "@/shared/lib/avatar-packs";
import { parseTeamKey } from "@/shared/lib/team-key";
import { LOCAL_MACHINE_ID, type AgentSessionRow } from "@/tauri/commands";

/** Per-render identity the rows read: role inputs, machines, the focused tab. */
export interface IdentityContext {
  /** How many agents each agent has under it in its team's tree, by `agentRowKey`: read from the
   *  whole trees (filters never change a role). */
  launched: ReadonlyMap<string, number>;
  /** True when the agent's tab is Hidden from its tile (it keeps running). */
  isHidden: (row: AgentSessionRow) => boolean;
  /** True when a live terminal session has no tab anywhere (it runs in the background). */
  isTabless: (row: AgentSessionRow) => boolean;
  /** The tab on one of its team's tiles that shows the agent, in any team, if any (what Hide tab
   *  hides). */
  tabOf: (row: AgentSessionRow) => { teamKey: string; workspaceId: string; tabId: string } | null;
  /** The routine that started the agent, by name; null for any other agent. */
  routineOf: (row: AgentSessionRow) => string | null;
  /** The name of the machine the agent runs on when that machine is offline; else null. */
  offlineMachineOf: (row: AgentSessionRow) => string | null;
  /** The agents whose machine is offline, by `agentRowKey`, so a via chip can show its manager
   *  is. */
  offlineHandles: ReadonlySet<string>;
  /** The remote machine an agent's tab runs on, for its colour and icon; null on This Mac. */
  remoteMachineOf: (row: AgentSessionRow) => RemoteMachineIdentity | null;
  /** A remote machine of one team by id, for a workspace card's icon; null for This Mac. */
  remoteMachineFor: (teamKey: string, machineId: string) => RemoteMachineIdentity | null;
  /** The session whose tab the CLI shows: the active tab of the active team's focused workspace. */
  currentSessionId: string | undefined;
  /** Whether the list has settled since it mounted; later rows ease in. */
  settled: boolean;
  /** The team the list shows its own agents of; a node of another team carries its own. */
  homeTeamKey: string;
  /** A team as the pane names it in words: its name, or `org › team` where another open team
   *  shares the name. */
  teamLabelOf: (teamKey: string) => string;
  /** Each team's face source: its own packs, then the built-ins. */
  facesOf: (teamKey: string) => FaceSource;
  /** Each team's packs, for its agents' Face menu. */
  avatarsOf: (teamKey: string) => AvatarPacks;
  /** Each team's role and face choices, by handle. */
  choicesOf: (teamKey: string) => Readonly<Record<string, AgentIdentityChoice>>;
  /** Choose an agent's role or face, or return it to Auto. */
  choose: (teamKey: string, row: AgentSessionRow, patch: AgentIdentityPatch) => void;
  /** The avatar packs faces come from, and the team's default pack. */
  avatars: AvatarPacks;
  /** Each team's duplicate-face numbers, by handle. */
  faceTagsOf: (teamKey: string) => ReadonlyMap<string, number>;
  /** A repo's glyph in one team, when that team has two or more source repos. */
  repoGlyphOf: (teamKey: string, sourceRepoSlug: string) => string | undefined;
  /** A workspace's crew in one team, by workspace id. */
  crewOf: (teamKey: string, workspaceId: string) => PillCrew | undefined;
  /** The running agents of the agent's own team it can be shown under (never one below it). */
  managerOptions: (teamKey: string, handle: string) => readonly TeamAgentIdentity[];
  /** The machine an agent runs on, by name: a team machine's for one whose tab runs
   *  there, else This Mac. */
  machineOf: (row: AgentSessionRow) => string;
  /** The id of the machine the session runs on ({@link LOCAL_MACHINE_ID} for This Mac). */
  machineIdOf: (row: AgentSessionRow) => string;
  /** Whether a pin holds a row of the team: by its team and handle, or by its session id. */
  isPinned: (teamKey: string, row: AgentSessionRow) => boolean;
  /** Pin the row's session, or unpin it. */
  togglePin: (teamKey: string, row: AgentSessionRow, name: string) => void;
  /** The roles the agent's session holds in its own team. */
  holdsOf: (teamKey: string, row: AgentSessionRow) => readonly RoleHolding[];
  /** Release a role a session holds. */
  releaseRole: (holding: RoleHolding) => void;
  /** The pane is narrow: one trailing marker, and the hover actions live in the menu. */
  narrow: boolean;
}

export const Identity = createContext<IdentityContext>({
  launched: new Map(),
  isHidden: () => false,
  isTabless: () => false,
  tabOf: () => null,
  routineOf: () => null,
  offlineMachineOf: () => null,
  offlineHandles: new Set(),
  remoteMachineOf: () => null,
  remoteMachineFor: () => null,
  currentSessionId: undefined,
  settled: false,
  homeTeamKey: "",
  teamLabelOf: (teamKey) => teamNameOf(teamKey),
  choicesOf: () => ({}),
  facesOf: () => BUILTIN_FACE_SOURCE,
  avatarsOf: () => BUILTIN_AVATARS,
  choose: () => {},
  avatars: BUILTIN_AVATARS,
  faceTagsOf: () => new Map(),
  repoGlyphOf: () => undefined,
  crewOf: () => undefined,
  managerOptions: () => [],
  machineOf: () => THIS_MAC,
  machineIdOf: () => LOCAL_MACHINE_ID,
  isPinned: () => false,
  togglePin: () => {},
  holdsOf: () => [],
  releaseRole: () => {},
  narrow: false,
});

/** A team's own name, as its header shows it after its org; the org for a key with no team. */
export function teamNameOf(teamKey: string): string {
  return parseTeamKey(teamKey).teamName ?? parseTeamKey(teamKey).orgSlug;
}
