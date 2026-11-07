// SPDX-License-Identifier: MIT
/**
 * The viewer's projection of the host-owned terminal layout. Each admitted
 * runtime member — This Mac's in-process runtime, its session-server sidecar,
 * a remote Machine's server — owns which workspaces hold its terminal tabs
 * (names, order, grid, focus, live/exited) and pushes every change, stamped
 * with the member's admitted target. The viewer keeps the latest layout per
 * member and a per-workspace projection of every shown member for rendering:
 * This Mac's tabs first, then each remote Machine's, one tab strip. A remote
 * Machine that drops keeps its tabs shown, marked offline, until it comes
 * back or is removed. Nothing here is ever persisted or edited in place:
 * {@link applyLayoutInput} is the one way in.
 *
 * Pure, so the projection rules are unit-tested without a store.
 */

import type {
  FrozenRuntimeLayout,
  RuntimeAdmissionRow,
  RuntimeLayoutReply,
  RuntimeMemberAddress,
  RuntimeMemberTarget,
  RuntimeNamespace,
  RuntimeSessionTarget,
  TerminalGridPreset,
  TerminalLayoutChanged,
  TerminalLayoutSnapshot,
  TerminalTabEntry,
  TerminalWorkspaceDescriptor,
  TerminalWorkspaceLayout,
} from "@/features/viewer/lib/cli-workspace/transport/terminal-layout-source";
import type {
  TerminalTab,
  TerminalTabOrigin,
} from "@/features/viewer/lib/cli-workspace/store/viewer-access";

/** One live member's latest layout, as this viewer sees it. */
export interface MemberLayout {
  privateAuthorityEpoch?: string;
  /** The admitted member this layout came from; its actions carry it. */
  target: RuntimeMemberTarget;
  /** The member's layout rev at the latest snapshot or change applied. */
  rev: number;
  workspaces: Record<string, TerminalWorkspaceLayout>;
  /** This viewer's focus per workspace. */
  active: Record<string, string>;
  /** Exited entries this viewer dismissed (it relaunched past or closed them
   *  while another viewer still shows them) — no longer shown here. */
  dismissed: readonly string[];
  /** When a remote Machine stopped answering: its layout is the last one it
   *  held, shown frozen until it is live again. */
  offlineSince?: number;
}

/** A workspace's tab grid, as rendered. */
export interface ProjectedGrid {
  paned: string[];
  preset: TerminalGridPreset;
  collapsed: boolean;
}

/** One workspace's layout merged across This Mac's members, as rendered: its tab-strip
 *  entries in order, its Hidden tabs (running out of sight — its background
 *  entries — listed under its tray chip), the focused tab and the grid. */
export interface ProjectedWorkspace {
  tabs: TerminalTab[];
  /** The workspace's Hidden tabs (Background entries the user hid), in order:
   *  still running, or ended — kept listed, with its exit, until the user acts. */
  hiddenTabs: TerminalTab[];
  active: string | undefined;
  grid: ProjectedGrid;
  descriptor: TerminalWorkspaceDescriptor;
}

export interface TerminalLayoutProjection {
  /** True once the first snapshot landed: until then "no tabs" means "not
   *  known yet", never "empty". */
  hydrated: boolean;
  /** The latest admission row per member, keyed by {@link memberKey} — kept
   *  after a member is removed so a late reply cannot bring it back. */
  admission: Record<string, RuntimeAdmissionRow>;
  /** When this viewer first saw each member's latest admission row (epoch ms), keyed like
   *  `admission`: how long a remote Machine admitted live or unavailable, with nothing of it
   *  held, has been waited on. Kept only while it is waited on (pruned once its layout is held,
   *  or its row stops being one that is waited on). */
  admittedAt?: Record<string, number>;
  /** Each live member's layout, keyed by {@link memberKey}. */
  members: Record<string, MemberLayout>;
  /** The rendered layout per workspace key: This Mac's members, then each
   *  privately admitted remote Machine's, in one strip. */
  byKey: Record<string, ProjectedWorkspace>;
  /** The tab this viewer focused per workspace key, when it is on a member
   *  other than the one whose focus would show (a remote tab beside local
   *  ones). Cleared by a local focus. */
  focus?: Record<string, string>;
  /** The remote tabs a workspace's grid panes, per workspace key — the part of
   *  its one grid This Mac's host cannot hold (it panes only its own tabs). */
  grids?: Record<string, ViewerGrid>;
}

/** The viewer-held part of a workspace's grid: its paned remote tabs, each
 *  with the member that holds it, and the grid's shape. This Mac's host holds
 *  the rest — its own paned tabs and the shape — and its shape wins; the shape
 *  here is read only while This Mac holds no tab of the workspace. A Machine's
 *  own grid is never read. */
export interface ViewerGrid {
  paned: { session: string; member: string }[];
  preset: TerminalGridPreset;
  collapsed: boolean;
}

export const EMPTY_TERMINAL_LAYOUT: TerminalLayoutProjection = Object.freeze({
  hydrated: false,
  admission: {},
  admittedAt: {},
  members: {},
  byKey: {},
  focus: {},
  grids: {},
}) as TerminalLayoutProjection;

