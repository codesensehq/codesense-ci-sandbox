// SPDX-License-Identifier: MIT
import { isRemoteMachineId, UNKNOWN_MACHINE_LABEL } from "@/features/machines/lib/machine-offline";
import { agentKeyOfRevision } from "@/features/viewer/lib/agent-rows";
import type { SessionStory } from "@/features/viewer/lib/session-story";
import { agentCliLabel } from "@/shared/lib/agent-clis";
import { registeredCliOfBinary } from "@/shared/lib/agent-runtimes";
import { AGENT_ROLE_CHOICES, type AgentIdentityChoice } from "@/shared/lib/agent-identity-choice";
import { agentPinId, agentRowKey, sessionPinId, type AgentPin } from "@/shared/lib/agent-pins";
import {
  IDLE_STATUSES,
  isNeedsYouStatus,
  WORKING_STATUSES,
  type AgentStatus,
} from "@/shared/lib/agent-status-config";
import { BUILTIN_FACE_SOURCE, parseFaceRef, type FaceSource } from "@/shared/lib/avatar-packs";
import {
  countMarks,
  markOfAgentStatus,
  topMark,
  type MarkCounts,
  type StatusMarkKind,
} from "@/shared/lib/status-mark";
import { parseTeamKey } from "@/shared/lib/team-key";
import type { AgentSessionRow } from "@/tauri/commands";

/** One agent in the launch tree: the agents it launched nest under it, wherever they work. */
export interface AgentTreeNode {
  projectedId?: string;
  projectedParentId?: string | null;
  projectedOrder?: number;
  workspaceRank?: number;
  crewLead?: boolean;
  row: AgentSessionRow;
  /** The story workspace this agent itself runs in, if any. */
  story: SessionStory | null;
  /** The workspace whose tab holds this agent, if any. */
  workspace: AgentWorkspaceRef | null;
  children: AgentTreeNode[];
  /** The displayed parent is a personal choice or the recorded launcher. */
  parentSource?: "chosen" | "launcher";
  /** Retained to explain a matching descendant, outside the current scope/filter. */
  scopeContext?: boolean;
  /** The other team it runs in, for a node of an other team's group; absent for this team's. */
  teamKey?: string;
}

/** The workspace a session's tab sits in. */
export interface AgentWorkspaceRef {
  /** The workspace record id (`generic:<dir>`, a story id, …). */
  id: string;
  /** The work it holds: its story title, or its name. */
  label: string;
  /** A story workspace shows its story; a plain folder shows a folder. */
  kind: "story" | "folder";
  /** A story workspace's story status. */
  storyStatus?: string;
  /** A story workspace's story title, when its label is the story id. */
  storyTitle?: string;
  /** The `owner/repo` a story workspace works in, when it has one. */
  sourceRepoSlug?: string;
  /** The remote machine the workspace runs on; absent on This Mac. */
  machineId?: string;
}

export function routineKeyOf(row: Pick<AgentSessionRow, "provenance">): string | null {
  return row.provenance?.routineKey?.trim() || null;
}

/** A routine's name for a row's second line: its listed name, else its id made readable
 *  (`nightly-triage` → `Nightly triage`). */
export function routineLabel(
  key: string,
  rows: readonly { key: string; def: { name: string } }[] | null | undefined,
): string {
  const named = rows?.find((r) => r.key === key)?.def.name.trim();
  if (named) return named;
  const id = key.split("/").filter(Boolean).pop() ?? key;
  const words = id.replace(/[-_]+/g, " ").trim();
  return words ? words.charAt(0).toUpperCase() + words.slice(1) : key;
}

export interface OtherTeamGroup {
  teamKey: string;
  nodes: AgentTreeNode[];
}

/**
 * A team as the pane names it: its name, or `org › team` (the org's display name) when another
 * team of `among` (team keys) has the same name in another org. A key with no team reads as its
 * org.
 */
