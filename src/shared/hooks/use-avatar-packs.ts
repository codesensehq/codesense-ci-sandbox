// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo, useRef } from "react";
import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";

import { createListenerGroup, type ListenerGroup } from "@/shared/lib/tauri-listeners";
import { getActiveTeamKey, NO_TEAM_SENTINEL, useActiveTeamKey } from "@/shared/lib/team-key";
import {
  BUILTIN_AVATAR_PACKS,
  BUILTIN_FACE_SOURCE,
  faceSource,
  type FaceSource,
} from "@/shared/lib/avatar-packs";
import { AGENTS_CHANGED_EVENT, AVATARS_CHANGED_EVENT } from "@/tauri/events";
import {
  avatarDefaultPackSet,
  avatarPackShareWithTeam,
  avatarPacksListForTeam,
  type AvatarPackView,
} from "@/tauri/commands";

/** One team's packs as they are on disk, read when the team is first needed. */
export interface TeamAvatarPacks {
  packs: AvatarPackView[];
  defaultPack: string | null;
  agentAvatars: Record<string, string>;
  warnings: string[];
  /** The read failed: only the built-in packs show, and the next time the team is needed it is
   *  read again. */
  failed?: boolean;
}

/**
 * The avatar packs on disk, per team: the team's own, then the user's personal ones, with the
 * team's default pack and each agent's default face. Read the first time a team is needed (the
 * active team, and every team the agents list shows), again on `avatars:changed` or
 * `agents:changed`, and again when a failed read's team is next needed; nothing polls. A failed
 * read leaves only the built-in packs; the active team's is read again after a growing wait.
 */
interface AvatarPacksState {
  byTeam: Record<string, TeamAvatarPacks>;
  /** Read `teamKey`'s packs now (a newer read wins over an older one still in flight). */
  load: (teamKey: string) => Promise<void>;
}

const NO_AVATARS: Record<string, string> = {};
const EMPTY_TEAM: TeamAvatarPacks = {
  packs: [],
  defaultPack: null,
  agentAvatars: NO_AVATARS,
  warnings: [],
};

/** The newest read of each team: an older one that finishes late is dropped. Numbers are never
 *  reused across teams or prunes, so a forgotten team's entry is simply deleted. */
const readSeqs = new Map<string, number>();
let lastSeq = 0;

/** How many reads of each team failed in a row, for the active team's retry wait. */
const failedReads = new Map<string, number>();

/** Each team's read in flight, so a team being read is not asked for again. */
const inFlight = new Map<string, Promise<void>>();

export const useAvatarPacksStore = create<AvatarPacksState>((set) => ({
  byTeam: {},
  load: (teamKey) => {
    const seq = ++lastSeq;
    readSeqs.set(teamKey, seq);
    const store = (entry: TeamAvatarPacks) =>
      set((st) => ({ byTeam: { ...st.byTeam, [teamKey]: entry } }));
    const read = avatarPacksListForTeam(teamKey).then(
      (view) => {
        if (readSeqs.get(teamKey) !== seq) return;
        failedReads.delete(teamKey);
        store({
          packs: view.packs,
          defaultPack: view.default_pack ?? null,
          agentAvatars: view.agent_avatars ?? NO_AVATARS,
          warnings: view.warnings,
        });
      },
      (e: unknown) => {
        if (readSeqs.get(teamKey) !== seq) return;
        failedReads.set(teamKey, (failedReads.get(teamKey) ?? 0) + 1);
        store({
          ...EMPTY_TEAM,
          warnings: [e instanceof Error ? e.message : String(e)],
          failed: true,
        });
      },
    );
    const done = read.finally(() => {
      if (inFlight.get(teamKey) === done) inFlight.delete(teamKey);
    });
    inFlight.set(teamKey, done);
    return done;
  },
}));

/** Read `teamKey`'s packs unless they are read (or being read) already. A failed read is read
 *  again. The one way a hook asks for a team. */
function ensureLoaded(teamKey: string): void {
  if (teamKey === "" || teamKey === NO_TEAM_SENTINEL || inFlight.has(teamKey)) return;
  const { byTeam, load } = useAvatarPacksStore.getState();
  const entry = byTeam[teamKey];
  if (entry && !entry.failed) return;
  void load(teamKey);
}

/** The teams each mounted hook needs, by caller: a team is kept while any caller needs it. */
const wanted = new Map<symbol, ReadonlySet<string>>();

/** Record the teams `caller` needs (none: it unmounted), and forget the teams no mounted caller
 *  needs any longer. The forgetting waits for the callers mounting in the same commit to say
 *  what they need, so one mounting first never drops a team the next is about to ask for. */
function wantTeams(caller: symbol, teams: ReadonlySet<string> | null): void {
  if (teams) wanted.set(caller, teams);
  else wanted.delete(caller);
  // Only a caller saying what it needs prunes: one unmounting while another mounts (a remount, a
  // team switch) must not drop the team the next one is about to ask for.
  if (!teams || prunePending) return;
  prunePending = true;
  queueMicrotask(() => {
    prunePending = false;
    const keep = new Set<string>();
    for (const set of wanted.values()) for (const teamKey of set) keep.add(teamKey);
    pruneTeams(keep);
  });
}
let prunePending = false;