/** What the viewer learns about the layout: every member's snapshot reply, a
 *  member's admission changing, one member's changed workspace, or that this
 *  viewer dismissed an entry it no longer shows (an exited one another viewer
 *  still shows, or one it superseded). */
export type LayoutInput =
  | { kind: "snapshot"; replies: RuntimeLayoutReply[] }
  | { kind: "admission"; row: RuntimeAdmissionRow }
  | {
      kind: "change";
      target: RuntimeMemberTarget;
      change: TerminalLayoutChanged;
      privateAuthorityEpoch?: string;
    }
  | { kind: "private-retired" }
  | { kind: "dismissed"; session: string }
  /** This viewer focused `session` in the workspace `key` (`null`: its
   *  members' own focus shows again). */
  | { kind: "focused"; key: string; session: string | null }
  /** This viewer arranged the workspace `key`'s grid: its remote part
   *  (`null`: none). */
  | { kind: "viewer-grid"; key: string; grid: ViewerGrid | null }
  /** The viewer-held grids restored from the app's saved state. */
  | { kind: "viewer-grids"; grids: Record<string, ViewerGrid> }
  /** The host answered a respawn in place with the tab's entry (live again):
   *  applied at once, ahead of the host's announcement of it. */
  | { kind: "respawned"; entry: TerminalTabEntry };

/** `applied` — the projection moved; `stale` — an old or duplicate input,
 *  dropped; `resync` — the viewer missed changes (an unknown member, a new
 *  admission, a rev gap): re-fetch the snapshot. */
export type LayoutApplyOutcome = "applied" | "stale" | "resync";

const NO_PANES = Object.freeze([]) as unknown as string[];
const DEFAULT_GRID = Object.freeze({
  paned: [],
  preset: "columns",
  collapsed: false,
}) as unknown as { preset: TerminalGridPreset; collapsed: boolean };
const NO_TABS = Object.freeze([]) as unknown as TerminalTab[];

/** The layout tags a remote shell tab carries: its handle and the request
 *  that created it, set by the host when it opened the tab. */
export const REMOTE_HANDLE_TAG = "remoteHandle";
export const REMOTE_REQUEST_TAG = "remoteRequestId";

function sameOrigin(a: TerminalTabOrigin | undefined, b: TerminalTabOrigin | undefined): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return (
    a.machineId === b.machineId &&
    a.member === b.member &&
    a.handle === b.handle &&
    a.requestId === b.requestId &&
    a.offlineSince === b.offlineSince
  );
}

/** Project one entry as a tab, reusing `prev` when nothing it shows changed. */
function toTab(
  entry: TerminalTabEntry,
  prev: TerminalTab | undefined,
  host: MemberLayout,
): TerminalTab {
  const exit =
    entry.state.kind === "exited"
      ? { code: entry.state.exitCode, signal: entry.state.signal }
      : undefined;
  const role = entry.role ?? undefined;
  const origin: TerminalTabOrigin | undefined =
    host.target.member.slot === "remote"
      ? {
          machineId: host.target.member.machineId,
          member: memberKey(host.target.member),
          handle: entry.tags[REMOTE_HANDLE_TAG] ?? "",
          ...(entry.tags[REMOTE_REQUEST_TAG] ? { requestId: entry.tags[REMOTE_REQUEST_TAG] } : {}),
          ...(host.offlineSince !== undefined ? { offlineSince: host.offlineSince } : {}),
        }
      : undefined;
  if (
    prev &&
    prev.name === entry.name &&
    prev.role === role &&
    prev.exit?.code === exit?.code &&
    prev.exit?.signal === exit?.signal &&
    (prev.exit === undefined) === (exit === undefined) &&
    sameOrigin(prev.origin, origin)
  ) {
    return prev;
  }
  return {
    id: entry.session,
    sessionId: entry.session,
    name: entry.name,
    ...(role !== undefined ? { role } : {}),
    ...(exit ? { exit } : {}),
    ...(origin ? { origin } : {}),
  };
}

function sameIds(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((id, i) => id === b[i]);
}

function sameTabs(a: readonly TerminalTab[], b: readonly TerminalTab[]): boolean {
  return a.length === b.length && a.every((tab, i) => tab === b[i]);
}

/** The tag a viewer stamps on an entry the user Hid from its tile (the tab `×`
 *  on a busy tab, or Hide). Only these Background entries are the tile's Hidden
 *  tabs: a headless launch into the same workspace is Background too, but it
 *  belongs to the Agents panel, never the tray. The mark is never cleared — on
 *  a tab entry it means nothing, and every Hide stamps it again. */
export const USER_HIDDEN_TAG = "hidden";

/** Whether a remote member's workspace may show under `key`: its descriptor
 *  names the same org and team the key carries, so a workspace id shared by
 *  two teams never shows one team's remote tabs in the other's. */