export function paneTeamLabel(
  teamKey: string,
  among: Iterable<string>,
  orgName: (orgSlug: string) => string | undefined,
): string {
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  const org = orgName(orgSlug) || orgSlug;
  if (!teamName) return org;
  for (const other of among) {
    const o = parseTeamKey(other);
    if (o.teamName === teamName && o.orgSlug !== orgSlug) return `${org} › ${teamName}`;
  }
  return teamName;
}

/** Every node of every other team's group. */
export function otherTeamNodes(groups: readonly OtherTeamGroup[]): AgentTreeNode[] {
  return groups.flatMap((group) => group.nodes);
}

export function countNodes(nodes: readonly AgentTreeNode[]): number {
  return nodes.reduce((n, node) => n + 1 + countNodes(node.children), 0);
}

/** Every row in a tree, depth-first, launched ones included. */
export function flattenRows(nodes: readonly AgentTreeNode[]): AgentSessionRow[] {
  return nodes.flatMap((node) => [node.row, ...flattenRows(node.children)]);
}

/** The handles of every agent in the tree whose status waits on a person. */
export function needsYouHandles(
  nodes: readonly AgentTreeNode[],
  statusOf: (row: AgentSessionRow) => AgentStatus,
): Set<string> {
  return new Set(
    flattenRows(nodes)
      .filter((row) => isNeedsYouStatus(statusOf(row)))
      .map((row) => row.handle),
  );
}

/** The agents of the teams' trees whose status waits on a person, by {@link agentRowKey}: one
 *  handle can run in two teams, and only the one that waits is counted. */
export function needsYouKeys(
  teams: readonly TeamTrees[],
  statusOf: (row: AgentSessionRow) => AgentStatus,
): Set<string> {
  const keys = new Set<string>();
  for (const team of teams) {
    for (const node of flatNodes(team.nodes)) {
      if (isNeedsYouStatus(statusOf(node.row))) {
        keys.add(agentRowKey(node.teamKey ?? team.teamKey, node.row.handle));
      }
    }
  }
  return keys;
}

/** The status filters over the tree: everyone, those at work, those waiting on a
 *  person, and those neither. Working, Needs you and Idle split every status between
 *  them, so their counts always add up to All. */
export type AgentStatusFilter = "all" | "working" | "needs" | "idle";

/** Whether an agent's status passes a status filter. */
export function passesStatusFilter(status: AgentStatus, filter: AgentStatusFilter): boolean {
  if (filter === "needs") return isNeedsYouStatus(status);
  if (filter === "working") return WORKING_STATUSES.includes(status);
  if (filter === "idle") return IDLE_STATUSES.includes(status);
  return true;
}

/** Whether every word of a search query appears in an agent's searchable text. */
export function matchesAgentSearch(text: string, query: string): boolean {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return true;
  const hay = text.toLowerCase();
  return words.every((w) => hay.includes(w));
}

/**
 * The agents of one team's trees a search and a status filter keep, by {@link agentRowKey} (a
 * node of another team's group names its own team; any other is `teamKey`'s). `textOf` gives an
 * agent's searchable text (name, task, role, workspace, machine, model). Each agent matches on
 * its own.
 */
export function filteredRowKeys(
  nodes: readonly AgentTreeNode[],
  teamKey: string,
  filter: { query: string; status: AgentStatusFilter },
  statusOf: (row: AgentSessionRow) => AgentStatus,
  textOf: (node: AgentTreeNode) => string,
): Set<string> {
  const keep = new Set<string>();
  const walk = (list: readonly AgentTreeNode[]) => {
    for (const node of list) {
      if (
        passesStatusFilter(statusOf(node.row), filter.status) &&
        matchesAgentSearch(textOf(node), filter.query)
      ) {
        keep.add(agentRowKey(node.teamKey ?? teamKey, node.row.handle));
      }
      walk(node.children);
    }
  };
  walk(nodes);
  return keep;
}