/** Keep only the teams in `keep`: a team no list shows any longer is read again when it is next
 *  needed, and a read of it still in flight is dropped. */
function pruneTeams(keep: ReadonlySet<string>): void {
  // With no read number left, whatever read of the team is still in flight is dropped.
  const forget = (teamKey: string) => {
    readSeqs.delete(teamKey);
    failedReads.delete(teamKey);
  };
  for (const teamKey of [...inFlight.keys()]) {
    if (keep.has(teamKey)) continue;
    forget(teamKey);
    inFlight.delete(teamKey);
  }
  const { byTeam } = useAvatarPacksStore.getState();
  const dropped = Object.keys(byTeam).filter((teamKey) => !keep.has(teamKey));
  if (dropped.length === 0) return;
  dropped.forEach(forget);
  useAvatarPacksStore.setState({
    byTeam: Object.fromEntries(Object.entries(byTeam).filter(([teamKey]) => keep.has(teamKey))),
  });
}

let watchers = 0;
let listeners: ListenerGroup | null = null;

/** Read again every team already read or being read: a face or an agent changed somewhere. */
function reload(): void {
  const { byTeam, load } = useAvatarPacksStore.getState();
  for (const teamKey of new Set([...Object.keys(byTeam), ...inFlight.keys()])) void load(teamKey);
}

function watch(): () => void {
  watchers += 1;
  if (watchers === 1) {
    listeners = createListenerGroup();
    listeners.track(listen(AVATARS_CHANGED_EVENT, reload));
    listeners.track(listen(AGENTS_CHANGED_EVENT, reload));
  }
  return () => {
    watchers -= 1;
    if (watchers === 0) {
      listeners?.dispose();
      listeners = null;
    }
  };
}

/** Each team entry's face source, kept while the entry is: another team loading, or the list of
 *  teams changing, hands an unchanged team the same source, so rows that read it stay as drawn.
 *  The one face source of a team's packs: its own, then the built-ins. */
const teamFaceSources = new WeakMap<TeamAvatarPacks, FaceSource>();
function teamFaceSourceOf(entry: TeamAvatarPacks): FaceSource {
  let faces = teamFaceSources.get(entry);
  if (!faces) {
    faces = faceSource([...entry.packs, ...BUILTIN_AVATAR_PACKS], {
      defaultPack: entry.defaultPack,
      agentAvatars: entry.agentAvatars,
    });
    teamFaceSources.set(entry, faces);
  }
  return faces;
}

/** Each team entry's packs, its own then the built-ins, kept while the entry is. */
const teamPackLists = new WeakMap<TeamAvatarPacks, AvatarPackView[]>();
function teamPacksOf(entry: TeamAvatarPacks): AvatarPackView[] {
  let packs = teamPackLists.get(entry);
  if (!packs) {
    packs = [...entry.packs, ...BUILTIN_AVATAR_PACKS];
    teamPackLists.set(entry, packs);
  }
  return packs;
}

export interface AvatarPacks {
  /** Every pack, the team's and personal ones first, then the built-ins. */
  packs: AvatarPackView[];
  /** The team's default pack, when it set one (else the built-in funny pack deals faces). */
  defaultPack: string | null;
  /** Packs and faces skipped on disk, and why. */
  warnings: string[];
  /** The image for a `pack:id` ref, or null when that face is not available here. */
  resolveFace: (ref: string | null | undefined) => string | null;
  /** Everything the agent face resolver reads. */
  faces: FaceSource;
  /** Set the team's default pack (null: the built-in funny pack). */
  setDefaultPack: (pack: string | null) => Promise<void>;
  /** Copy one of your own packs into the team, so every teammate gets its faces (yours stays,
   *  for the org's other teams). Rejects when the team already has a pack with that id. The
   *  packs re-read on `avatars:changed`. */
  sharePack: (pack: string) => Promise<void>;
  /** Whether this team's packs can be shared to it or given a default: only the active team's. */
  canShare: boolean;
}

/** One team's packs as a view. `actions` are the active team's own; any other team's are
 *  read-only. */
function avatarPacksView(
  entry: TeamAvatarPacks,
  actions: Pick<AvatarPacks, "setDefaultPack" | "sharePack"> | null,
): AvatarPacks {
  const faces = teamFaceSourceOf(entry);
  return {
    packs: teamPacksOf(entry),
    defaultPack: entry.defaultPack,
    warnings: entry.warnings,
    resolveFace: faces.resolve,
    faces,
    setDefaultPack: actions?.setDefaultPack ?? noSetDefault,
    sharePack: actions?.sharePack ?? noShare,
    canShare: actions !== null,
  };
}

const noSetDefault = async (): Promise<void> => {};
const noShare = async (): Promise<void> => {};

/** The backend acts on whichever team is active when it is called: refuse when that is no longer
 *  the team these actions were made for. */
function assertStillActive(teamKey: string): void {
  if (getActiveTeamKey() !== teamKey) {
    throw new Error("The active team changed. Open the team again and retry.");
  }
}