function sameTeam(key: string, layout: TerminalWorkspaceLayout): boolean {
  const at = key.indexOf("|");
  if (at <= 0) return false;
  const teamKey = key.slice(0, at);
  const colon = teamKey.indexOf(":");
  const org = colon === -1 ? teamKey : teamKey.slice(0, colon);
  const team = colon === -1 ? null : teamKey.slice(colon + 1);
  return (
    layout.descriptor.org === org &&
    (layout.descriptor.team ?? null) === (team === "_none" ? null : team)
  );
}

/** The entries of `layout` a host shows: a remote Machine's only when they are
 *  its private shell tabs (an untagged session has no tab to attach). */
function shownEntries(host: MemberLayout, key: string, layout: TerminalWorkspaceLayout) {
  if (host.target.member.slot !== "remote") return layout.tabs;
  if (!sameTeam(key, layout)) return [];
  return layout.tabs.filter((entry) => !!entry.tags[REMOTE_HANDLE_TAG]);
}

/** Merge `key`'s layout across `hosts` into its rendered form, reusing the
 *  previous projection's objects wherever they are unchanged (so a change to
 *  one tab re-renders only that tab). `undefined` when no host holds it. */
function projectWorkspace(
  hosts: readonly MemberLayout[],
  key: string,
  prev: ProjectedWorkspace | undefined,
  focus: string | undefined,
  viewerGrid: ViewerGrid | undefined,
): ProjectedWorkspace | undefined {
  const held = hosts.flatMap((host) => {
    const layout = host.workspaces[key];
    if (!layout) return [];
    const entries = shownEntries(host, key, layout);
    if (host.target.member.slot === "remote" && entries.length === 0) return [];
    return [{ host, layout, entries }];
  });
  if (held.length === 0) return undefined;
  const prevById = new Map(
    [...(prev?.tabs ?? []), ...(prev?.hiddenTabs ?? [])].map((t) => [t.id, t]),
  );
  const shown = (placement: TerminalTabEntry["placement"]) =>
    held.flatMap(({ host, entries }) =>
      entries
        .filter(
          (entry) =>
            entry.placement === placement &&
            !host.dismissed.includes(entry.session) &&
            // A Hidden tab stays listed once its process ends, marked ended or
            // crashed, until the user restores, restarts or dismisses it.
            (placement === "tab" || USER_HIDDEN_TAG in entry.tags),
        )
        .map((entry) => toTab(entry, prevById.get(entry.session), host)),
    );
  const nextTabs = shown("tab");
  const tabs = prev && sameTabs(prev.tabs, nextTabs) ? prev.tabs : nextTabs;
  const nextHidden = shown("background");
  const hiddenTabs =
    nextHidden.length === 0
      ? NO_TABS
      : prev && sameTabs(prev.hiddenTabs, nextHidden)
        ? prev.hiddenTabs
        : nextHidden;
  const ids = new Set(tabs.map((t) => t.id));
  // This viewer's own focus first; a workspace no viewer focused yet shows
  // its first tab.
  const active =
    (focus && ids.has(focus) ? focus : undefined) ??
    held.map(({ host }) => host.active[key]).find((id) => id && ids.has(id)) ??
    tabs[0]?.id;
  // One grid per workspace: This Mac's host holds its own paned tabs and the
  // shape, the viewer the paned remote tabs. A Machine's own grid is ignored.
  const local = held.filter(({ host }) => host.target.member.slot !== "remote");
  const shape =
    (local.find(({ layout }) => layout.grid.paned.length > 0) ?? local[0])?.layout.grid ??
    viewerGrid ??
    DEFAULT_GRID;
  const pane = new Set([
    ...local.flatMap(({ layout }) => layout.grid.paned),
    ...(viewerGrid?.paned.map((p) => p.session) ?? []),
  ]);
  const nextPaned = tabs.filter((t) => pane.has(t.id)).map((t) => t.id);
  const paned =
    nextPaned.length === 0
      ? NO_PANES
      : prev && sameIds(prev.grid.paned, nextPaned)
        ? prev.grid.paned
        : nextPaned;
  const preset = shape.preset;
  const collapsed = shape.collapsed;
  const grid =
    prev &&
    prev.grid.paned === paned &&
    prev.grid.preset === preset &&
    prev.grid.collapsed === collapsed
      ? prev.grid
      : { paned, preset, collapsed };
  const descriptor = held[0].layout.descriptor;
  if (
    prev &&
    prev.tabs === tabs &&
    prev.hiddenTabs === hiddenTabs &&
    prev.active === active &&
    prev.grid === grid &&
    prev.descriptor === descriptor
  ) {
    return prev;
  }
  return { tabs, hiddenTabs, active, grid, descriptor };
}

/** A member's key in the projection's records. */
export function memberKey(member: RuntimeMemberAddress): string {
  return `${member.machineId}/${member.slot}`;
}

/** How long a remote Machine admitted live or unavailable, or being connected, with nothing of
 *  it held, is waited on for its layout before its tabs count as unknown for good (until it is
 *  admitted or connected again). */
export const REMOTE_LAYOUT_WAIT_MS = 15_000;