// ---------------------------------------------------------------------------
// Pins: agents a person keeps at the top of the list.
// ---------------------------------------------------------------------------

/** What pinning a row of `teamKey` pins: that session only. */
export function pinForRow(
  teamKey: string,
  row: Pick<AgentSessionRow, "handle" | "sessionId">,
  name: string,
): AgentPin {
  return { kind: "session", teamKey, handle: row.handle, sessionId: row.sessionId, name };
}

/** The pin id a row of `teamKey` would be pinned under ({@link agentPinId} of
 *  {@link pinForRow}). */
export function pinIdOfRow(teamKey: string, row: Pick<AgentSessionRow, "handle">): string {
  return sessionPinId(teamKey, row.handle);
}

/** One pin as the Pinned group shows it, with its running session. */
export interface PinnedEntry {
  pin: AgentPin;
  /** The team the pin belongs to: pins are kept per team, and the Pinned group shows them all. */
  teamKey: string;
  id: string;
  /** Its running session as a flat node (the agents it launched stay in the tree). One node,
   *  unless the same handle is listed twice. */
  nodes: AgentTreeNode[];
}

/** Every node of the trees, depth-first, as flat nodes (no children). */
function flatNodes(trees: readonly AgentTreeNode[]): AgentTreeNode[] {
  return trees.flatMap((node) => [{ ...node, children: [] }, ...flatNodes(node.children)]);
}

/** One team's launch trees: a node of another team's group names its team, any other is this
 *  team's. */
export interface TeamTrees {
  teamKey: string;
  nodes: readonly AgentTreeNode[];
}

/** The agents the teams' trees list: by {@link agentRowKey}, and by session id. */
export interface ListedAgents {
  keys: Set<string>;
  sessionIds: Set<string>;
}

/** Every agent of the teams' trees, by {@link agentRowKey} and by session id. */
export function listedAgentsOf(teams: readonly TeamTrees[]): ListedAgents {
  const keys = new Set<string>();
  const sessionIds = new Set<string>();
  for (const team of teams) {
    for (const node of flatNodes(team.nodes)) {
      keys.add(agentRowKey(node.teamKey ?? team.teamKey, node.row.handle));
      if (node.row.sessionId) sessionIds.add(node.row.sessionId);
    }
  }
  return { keys, sessionIds };
}

/** A pin, with the team it is kept in. */
export interface TeamPin {
  teamKey: string;
  pin: AgentPin;
}

/**
 * The pin that holds a row of `teamKey`: the pin of its team and handle, else the pin of its
 * session id. A session id match finds a pin whose row the trees place in another team for now (a
 * row with no tab and no story follows the active team), as {@link endedSessionPins} keeps it.
 */
export function pinOfRow(
  pins: readonly TeamPin[],
  teamKey: string,
  row: Pick<AgentSessionRow, "handle" | "sessionId">,
): TeamPin | null {
  const id = pinIdOfRow(teamKey, row);
  return (
    pins.find(({ pin }) => agentPinId(pin) === id) ??
    (row.sessionId === null
      ? undefined
      : pins.find(({ pin }) => pin.sessionId === row.sessionId)) ??
    null
  );
}

/**
 * The Pinned group's entries: every team's pins, in the order things were pinned, each with its
 * running session, matched by the session's own team and handle, or by its session id when the
 * trees place it in another team. One session shows once: a later pin of a session an earlier pin
 * already shows is left out. A pinned session that no longer runs is left out
 * ({@link endedSessionPins} drops it). The team a pin is kept in decides where its changes are
 * written.
 */