/** The active team's own actions: none with no team. The team is the one these were made for,
 *  never whichever is active once a call returns. */
function teamActions(teamKey: string): Pick<AvatarPacks, "setDefaultPack" | "sharePack"> | null {
  if (teamKey === NO_TEAM_SENTINEL) return null;
  return {
    setDefaultPack: async (pack) => {
      assertStillActive(teamKey);
      await avatarDefaultPackSet(pack);
      useAvatarPacksStore.setState((st) => ({
        byTeam: {
          ...st.byTeam,
          [teamKey]: { ...(st.byTeam[teamKey] ?? EMPTY_TEAM), defaultPack: pack },
        },
      }));
    },
    sharePack: async (pack) => {
      assertStillActive(teamKey);
      await avatarPackShareWithTeam(pack);
    },
  };
}

/** The first wait before a failed read of the active team is read again, doubling per failure. */
const RETRY_FIRST_MS = 2_000;
const RETRY_MAX_MS = 60_000;

/** Read the active team again after a failed read, waiting longer after each failure in a row.
 *  Every other team is read again only when it is next needed. */
function useRetryFailedRead(teamKey: string, entry: TeamAvatarPacks | undefined): void {
  useEffect(() => {
    if (!entry?.failed || teamKey === NO_TEAM_SENTINEL) return;
    const failures = Math.max(1, failedReads.get(teamKey) ?? 1);
    const wait = Math.min(RETRY_MAX_MS, RETRY_FIRST_MS * 2 ** (failures - 1));
    const timer = setTimeout(() => ensureLoaded(teamKey), wait);
    return () => clearTimeout(timer);
  }, [teamKey, entry]);
}

/** A stable identity for one mounted caller of the hooks. */
function useCallerId(): symbol {
  const ref = useRef<symbol | null>(null);
  ref.current ??= Symbol("avatar-packs-caller");
  return ref.current;
}

/** The avatar packs the active team can use, and a resolver for face refs. */
export function useAvatarPacks(): AvatarPacks {
  const teamKey = useActiveTeamKey();
  const entry = useAvatarPacksStore((s) =>
    teamKey === NO_TEAM_SENTINEL ? undefined : s.byTeam[teamKey],
  );
  const caller = useCallerId();
  useEffect(() => watch(), []);
  useEffect(() => {
    wantTeams(caller, new Set([teamKey]));
    ensureLoaded(teamKey);
  }, [caller, teamKey]);
  useEffect(() => () => wantTeams(caller, null), [caller]);
  useRetryFailedRead(teamKey, entry);
  const actions = useMemo(() => teamActions(teamKey), [teamKey]);
  return useMemo(() => avatarPacksView(entry ?? EMPTY_TEAM, actions), [entry, actions]);
}

/**
 * Each listed team's packs, by team key, and always the active team's: the agents list draws
 * every team's agents with that team's own faces, and the Face menu lists that team's packs. Only
 * the active team can be shared to or given a default pack; the others are read-only here. Loads
 * any team not read yet, and forgets the teams no mounted caller lists and none has active.
 */
export function useTeamAvatarPacks(teamKeys: readonly string[]): (teamKey: string) => AvatarPacks {
  const byTeam = useAvatarPacksStore((s) => s.byTeam);
  const activeTeam = useActiveTeamKey();
  const caller = useCallerId();
  useEffect(() => watch(), []);
  const keyList = teamKeys.join("\n");
  useEffect(() => {
    const listed = keyList.split("\n").filter((tk) => tk !== "");
    wantTeams(caller, new Set([...listed, activeTeam]));
    for (const teamKey of [activeTeam, ...listed]) ensureLoaded(teamKey);
  }, [caller, keyList, activeTeam]);
  useEffect(() => () => wantTeams(caller, null), [caller]);
  useRetryFailedRead(activeTeam, byTeam[activeTeam]);
  const activeActions = useMemo(() => teamActions(activeTeam), [activeTeam]);
  const views = useMemo(
    () =>
      new Map(
        [...new Set([...keyList.split("\n"), activeTeam])]
          .filter((tk) => tk !== "" && tk !== NO_TEAM_SENTINEL)
          .map((tk): [string, AvatarPacks] => [
            tk,
            // The no-team sentinel is never a team to share into, even when no team is active.
            avatarPacksView(byTeam[tk] ?? EMPTY_TEAM, tk === activeTeam ? activeActions : null),
          ]),
      ),
    [keyList, byTeam, activeTeam, activeActions],
  );
  return useCallback((teamKey: string) => views.get(teamKey) ?? BUILTIN_AVATARS, [views]);
}

/** The built-in packs alone, for views drawn outside a team (and as a context default). */
export const BUILTIN_AVATARS: AvatarPacks = {
  packs: [...BUILTIN_AVATAR_PACKS],
  defaultPack: null,
  warnings: [],
  resolveFace: BUILTIN_FACE_SOURCE.resolve,
  faces: BUILTIN_FACE_SOURCE,
  setDefaultPack: async () => {},
  sharePack: async () => {},
  canShare: false,
};