/** Whether a wait that began at `since` is still running at `now`. A clock that went backwards
 *  (`now` before `since`) ends the wait rather than stretching it until the clock catches up. */
export function withinRemoteLayoutWait(since: number | undefined, now: number): boolean {
  if (since === undefined) return false;
  const waited = now - since;
  return waited >= 0 && waited < REMOTE_LAYOUT_WAIT_MS;
}

/** Whether `key`'s admission row is one a viewer waits on a layout for: a remote Machine's, live
 *  or unavailable. */
function waitedOn(row: RuntimeAdmissionRow | undefined): boolean {
  return (
    row !== undefined &&
    row.member.slot === "remote" &&
    (row.state === "live" || row.state === "unavailable")
  );
}

/**
 * Whether a remote Machine's layout is still on its way, so the tabs it holds are not known yet:
 * it is admitted live or unavailable, nothing of it is held, and it was admitted less than
 * {@link REMOTE_LAYOUT_WAIT_MS} ago; or it is being connected, nothing of it is held, and it set
 * out less than that long ago. No Machine is waited on forever, live, unreachable or stuck
 * connecting. `connecting` maps each Machine being connected to when it set out; a plain list
 * of ids is taken as already bounded by the caller. Time-dependent: a caller re-reads it as
 * `now` moves on.
 */
export function remoteLayoutPending(
  layout: Pick<TerminalLayoutProjection, "admission" | "members" | "admittedAt"> | null | undefined,
  connecting: Iterable<string> | Readonly<Record<string, number>>,
  now: number = Date.now(),
): boolean {
  const admission = layout?.admission ?? {};
  const members = layout?.members ?? {};
  for (const [key, row] of Object.entries(admission)) {
    if (!waitedOn(row) || members[key]) continue;
    if (withinRemoteLayoutWait(layout?.admittedAt?.[key], now)) return true;
  }
  const setOut: Iterable<[string, number | null]> =
    Symbol.iterator in connecting
      ? [...(connecting as Iterable<string>)].map((id): [string, null] => [id, null])
      : Object.entries(connecting as Readonly<Record<string, number>>);
  for (const [machineId, since] of setOut) {
    if (members[memberKey({ machineId, slot: "remote" })]) continue;
    if (since === null || withinRemoteLayoutWait(since, now)) return true;
  }
  return false;
}

/** When each running wait of {@link remoteLayoutPending} on an admitted Machine ends (epoch ms),
 *  soonest first: a caller moves its clock then, so the answer is re-read the moment one runs
 *  out. */
export function remoteLayoutWaitEnds(
  layout: Pick<TerminalLayoutProjection, "admission" | "members" | "admittedAt"> | null | undefined,
): number[] {
  const ends: number[] = [];
  for (const [key, row] of Object.entries(layout?.admission ?? {})) {
    if (!waitedOn(row) || layout?.members?.[key]) continue;
    const since = layout?.admittedAt?.[key];
    if (since !== undefined) ends.push(since + REMOTE_LAYOUT_WAIT_MS);
  }
  return ends.sort((x, y) => x - y);
}

/** `admittedAt` kept only for the members still waited on: a remote row live or unavailable with
 *  nothing of it held. */
function pruneAdmittedAt(
  admittedAt: Record<string, number>,
  admission: Readonly<Record<string, RuntimeAdmissionRow>>,
  members: Readonly<Record<string, MemberLayout>>,
): Record<string, number> {
  for (const key of Object.keys(admittedAt)) {
    if (!waitedOn(admission[key]) || members[key]) delete admittedAt[key];
  }
  return admittedAt;
}

/** Admission counters are decimal u64s: compare them exactly. */
function compareRevision(a: string, b: string): number {
  const d = BigInt(a) - BigInt(b);
  return d > 0n ? 1 : d < 0n ? -1 : 0;
}

function sameNamespace(a: RuntimeNamespace, b: RuntimeNamespace): boolean {
  return a.serverId === b.serverId && a.epoch === b.epoch && a.generation === b.generation;
}

function isRemote(member: MemberLayout): boolean {
  return member.target.member.slot === "remote";
}

/** The members whose tabs show, in strip order: This Mac's first, then every
 *  remote Machine admitted under a private authority, by machine. A remote
 *  layout with no private authority is never shown. */
function shownMembers(members: Record<string, MemberLayout>): MemberLayout[] {
  const local = Object.values(members).filter((m) => !isRemote(m));
  const remote = Object.values(members)
    .filter((m) => isRemote(m) && m.privateAuthorityEpoch != null)
    .sort((a, b) =>
      a.target.member.machineId < b.target.member.machineId
        ? -1
        : a.target.member.machineId > b.target.member.machineId
          ? 1
          : 0,
    );
  return [...local, ...remote];
}