export function pinnedEntries(
  pins: readonly TeamPin[],
  teams: readonly TeamTrees[],
): PinnedEntry[] {
  const flat = teams.flatMap((team) =>
    flatNodes(team.nodes).map((node) => ({
      node,
      id: pinIdOfRow(node.teamKey ?? team.teamKey, node.row),
    })),
  );
  const shown = new Set<AgentTreeNode>();
  return pins.flatMap(({ teamKey, pin }): PinnedEntry[] => {
    const id = agentPinId(pin);
    const byId = flat.filter((n) => n.id === id);
    const matched =
      byId.length > 0 || !pin.sessionId
        ? byId
        : flat.filter((n) => n.node.row.sessionId === pin.sessionId);
    const nodes = matched.map((n) => n.node).filter((n) => !shown.has(n));
    for (const n of nodes) shown.add(n);
    return nodes.length > 0 ? [{ pin, teamKey, id, nodes }] : [];
  });
}

/**
 * The pinned sessions that have ended, by pin id: no listed agent runs its session (by session
 * id) or its handle in its team, and no tab anywhere holds its session. A session id match keeps
 * a pin whose row the trees place in another team for now (a row with no tab and no story
 * follows the active team). `listed` must be the full list: pass it only once the roster and the
 * tabs have loaded, or a still-running session reads as ended.
 */
export function endedSessionPins(
  pins: readonly AgentPin[],
  listed: ListedAgents,
  hasTab: (sessionId: string) => boolean,
): string[] {
  return pins.flatMap((pin) => {
    if (listed.keys.has(agentRowKey(pin.teamKey, pin.handle))) return [];
    if (pin.sessionId && (listed.sessionIds.has(pin.sessionId) || hasTab(pin.sessionId))) {
      return [];
    }
    return [agentPinId(pin)];
  });
}

/** How many Pinned rows show before "+n more pinned". */
export const PINNED_VISIBLE_MAX = 5;

/** The trailing markers a row can show. */
export type RowMarker = "due" | "hidden" | "headless" | "pin";

/** A row's trailing markers, most urgent first (due > hidden > pin; `due`: a routine's
 *  wake waits for this session to be idle); a narrow
 *  pane keeps only the first. Hidden and headless are one slot: the first that applies.
 *  An offline agent gets its Offline tag on line one, not a trailing marker. */
export function rowMarkers(
  state: {
    hidden: boolean;
    headless: boolean;
    pinned: boolean;
    due?: boolean;
  },
  narrow: boolean,
): RowMarker[] {
  const place: RowMarker | null = state.hidden ? "hidden" : state.headless ? "headless" : null;
  const all: RowMarker[] = [
    ...(state.due ? (["due"] as const) : []),
    ...(place ? [place] : []),
    ...(state.pinned ? (["pin"] as const) : []),
  ];
  return narrow ? all.slice(0, 1) : all;
}

/** The machine an agent runs on, as a row says it when its tab runs here. */
export const THIS_MAC = "This Mac";

/**
 * The machine an agent runs on, by name: the listed machine its tab runs on (`machineId`, from
 * the tab's origin), else This Mac. A machine no list knows reads {@link UNKNOWN_MACHINE_LABEL},
 * never its raw id.
 */
export function agentMachineLabel(
  machineId: string | null | undefined,
  machines: readonly { machineId: string; label: string }[] | null | undefined,
): string {
  if (!isRemoteMachineId(machineId)) return THIS_MAC;
  return machines?.find((m) => m.machineId === machineId)?.label ?? UNKNOWN_MACHINE_LABEL;
}

/**
 * An agent's model, as far as its launch tells: a `--model`/`-m` value among its launch
 * args, else the CLI it runs (`claude` → `Claude`). Null when neither is known.
 */
export function agentModelLabel(launch: {
  cli: string | null | undefined;
  args?: readonly string[] | null;
}): string | null {
  // A CLI whose model comes from its configured endpoint names it nowhere in this live
  // launch spec, and only its adapter reads its native options: its label stays the CLI's.
  if (registeredCliOfBinary(launch.cli?.trim())?.modelEndpoint) return agentCliLabel(launch.cli);
  const args = launch.args ?? [];
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (arg.startsWith("--model=")) return arg.slice("--model=".length) || null;
    if ((arg === "--model" || arg === "-m") && args[i + 1]) return args[i + 1];
  }
  const cli = launch.cli?.trim();
  if (!cli) return null;
  return agentCliLabel(cli);
}

/** A row's quiet second line: role, workspace, machine and model, leaving out what is unknown. */
export function agentDetailLine(parts: {
  role: string;
  workspace: string | null;
  machine: string | null;
  model: string | null;
}): string {
  return [parts.role, parts.workspace, parts.machine, parts.model]
    .map((p) => p?.trim())
    .filter((p): p is string => Boolean(p))
    .join(" · ");
}

/**
 * A list row's second line, in two parts split where a "launched by" mark goes.
 *
 * `lead` names what the row's place does not already say: its team (when the list spans teams),
 * a role its piece cannot show (a manager's queen and a worker's pawn say theirs; a custom role's
 * generic piece does not), a workspace no card above it names, and a machine other than This
 * Mac. `tail` says what runs it and for how long. When nothing else would show and no launcher
 * fills the line, a stopped run's status word stands in (its first line then leaves it out), else
 * the role (unless it only repeats the agent's name).
 */
export function agentRowLine(parts: {
  team: string | null;
  role: string;
  /** The row's piece shows its role, so the line need not. */
  pieceShowsRole: boolean;
  workspace: string | null;
  machine: string | null;
  model: string | null;
  elapsed: string | null;
  /** The agent's own name, which the role never repeats. */
  name: string;
  /** A "launched by" mark fills the line. */
  hasVia: boolean;
  /** A stopped run's status word ("Ended"), or null while it runs. */
  stopped?: string | null;
}): { lead: string; tail: string } {
  const join = (xs: readonly (string | null | undefined)[]) =>
    xs
      .map((x) => x?.trim())
      .filter((x): x is string => Boolean(x))
      .join(" · ");
  const lead = join([
    parts.team,
    parts.pieceShowsRole ? null : parts.role,
    parts.workspace,
    parts.machine,
  ]);
  const tail = join([parts.model, parts.elapsed]);
  if (lead || tail || parts.hasVia) return { lead, tail };
  if (parts.stopped) return { lead: parts.stopped, tail: "" };
  const role = parts.role.trim();
  return { lead: role.toLowerCase() === parts.name.trim().toLowerCase() ? "" : role, tail: "" };
}

// ---------------------------------------------------------------------------
// Identity: what each agent is (its face), what it does in the tree (its piece) and
// how it is doing (its dot) are three separate signals, all resolved here.
// ---------------------------------------------------------------------------

/** The chess piece that shows an agent's role. */
export type AgentPiece = "manager" | "worker" | "custom";

export interface AgentRole {
  piece: AgentPiece;
  /** The role in words, for a tooltip and the accessible name. */
  label: string;
}

/**
 * An agent's role. The role a person chose for it wins, then a role the launch named; otherwise it is read from the launch tree, never stored: an
 * agent that has launched others manages them, one that has not is a worker. So a worker that
 * starts launching becomes a manager on the next render.
 */
export function resolveAgentRole(
  row: Pick<AgentSessionRow, "role">,
  tree: {
    launched: number;
    choice?: AgentIdentityChoice;
  },
): AgentRole {
  const chosen = tree.choice?.role;
  if (chosen) {
    const label = AGENT_ROLE_CHOICES.find((c) => c.id === chosen)?.label ?? chosen;
    if (chosen === "manager") return { piece: "manager", label };
    if (chosen === "worker") return { piece: "worker", label };
    return { piece: "custom", label };
  }
  const named = row.role?.trim();
  if (named) {
    const known = named.toLowerCase();
    if (known === "manager" || known === "lead") return { piece: "manager", label: "Manager" };
    if (known === "worker") return { piece: "worker", label: "Worker" };
    const offered = AGENT_ROLE_CHOICES.find((c) => c.id === known)?.label;
    return { piece: "custom", label: offered ?? named.charAt(0).toUpperCase() + named.slice(1) };
  }
  if (tree.launched > 0) {
    return { piece: "manager", label: `Manager · launched ${tree.launched}` };
  }
  return { piece: "worker", label: "Worker" };
}