/** Re-project `keys` over `members`, keeping every other workspace as it was. */
function reproject(
  prev: TerminalLayoutProjection,
  members: Record<string, MemberLayout>,
  keys: Iterable<string>,
  focus: Record<string, string> = prev.focus ?? {},
  grids: Record<string, ViewerGrid> = prev.grids ?? {},
): Record<string, ProjectedWorkspace> {
  const shown = shownMembers(members);
  let byKey = prev.byKey;
  for (const key of keys) {
    const next = projectWorkspace(shown, key, prev.byKey[key], focus[key], grids[key]);
    if (next === prev.byKey[key]) continue;
    if (byKey === prev.byKey) byKey = { ...prev.byKey };
    if (next) byKey[key] = next;
    else delete byKey[key];
  }
  return byKey;
}

function memberFromSnapshot(
  target: RuntimeMemberTarget,
  snapshot: TerminalLayoutSnapshot,
  privateAuthorityEpoch?: string,
): MemberLayout {
  const active: Record<string, string> = {};
  for (const [key, layout] of Object.entries(snapshot.workspaces)) {
    const focus = snapshot.viewer.active[key] ?? layout.lastActive;
    if (focus) active[key] = focus;
  }
  return {
    target,
    privateAuthorityEpoch,
    rev: snapshot.rev,
    workspaces: snapshot.workspaces,
    active,
    dismissed: snapshot.viewer.hidden,
  };
}

/** A remote Machine's last layout, frozen: shown offline from `since`. Its
 *  target names the lifetime that held it, which resolves to nothing
 *  actionable while the member is not live. */
function frozenMember(
  member: RuntimeMemberAddress,
  revision: string,
  frozen: FrozenRuntimeLayout,
  privateAuthorityEpoch: string | undefined,
  since: number,
): MemberLayout {
  const target: RuntimeMemberTarget = {
    member,
    admissionRevision: revision,
    namespace: frozen.lastNamespace ?? {
      serverId: frozen.serverId,
      epoch: frozen.epoch,
      generation: "0",
    },
  };
  return {
    ...memberFromSnapshot(target, frozen.snapshot, privateAuthorityEpoch),
    offlineSince: since,
  };
}

/** `held`, kept shown but marked offline (since its first drop). */
function offline(held: MemberLayout, since: number): MemberLayout {
  return held.offlineSince !== undefined ? held : { ...held, offlineSince: since };
}

/** The admission row a snapshot reply was sealed under. */
function admissionOf(reply: RuntimeLayoutReply): RuntimeAdmissionRow {
  const { member, admissionRevision: revision } = reply;
  if (reply.mode === "live") {
    return {
      member,
      revision,
      privateAuthorityEpoch: reply.privateAuthorityEpoch,
      state: "live",
      namespace: reply.namespace,
      historical: null,
    };
  }
  if (reply.mode === "historical") {
    return {
      member,
      revision,
      privateAuthorityEpoch: reply.privateAuthorityEpoch,
      state: "historical",
      namespace: null,
      historical: reply.frozen,
    };
  }
  return { member, revision, state: "unavailable", namespace: null, historical: null };
}

/** Workspace keys `members` hold, plus every key already projected. */
function affectedKeys(
  prev: TerminalLayoutProjection,
  members: Iterable<MemberLayout>,
): Set<string> {
  const keys = new Set(Object.keys(prev.byKey));
  for (const member of members) for (const key of Object.keys(member.workspaces)) keys.add(key);
  return keys;
}

/** The viewer-held grids, without the remote tabs that are truly gone: their
 *  Machine was removed, its private authority retired, or its live layout no
 *  longer holds them there. A tab of an offline or not yet connected Machine
 *  stays paned, so its cell shows it offline. `prev` itself when nothing went. */
function pruneGrids(
  projection: TerminalLayoutProjection,
  grids: Record<string, ViewerGrid>,
): Record<string, ViewerGrid> {
  let next = grids;
  for (const [key, grid] of Object.entries(grids)) {
    const paned = grid.paned.filter(({ session, member }) => {
      const row = projection.admission[member];
      if (row?.state === "removed") return false;
      const held = projection.members[member];
      // Not read yet (a Machine not connected since launch): kept for it.
      if (!held) return true;
      if (held.offlineSince !== undefined) return true;
      return held.workspaces[key]?.tabs.some((t) => t.session === session) ?? false;
    });
    if (paned.length === grid.paned.length) continue;
    if (next === grids) next = { ...grids };
    next[key] = { ...grid, paned };
  }
  return next;
}

function sameGrid(a: ViewerGrid | undefined, b: ViewerGrid | undefined): boolean {
  return (
    a === b ||
    (!!a &&
      !!b &&
      a.preset === b.preset &&
      a.collapsed === b.collapsed &&
      a.paned.length === b.paned.length &&
      a.paned.every((p, i) => p.session === b.paned[i].session && p.member === b.paned[i].member))
  );
}

/** Apply what the viewer learned to its projection — the one way the
 *  projection changes. A snapshot replaces every member it is not older than;
 *  an admission or change older than the member's latest is dropped. */
export function applyLayoutInput(
  prev: TerminalLayoutProjection,
  input: LayoutInput,
  now: number = Date.now(),
): { next: TerminalLayoutProjection; outcome: LayoutApplyOutcome } {
  const current = prev.grids ?? {};
  if (input.kind === "viewer-grid" || input.kind === "viewer-grids") {
    let grids: Record<string, ViewerGrid>;
    if (input.kind === "viewer-grids") {
      grids = { ...current, ...input.grids };
    } else {
      if (sameGrid(current[input.key], input.grid ?? undefined)) {
        return { next: prev, outcome: "stale" };
      }
      grids = { ...current };
      if (input.grid) grids[input.key] = input.grid;
      else delete grids[input.key];
    }
    if (prev.hydrated) grids = pruneGrids(prev, grids);
    const keys = input.kind === "viewer-grid" ? [input.key] : Object.keys(grids);
    return {
      next: { ...prev, grids, byKey: reproject(prev, prev.members, keys, undefined, grids) },
      outcome: "applied",
    };
  }
  const applied = applyMemberInput(prev, input, now);
  if (applied.next === prev || !applied.next.hydrated) return applied;
  // What the hosts said may have ended remote tabs the grid panes.
  let grids = current;
  if (input.kind === "private-retired") {
    grids = Object.fromEntries(
      Object.entries(current).map(([key, grid]) => [
        key,
        {
          ...grid,
          paned: grid.paned.filter((p) => applied.next.members[p.member] !== undefined),
        },
      ]),
    );
  }
  grids = pruneGrids(applied.next, grids);
  if (grids === current) return applied;
  const changed = Object.keys(grids).filter((key) => !sameGrid(grids[key], current[key]));
  return {
    next: {
      ...applied.next,
      grids,
      byKey: reproject(applied.next, applied.next.members, changed, undefined, grids),
    },
    outcome: applied.outcome,
  };
}