/** Which agent this is, as a face: an image from an avatar pack, or a mark. */
export type AgentFace =
  | { kind: "image"; ref: string; src: string }
  | { kind: "mark"; text: string; color: string | null };

const HEX_COLOR = /^#(?:[0-9a-f]{3}|[0-9a-f]{6})$/i;

/** A short stable hash (FNV-1a), so a key always lands on the same face. */
function hashKey(key: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < key.length; i++) {
    h ^= key.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

/** Up to two initials of a name, for a mark coloured by a launch's colour hint. */
export function initialsOf(name: string): string {
  const words = name.split(/[^\p{L}\p{N}]+/u).filter(Boolean);
  if (words.length === 0) return "?";
  const letters = words.length === 1 ? words[0].slice(0, 2) : words[0][0] + words[1][0];
  return letters.toUpperCase();
}

/** A named face as one of: a pack face that is on this machine, a colour (a coloured mark of
 *  the agent's initials) or a short glyph such as an emoji. Null otherwise, so the next
 *  choice applies. */
function namedFace(
  named: string | null | undefined,
  name: string,
  faces: FaceSource,
): AgentFace | null {
  const value = named?.trim();
  if (!value) return null;
  const src = faces.resolve(value);
  if (src) return { kind: "image", ref: value, src };
  if (parseFaceRef(value)) return null;
  if (HEX_COLOR.test(value)) return { kind: "mark", text: initialsOf(name), color: value };
  if ([...value].length <= 2) return { kind: "mark", text: value, color: null };
  return null;
}

/**
 * An agent's face. The first of these that is available here wins: the face a person picked
 * for it, the avatar its launch named, its agent definition's own default face. Past those,
 * a face dealt from the team's default pack (the built-in funny pack unless the team chose another), picked stably by the agent's
 * definition key, else its name, so every session of one agent shares a face. A named face
 * whose pack this machine lacks (someone's personal pack, a pack not synced yet) falls
 * through to the next choice.
 */
export function resolveAgentFace(
  row: Pick<AgentSessionRow, "avatar" | "agentRevision" | "handle" | "displayName" | "rowNames">,
  opts: {
    name: string;
    choice?: AgentIdentityChoice;
    faces?: FaceSource;
  },
): AgentFace {
  const faces = opts.faces ?? BUILTIN_FACE_SOURCE;
  const agentKey = agentKeyOfRevision(row.agentRevision?.trim());
  const name =
    row.rowNames?.source === "progress" ? (row.rowNames.secondary ?? opts.name) : opts.name;
  const named =
    namedFace(opts.choice?.face, name, faces) ??
    namedFace(row.avatar, name, faces) ??
    (agentKey ? namedFace(faces.agentAvatar(agentKey), name, faces) : null);
  if (named) return named;
  const pool = faces.pool.length > 0 ? faces.pool : BUILTIN_FACE_SOURCE.pool;
  const dealt = pool[hashKey(agentKey ?? row.displayName ?? row.handle) % pool.length];
  return { kind: "image", ref: dealt.ref, src: dealt.src };
}

/** What makes two faces the same face: an image's ref, a mark's text and colour. */
export function faceKey(face: AgentFace): string {
  switch (face.kind) {
    case "image":
      return `image:${face.ref}`;
    case "mark":
      return `mark:${face.text}:${face.color ?? ""}`;
  }
}

/** One running agent as the duplicate-face count reads it. */
export interface FaceTagEntry {
  handle: string;
  startedAtMs: number | null | undefined;
  face: AgentFace;
}

/**
 * A number for each running agent that shares its face with another, so people can tell them
 * apart: within each group wearing one face, 1, 2, 3… by start time (earliest first; an
 * unknown start sorts last, then by handle, so the numbers never shuffle on a re-render). An
 * agent whose face is its own gets none. Pass the running agents of one team.
 */
export function duplicateFaceTags(entries: readonly FaceTagEntry[]): Map<string, number> {
  const groups = new Map<string, FaceTagEntry[]>();
  for (const e of entries) {
    const key = faceKey(e.face);
    const group = groups.get(key) ?? [];
    if (!group.some((g) => g.handle === e.handle)) group.push(e);
    groups.set(key, group);
  }
  const tags = new Map<string, number>();
  const startOf = (e: FaceTagEntry) => e.startedAtMs ?? Number.POSITIVE_INFINITY;
  for (const group of groups.values()) {
    if (group.length < 2) continue;
    group
      .slice()
      .sort((a, b) => startOf(a) - startOf(b) || a.handle.localeCompare(b.handle))
      .forEach((e, i) => tags.set(e.handle, i + 1));
  }
  return tags;
}

/** A workspace's lead, the rest of its crew as a capped stack, and its overall status. */
export interface WorkspaceCrew {
  /** The highest agent in the tree among the workspace's own (usually its manager). */
  lead: AgentSessionRow | null;
  /** The others, in tree order, at most {@link CREW_STACK_MAX}. */
  crew: AgentSessionRow[];
  /** How many more agents the stack leaves out ("+N"). */
  more: number;
  /** The crew's roll-up mark (`topMark`): needs you, then an error or a crash, then work,
   *  then the quiet marks. */
  mark: StatusMarkKind;
  /** How many of the crew show each mark, for the roll-up's words. */
  counts: MarkCounts;
}

/** The lead plus this many others makes the five portraits the approved mockup shows. */
export const CREW_STACK_MAX = 4;

/** The lead, crew and aggregate status of a workspace's agent trees. */
export function workspaceCrew(
  nodes: readonly AgentTreeNode[],
  statusOf: (row: AgentSessionRow) => AgentStatus,
  markOf: (row: AgentSessionRow) => StatusMarkKind = (row) => markOfAgentStatus(statusOf(row)),
): WorkspaceCrew {
  const rows = flattenRows(nodes);
  // Top-level nodes are the highest in the tree; among them the one leading the most wins,
  // then stable order.
  const leadNode = nodes.find((node) => node.crewLead) ?? null;
  const lead = leadNode?.row ?? null;
  const others = rows.filter((r) => r !== lead);
  const marks = rows.map(markOf);
  return {
    lead,
    crew: others.slice(0, CREW_STACK_MAX),
    more: Math.max(0, others.length - CREW_STACK_MAX),
    mark: topMark(marks) ?? "ready",
    counts: countMarks(marks),
  };
}

/** The team piece colours, one per workspace in list order, as oklch hues. */
export const TEAM_PIECE_HUES = [300, 180, 50, 340, 265, 215] as const;

/** The piece hue of the workspace at `index` among the listed ones. */
export function teamPieceHue(index: number): number {
  return TEAM_PIECE_HUES[
    ((index % TEAM_PIECE_HUES.length) + TEAM_PIECE_HUES.length) % TEAM_PIECE_HUES.length
  ];
}

/**
 * How many listed agents each agent has under it in its team's launch tree, by
 * {@link agentRowKey}: the input to its role. Read from the whole trees, so it never depends on
 * where its children's tabs sit or on a filter. An agent with none is left out.
 */
export function launchedCountsOf(teams: readonly TeamTrees[]): Map<string, number> {
  const counts = new Map<string, number>();
  const walk = (nodes: readonly AgentTreeNode[], teamKey: string) => {
    for (const node of nodes) {
      if (node.children.length > 0) {
        counts.set(agentRowKey(node.teamKey ?? teamKey, node.row.handle), node.children.length);
      }
      walk(node.children, teamKey);
    }
  };
  for (const team of teams) walk(team.nodes, team.teamKey);
  return counts;
}