function applyMemberInput(
  prev: TerminalLayoutProjection,
  input: Exclude<LayoutInput, { kind: "viewer-grid" } | { kind: "viewer-grids" }>,
  now: number,
): { next: TerminalLayoutProjection; outcome: LayoutApplyOutcome } {
  if (input.kind === "private-retired") {
    const privateKeys = new Set([
      ...Object.keys(prev.members).filter((key) => prev.members[key].privateAuthorityEpoch != null),
      ...Object.keys(prev.admission).filter(
        (key) => prev.admission[key].privateAuthorityEpoch != null,
      ),
    ]);
    if (privateKeys.size === 0) return { next: prev, outcome: "stale" };
    const members = { ...prev.members };
    const admission = { ...prev.admission };
    const admittedAt = { ...(prev.admittedAt ?? {}) };
    for (const key of privateKeys) {
      delete members[key];
      delete admission[key];
      delete admittedAt[key];
    }
    return {
      next: {
        ...prev,
        members,
        admission,
        admittedAt,
        byKey: reproject(prev, members, affectedKeys(prev, Object.values(prev.members))),
      },
      outcome: "applied",
    };
  }
  if (input.kind === "snapshot") {
    const admission = { ...prev.admission };
    const admittedAt = { ...(prev.admittedAt ?? {}) };
    const members: Record<string, MemberLayout> = {};
    // This Mac's terminals are never dropped by an incomplete snapshot: what the
    // backend withheld or could not read keeps what was shown, and is asked for again.
    let incomplete = false;
    for (const reply of input.replies) {
      const key = memberKey(reply.member);
      const known = admission[key];
      if (known && compareRevision(reply.admissionRevision, known.revision) < 0) {
        // A reply sealed before the member's latest admission (a re-admission,
        // a removal) is older than what the viewer knows: keep what it shows.
        if (prev.members[key]) members[key] = prev.members[key];
        continue;
      }
      if (
        reply.mode === "unavailable" &&
        reply.member.slot !== "remote" &&
        known?.state === "live" &&
        known.revision === reply.admissionRevision
      ) {
        // A live member's read failed (a busy server): it is still admitted
        // live, so keep that and what it shows, and ask again.
        if (prev.members[key]) members[key] = prev.members[key];
        incomplete = true;
        continue;
      }
      if (!known || known.revision !== reply.admissionRevision) admittedAt[key] = now;
      admission[key] = admissionOf(reply);
      // This Mac's member unreadable now (a reconnecting server, or a failed
      // read the viewer cannot tell apart on a fresh boot): asked again.
      if (reply.mode === "unavailable" && reply.member.slot !== "remote") incomplete = true;
      const kept = prev.members[key];
      if (reply.mode === "unavailable" && kept) {
        // Unreachable for now (a reconnecting sidecar, a dropped Machine): keep
        // showing what it held until it is admitted again or removed.
        members[key] = isRemote(kept) ? offline(kept, now) : kept;
      }
      if (reply.mode === "historical" && reply.member.slot === "remote") {
        // A dropped Machine keeps its tabs in place, offline: the newest layout
        // this viewer saw, else the one the app last read from it.
        members[key] =
          kept && isRemote(kept)
            ? offline(kept, now)
            : frozenMember(
                reply.member,
                reply.admissionRevision,
                reply.frozen,
                reply.privateAuthorityEpoch,
                now,
              );
      }
      if (reply.mode !== "live") continue;
      const { snapshot } = reply;
      const target = {
        member: reply.member,
        admissionRevision: reply.admissionRevision,
        namespace: reply.namespace,
      };
      // A re-fetch that raced newer changes of the same admission is older
      // than what is shown: keep the newer.
      members[key] =
        kept &&
        kept.offlineSince === undefined &&
        kept.target.admissionRevision === target.admissionRevision &&
        sameNamespace(kept.target.namespace, target.namespace) &&
        kept.rev > snapshot.rev
          ? kept
          : memberFromSnapshot(target, snapshot, reply.privateAuthorityEpoch);
    }
    // A reply the backend withheld (its admission changed mid-read) keeps what
    // was shown; an admitted-live member left without a layout is asked for again.
    const replied = new Set(input.replies.map((reply) => memberKey(reply.member)));
    for (const [key, held] of Object.entries(prev.members)) {
      if (isRemote(held) || replied.has(key)) continue;
      members[key] = held;
      incomplete = true;
    }
    for (const [key, row] of Object.entries(admission)) {
      if (row.member.slot !== "remote" && row.state === "live" && !members[key]) incomplete = true;
    }
    const byKey = reproject(prev, members, affectedKeys(prev, Object.values(members)));
    return {
      next: {
        hydrated: true,
        admission,
        admittedAt: pruneAdmittedAt(admittedAt, admission, members),
        members,
        byKey,
        focus: prev.focus ?? {},
        grids: prev.grids ?? {},
      },
      outcome: incomplete ? "resync" : "applied",
    };
  }

  if (input.kind === "admission") {
    const { row } = input;
    const key = memberKey(row.member);
    const known = prev.admission[key];
    if (known && compareRevision(row.revision, known.revision) <= 0) {
      return { next: prev, outcome: "stale" };
    }
    const admitted = {
      admission: { ...prev.admission, [key]: row },
      admittedAt: { ...(prev.admittedAt ?? {}), [key]: now },
    };
    const held = prev.members[key];
    if (row.state === "live" && held && sameNamespace(held.target.namespace, row.namespace)) {
      const target = { ...held.target, admissionRevision: row.revision };
      if (held.offlineSince === undefined) {
        const members = { ...prev.members, [key]: { ...held, target } };
        return { next: { ...prev, ...admitted, members }, outcome: "applied" };
      }
      // Back from offline in the same lifetime: shown live again, and re-read
      // for what changed while it was away.
      const { offlineSince: _back, ...rest } = held;
      const members = { ...prev.members, [key]: { ...rest, target } };
      const byKey = reproject(prev, members, Object.keys(held.workspaces));
      return { next: { ...prev, ...admitted, members, byKey }, outcome: "resync" };
    }
    const remote = row.member.slot === "remote";
    if (row.state === "unavailable" || (remote && row.state === "historical")) {
      // Unreachable for now (a reconnecting sidecar, a dropped Machine): its
      // tabs stay shown — a Machine's marked offline — but no longer resolve
      // to an actionable target.
      if (!remote) return { next: { ...prev, ...admitted }, outcome: "applied" };
      const kept = held
        ? offline(held, now)
        : row.state === "historical"
          ? frozenMember(row.member, row.revision, row.historical, row.privateAuthorityEpoch, now)
          : undefined;
      if (!kept || kept === held) return { next: { ...prev, ...admitted }, outcome: "applied" };
      const members = { ...prev.members, [key]: kept };
      const byKey = reproject(prev, members, Object.keys(kept.workspaces));
      return { next: { ...prev, ...admitted, members, byKey }, outcome: "applied" };
    }
    if (row.state === "live" && held) {
      // A new lifetime or connection: its snapshot replaces the held layout.
      return { next: { ...prev, ...admitted }, outcome: "resync" };
    }
    // Removed or historical ends what the viewer showed for the member; a new
    // live one is fetched.
    const members = { ...prev.members };
    delete members[key];
    const byKey = held ? reproject(prev, members, Object.keys(held.workspaces)) : prev.byKey;
    return {
      next: { ...prev, ...admitted, members, byKey },
      outcome: row.state === "live" ? "resync" : "applied",
    };
  }

  if (input.kind === "dismissed") {
    const key = Object.keys(prev.members).find((k) =>
      Object.values(prev.members[k].workspaces).some((ws) =>
        ws.tabs.some((t) => t.session === input.session),
      ),
    );
    if (key === undefined || prev.members[key].dismissed.includes(input.session)) {
      return { next: prev, outcome: "stale" };
    }
    const member = prev.members[key];
    const members = {
      ...prev.members,
      [key]: { ...member, dismissed: [...member.dismissed, input.session] },
    };
    const keys = Object.keys(member.workspaces).filter((ws) =>
      member.workspaces[ws].tabs.some((t) => t.session === input.session),
    );
    return {
      next: { ...prev, members, byKey: reproject(prev, members, keys) },
      outcome: "applied",
    };
  }

  if (input.kind === "focused") {
    const focus = { ...(prev.focus ?? {}) };
    if (input.session === null) {
      if (!(input.key in focus)) return { next: prev, outcome: "stale" };
      delete focus[input.key];
    } else {
      if (focus[input.key] === input.session) return { next: prev, outcome: "stale" };
      focus[input.key] = input.session;
    }
    return {
      next: { ...prev, focus, byKey: reproject(prev, prev.members, [input.key], focus) },
      outcome: "applied",
    };
  }

  if (input.kind === "respawned") {
    const { entry } = input;
    const found = memberOfSession(prev, entry.session);
    if (!found) return { next: prev, outcome: "stale" };
    const { key: mk, workspace: key, layout: ws } = found;
    const member = prev.members[mk];
    const tabs = ws.tabs.map((t) =>
      t.session === entry.session ? { ...t, state: entry.state } : t,
    );
    const members = {
      ...prev.members,
      [mk]: {
        ...member,
        workspaces: { ...member.workspaces, [key]: { ...ws, tabs } },
        dismissed: member.dismissed.filter((d) => d !== entry.session),
      },
    };
    return {
      next: { ...prev, members, byKey: reproject(prev, members, [key]) },
      outcome: "applied",
    };
  }

  const { target } = input;
  const { epoch, rev, workspace, layout } = input.change;
  const key = memberKey(target.member);
  const member = prev.members[key];
  const known = prev.admission[key];
  if (
    known &&
    (!member || known.state !== "live") &&
    compareRevision(target.admissionRevision, known.revision) <= 0
  ) {
    // Sealed no later than an admission that already ended or withheld the
    // member: its next admission brings it back.
    return { next: prev, outcome: "stale" };
  }
  if (!member) return { next: prev, outcome: "resync" };
  const admitted = compareRevision(target.admissionRevision, member.target.admissionRevision);
  if (admitted < 0) return { next: prev, outcome: "stale" };
  if (
    admitted > 0 ||
    !sameNamespace(target.namespace, member.target.namespace) ||
    epoch !== member.target.namespace.epoch
  ) {
    return { next: prev, outcome: "resync" };
  }
  if (rev <= member.rev) return { next: prev, outcome: "stale" };
  const workspaces = { ...member.workspaces };
  const active = { ...member.active };
  if (layout) {
    workspaces[workspace] = layout;
    if (layout.lastActive) active[workspace] = layout.lastActive;
    else delete active[workspace];
  } else {
    delete workspaces[workspace];
    delete active[workspace];
  }
  const members = { ...prev.members, [key]: { ...member, rev, workspaces, active } };
  // A local member focusing another tab (a new one opened, a tab closed)
  // takes the focus back from a remote tab this viewer focused.
  let focus = prev.focus ?? {};
  if (
    !isRemote(member) &&
    workspace in focus &&
    (active[workspace] ?? null) !== (member.active[workspace] ?? null)
  ) {
    focus = { ...focus };
    delete focus[workspace];
  }
  const next = { ...prev, members, focus, byKey: reproject(prev, members, [workspace], focus) };
  return { next, outcome: rev === member.rev + 1 ? "applied" : "resync" };
}

/** The first member holding `session`, with the workspace holding it. */
export function memberOfSession(
  projection: TerminalLayoutProjection,
  session: string,
): { key: string; workspace: string; layout: TerminalWorkspaceLayout } | undefined {
  for (const [key, member] of Object.entries(projection.members)) {
    for (const [workspace, layout] of Object.entries(member.workspaces)) {
      if (layout.tabs.some((t) => t.session === session)) return { key, workspace, layout };
    }
  }
  return undefined;
}

/** The keys of every member holding `session`: more than one means the wire
 *  id is ambiguous, and no member may be picked for it. */
export function holdersOfSession(projection: TerminalLayoutProjection, session: string): string[] {
  return Object.entries(projection.members)
    .filter(([, member]) =>
      Object.values(member.workspaces).some((ws) => ws.tabs.some((t) => t.session === session)),
    )
    .map(([key]) => key);
}

/** Whether `session` is held by a remote Machine's member (by any, when its id
 *  is ambiguous). */
export function heldRemotely(projection: TerminalLayoutProjection, session: string): boolean {
  return holdersOfSession(projection, session).some(
    (key) => projection.members[key].target.member.slot === "remote",
  );
}

/** The exact admitted target of `session`: `undefined` unless exactly one live
 *  member holds it — an ambiguous wire id never picks a member. */
export function sessionTarget(
  projection: TerminalLayoutProjection,
  session: string,
): RuntimeSessionTarget | undefined {
  const holders = Object.entries(projection.members).filter(([, member]) =>
    Object.values(member.workspaces).some((ws) => ws.tabs.some((t) => t.session === session)),
  );
  if (holders.length !== 1) return undefined;
  const [key, member] = holders[0];
  // A member withheld since (unreachable) resolves to nothing actionable.
  const row = projection.admission[key];
  if (row && (row.state !== "live" || row.revision !== member.target.admissionRevision)) {
    return undefined;
  }
  return { ...member.target, session };
}
