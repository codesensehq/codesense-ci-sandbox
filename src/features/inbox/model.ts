// SPDX-License-Identifier: MIT
import type { RoleQueue } from "@/tauri/commands";
import { messagePreview } from "@/shared/lib/message-preview";
import type { InboxTeam, SessionListRow, SessionProgress } from "@/tauri/commands";

import type { ConversationMessage, RoleSummary } from "./api";

// Pure view model for the inbox lenses and the rollup. Keys are a session handle, the literal
// "human", or "#role" for a role queue — the same strings the composer's To picker uses.

export const HUMAN = "human";

export type SessionState = SessionListRow["state"];

export interface Participant {
  key: string;
  name: string;
  kind: "human" | "session";
  state: SessionState;
  /** Null for a session on this Mac. */
  machine: string | null;
  launchedBy: string | null;
  roles: string[];
  agent: string | null;
  color: string;
  /** Its latest `signal_progress`, when the roster reported one. */
  progress: SessionProgress | null;
}

export type Directory = Map<string, Participant>;

export const isRoleKey = (key: string | null | undefined): key is string =>
  !!key && key.startsWith("#");

/**
 * The side an inbox view speaks for: one key (a tab's session, or the human in the rollup), or
 * a set of keys (every session of a saved agent), whose messages read as "out".
 */
export type Self = string | ReadonlySet<string>;

export const isSelf = (self: Self, key: string | null | undefined): boolean =>
  !!key && (typeof self === "string" ? key === self : self.has(key));

/** A message's identity: ids are unique only within one team's inbox. */
export const msgKey = (m: ConversationMessage): string => `${m.team.org}/${m.team.team}#${m.id}`;

/**
 * A withdrawn role message comes back addressed to the bare role name; put it back on its
 * queue so it renders as the role message it was (with a "withdrawn" claim state).
 */
export function normalizeMessage(m: ConversationMessage): ConversationMessage {
  if (!m.toRole && m.to && m.delivery?.state === "withdrawn")
    return { ...m, toRole: m.to, to: null, toName: null };
  return m;
}

/** The recipient key of a message: its role queue, else its session (or the human). */
export const toKeyOf = (m: ConversationMessage): string =>
  m.toRole ? `#${m.toRole}` : (m.to ?? HUMAN);

// Saturated enough to carry dark initials in both themes; picked by a stable hash so a
// session keeps its colour across renders, lenses and the rollup.
const PALETTE = [
  "#58a6ff",
  "#56d4dd",
  "#f0883e",
  "#3fb950",
  "#d2a8ff",
  "#ffa657",
  "#79c0ff",
  "#7ee787",
  "#ff7b72",
  "#e3b341",
];
const EXITED_COLOR = "#8b949e";

export function colorFor(handle: string): string {
  let h = 5381;
  for (let i = 0; i < handle.length; i++) h = ((h << 5) + h + handle.charCodeAt(i)) | 0;
  return PALETTE[Math.abs(h) % PALETTE.length] ?? PALETTE[0]!;
}

/** `impl-SW-AID-5` → `I5`; `amelia-child-5plus5` → `AC`; `Terminal 9` → `T9`. */
export function initials(name: string): string {
  const worker = name.match(/^(impl|review|qa)-.*?(\d+)$/);
  if (worker) return worker[1]![0]!.toUpperCase() + worker[2];
  const words = name
    // Composed first: a decomposed "É" (E + combining accent) is one letter, not two words.
    .normalize("NFC")
    .replace(/^(impl|review|qa)-/, "")
    // Any script's letters, marks and digits ("Équipe", "调度", "हिन्दी"), not only ASCII.
    .replace(/[^\p{L}\p{M}\p{N}]+/gu, " ")
    .trim()
    .split(" ")
    // A word starts with a letter or digit: a mark left over from a dropped emoji (U+FE0F, a
    // skin tone's modifier) is not a word and must not become an invisible initial.
    .map((w) => w.replace(/^\p{M}+/u, ""))
    .filter(Boolean)
    .slice(0, 2);
  return (
    words
      // A code point, not a UTF-16 unit: a letter outside the BMP stays whole.
      .map((w) => [...w][0])
      .join("")
      .toUpperCase() || "?"
  );
}

/**
 * A readable name for a handle nobody named: worker handles are paths
 * (`_worker/org/repo/team/demo-child`), so the last part is the session's own name.
 */
export function shortHandle(handle: string): string {
  const parts = handle.split("/").filter(Boolean);
  return parts[parts.length - 1] ?? handle;
}

/** Shorten `text` to `max` characters by cutting its middle, so both ends stay readable. */
export function middleTruncate(text: string, max: number): string {
  if (text.length <= max || max < 3) return text;
  const keep = max - 1;
  const head = Math.ceil(keep / 2);
  return `${text.slice(0, head)}…${text.slice(text.length - (keep - head))}`;
}

/** A name that is just the handle again says nothing; fall back to the handle's short form. */
const nameOr = (name: string | null | undefined, key: string): string => {
  const n = name?.trim();
  return n && n !== key ? n : shortHandle(key);
};

/** What the session list calls This Mac: its id (`local`) or its label (`This Mac`). */
const LOCAL_MACHINES = new Set(["", "local", "this-mac", "this mac", "localhost"]);
const normalizeMachine = (m: string | null | undefined): string | null =>
  m && !LOCAL_MACHINES.has(m.toLowerCase()) ? m : null;

/**
 * Everyone the inbox can name: the roster first, then anyone a message mentions that the
 * roster no longer lists (an ended session keeps its name from the message).
 */
export function buildDirectory(
  sessions: readonly SessionListRow[],
  messages: readonly ConversationMessage[],
  opts: {
    humanName?: string | null;
    queues?: readonly RoleQueue[];
    handleForSession?: (session: string) => string | null;
    holdings?: readonly { role: string; instanceId: string }[];
    /** The session the view speaks for; `name` is what the tab roster calls it. */
    me?: { handle: string; roles: readonly string[]; name?: string | null };
  } = {},
): Directory {
  const dir: Directory = new Map();
  dir.set(HUMAN, {
    key: HUMAN,
    name: opts.humanName?.trim() || "you",
    kind: "human",
    state: "running",
    machine: null,
    launchedBy: null,
    roles: [],
    agent: null,
    color: "#e6edf3",
    progress: null,
  });
  for (const s of sessions) {
    dir.set(s.handle, {
      key: s.handle,
      name: nameOr(s.displayName, s.handle),
      kind: "session",
      state: s.state,
      machine: normalizeMachine(s.machine),
      launchedBy: s.launchedBy,
      roles: [],
      agent: s.agent ?? null,
      color: s.state === "exited" ? EXITED_COLOR : colorFor(s.handle),
      progress: s.progress ?? null,
    });
  }
  const remember = (key: string | null, name: string | null) => {
    if (!key || dir.has(key)) return;
    dir.set(key, {
      key,
      name: nameOr(name, key),
      kind: "session",
      state: "unknown",
      machine: null,
      launchedBy: null,
      roles: [],
      agent: null,
      color: colorFor(key),
      progress: null,
    });
  };
  for (const m of messages) {
    remember(m.from, m.fromName);
    remember(m.to, m.toName);
  }
  const holdings = [
    ...(opts.holdings ?? []),
    ...(opts.queues ?? []).flatMap((q) =>
      q.registrations
        .filter((r) => r.live)
        .map((r) => ({ role: q.role, instanceId: r.sessionId ?? r.instanceId })),
    ),
  ];
  for (const registration of holdings) {
    const handle = opts.handleForSession?.(registration.instanceId) ?? registration.instanceId;
    const holder = handle ? dir.get(handle) : undefined;
    if (holder && !holder.roles.includes(registration.role)) holder.roles.push(registration.role);
  }
  if (opts.me) {
    const listed = dir.has(opts.me.handle);
    remember(opts.me.handle, opts.me.name ?? null);
    const me = dir.get(opts.me.handle)!;
    const roles = new Set([...me.roles, ...opts.me.roles]);
    // The roster's own display name wins; the caller's name stands in only for a short handle.
    const name = !listed && opts.me.name?.trim() ? opts.me.name.trim() : me.name;
    dir.set(me.key, { ...me, name, roles: [...roles] });
  }
  return dir;
}

export function participant(dir: Directory, key: string): Participant {
  return (
    dir.get(key) ?? {
      key,
      name: shortHandle(key),
      kind: "session",
      state: "unknown",
      machine: null,
      launchedBy: null,
      roles: [],
      agent: null,
      color: colorFor(key),
      progress: null,
    }
  );
}

/** How a key reads in prose: `#role`, `you (Name)` for the human, else the session's name. */
export function labelOf(dir: Directory, key: string): string {
  if (isRoleKey(key)) return key;
  const p = participant(dir, key);
  if (p.kind === "human") return p.name === "you" ? "you" : `you (${p.name})`;
  return p.name;
}

export const isLive = (p: Participant): boolean =>
  p.kind === "session" && p.state !== "exited" && p.state !== "offline";

export function stateLabel(state: SessionState): string {
  switch (state) {
    case "needs-you":
      return "needs you";
    case "unknown":
      return "not seen";
    default:
      return state;
  }
}

/** The status-dot modifier the stylesheet draws. */
export function dotClass(state: SessionState): "running" | "idle" | "needs" | "exited" {
  if (state === "running") return "running";
  if (state === "idle") return "idle";
  if (state === "needs-you") return "needs";
  return "exited";
}

export const machineLabel = (p: Participant): string => p.machine ?? "This Mac";

/** The roles a participant holds (the ones its queue chips show). No role is special. */
export const heldRoles = (p: Participant): string[] => p.roles;

/** Unread from `me`'s side: someone else's message that its recipient has not read. */
export function isUnreadFor(m: ConversationMessage, me: Self): boolean {
  if (!m.unread || isSelf(me, m.from)) return false;
  return me === HUMAN ? m.to === HUMAN : true;
}

/**
 * Who `me` is talking with in `m`: the role queue it went to, else the other party. When `me`
 * is a set of sessions the human is the one looking, so a message between the human and one
 * of them is filed under that session: each session is its own conversation with you.
 */
export function counterpart(m: ConversationMessage, me: Self): string {
  if (m.toRole) return `#${m.toRole}`;
  const other = isSelf(me, m.from) ? (m.to ?? HUMAN) : m.from;
  if (typeof me !== "string" && other === HUMAN)
    return isSelf(me, m.from) ? m.from : (m.to ?? HUMAN);
  return other;
}

/**
 * Who a reply to `m` goes to. For one session: whoever it was talking with. For a set of
 * sessions: always this side — the session involved, or the role queue it was sent to — since
 * the human writes to the agent being looked at, never on its behalf to someone else.
 */
export function replyTarget(
  m: ConversationMessage,
  me: Self,
  /** For a set of sessions, the roles they stand for: a queue outside them is another agent's. */
  held?: readonly string[],
): string {
  if (typeof me === "string") {
    const target = m.from === me ? toKeyOf(m) : repliesGoTo(m);
    // A tab's conversation with you: the human (who always sends) answers that session.
    return target === HUMAN && me !== HUMAN ? me : target;
  }
  // One of its sessions writing to another agent's queue: the human answers that session,
  // never, on its behalf, the other agent.
  if (m.toRole && held && !held.includes(m.toRole) && isSelf(me, m.from)) return m.from;
  if (m.toRole) return `#${m.toRole}`;
  if (isSelf(me, m.from)) return repliesGoTo(m);
  if (isSelf(me, m.to)) return m.to!;
  return repliesGoTo(m);
}

/**
 * Where an answer to `m` goes, as a composer key: its sender's `replyTo` (email Reply-To:
 * a session handle, or `role:<id>` → `#id`) when it set one, else its sender. A `replyTo`
 * naming the human (who is the one answering) changes nothing.
 */
export function repliesGoTo(m: ConversationMessage): string {
  const key = replyToKey(m);
  return key && key !== HUMAN ? key : m.from;
}

/** A message's stored `replyTo` as a composer key (`role:<id>` → `#id`); null when unset. */
export function replyToKey(m: Pick<ConversationMessage, "replyTo" | "via">): string | null {
  const raw = m.via === "terminal" ? null : m.replyTo?.trim();
  if (!raw) return null;
  if (raw.startsWith("role:")) {
    const role = raw.slice("role:".length).trim();
    // `human` is no role queue: an older build stored `role:human` for the human.
    if (role.toLowerCase() === HUMAN) return HUMAN;
    return role ? `#${role}` : null;
  }
  return raw;
}

export interface Thread {
  key: string;
  messages: ConversationMessage[];
  last: ConversationMessage;
  unread: number;
}

/** `me`'s conversations, newest activity first. */
export function threadsFor(messages: readonly ConversationMessage[], me: Self): Thread[] {
  const byKey = new Map<string, ConversationMessage[]>();
  for (const m of messages) {
    const k = counterpart(m, me);
    const list = byKey.get(k);
    if (list) list.push(m);
    else byKey.set(k, [m]);
  }
  return [...byKey.entries()]
    .map(([key, ms]) => ({
      key,
      messages: ms,
      last: ms[ms.length - 1]!,
      unread: ms.filter((m) => isUnreadFor(m, me)).length,
    }))
    .sort((a, b) => b.last.createdAtMs - a.last.createdAtMs);
}

/** The three lanes of the Threads list: Unread, Queued for my role, Conversations. */
export function threadLanes(
  threads: readonly Thread[],
  held: readonly string[],
): { unread: Thread[]; role: Thread[]; rest: Thread[] } {
  const holds = (k: string) => isRoleKey(k) && held.includes(k.slice(1));
  const role = threads.filter((t) => holds(t.key));
  const conv = threads.filter((t) => !holds(t.key));
  return {
    unread: conv.filter((t) => t.unread > 0),
    role,
    rest: conv.filter((t) => t.unread === 0),
  };
}

export type DirectionKind = "term" | "q" | "out" | "in";

/** The Mailspool's direction column: ⇢ typed, ⤳ role, → sent, ← received. */
export function direction(
  m: ConversationMessage,
  me: Self,
): { kind: DirectionKind; glyph: string } {
  if (m.via === "terminal") return { kind: "term", glyph: "⇢" };
  if (m.toRole) return { kind: "q", glyph: "⤳" };
  if (isSelf(me, m.from)) return { kind: "out", glyph: "→" };
  return { kind: "in", glyph: "←" };
}

/**
 * One line of a message for a list row: markdown marks (emphasis, code, heading hashes, table
 * pipes and rules, code fences) dropped, line breaks as " · ".
 */
export const plainPreview = (text: string): string =>
  text
    .split("\n")
    .filter((line) => !/^\s*```/.test(line) && !/^\s*\|?\s*:?-{2,}[\s|:-]*$/.test(line))
    .map((line) =>
      line
        .replace(/^\s*#{1,6}\s+/, "")
        .replace(/^\s*\|\s*|\s*\|\s*$/g, "")
        .replace(/\s*\|\s*/g, "  ")
        .replace(/\*\*|`/g, "")
        .trim(),
    )
    .filter(Boolean)
    .join(" · ");

/**
 * A message's list-row preview: the sender's one-line `summary` when it set one, an html
 * body's title and visible words (never its markup), else [`plainPreview`] of the text.
 */
export function messageRowPreview(
  m: Pick<ConversationMessage, "text" | "contentType" | "summary" | "via">,
): string {
  if (m.via !== "terminal" && (m.summary?.trim() || m.contentType === "html")) {
    return messagePreview(m, Number.MAX_SAFE_INTEGER);
  }
  return plainPreview(m.text);
}

export type SpoolFilter = "all" | "unread" | "roles" | "terminal";

/**
 * The Mailspool's filter: a chip plus a free-text prompt understanding `from:@x`, `to:@x` /
 * `to:#role`, and anything else as text to find (quotes optional).
 */
export function filterSpool(
  messages: readonly ConversationMessage[],
  me: Self,
  filter: SpoolFilter,
  query: string,
  dir: Directory,
): ConversationMessage[] {
  let out = messages.slice();
  if (filter === "unread") out = out.filter((m) => isUnreadFor(m, me));
  if (filter === "roles") out = out.filter((m) => m.toRole !== null);
  if (filter === "terminal") out = out.filter((m) => m.via === "terminal");
  const terms = query.match(/\S+:"[^"]*"|"[^"]*"|\S+/g) ?? [];
  const names = (key: string | null) =>
    key ? `${key} ${isRoleKey(key) ? "" : participant(dir, key).name}`.toLowerCase() : "";
  for (const raw of terms) {
    const term = raw.replace(/["“”]/g, "").toLowerCase();
    if (!term) continue;
    if (term.startsWith("from:")) {
      const v = term.slice(5).replace(/^@/, "");
      out = out.filter((m) => names(m.from).includes(v));
    } else if (term.startsWith("to:")) {
      const v = term.slice(3);
      out = out.filter((m) =>
        v.startsWith("#")
          ? m.toRole?.toLowerCase().includes(v.slice(1))
          : names(m.to).includes(v.replace(/^@/, "")),
      );
    } else {
      out = out.filter((m) => m.text.toLowerCase().includes(term));
    }
  }
  return out;
}

export function formatClock(ms: number): string {
  if (!Number.isFinite(ms)) return "--:--";
  const d = new Date(ms);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

export function dayLabel(ms: number, now: number = Date.now()): string {
  const start = (t: number) => {
    const d = new Date(t);
    d.setHours(0, 0, 0, 0);
    return d.getTime();
  };
  const days = Math.round((start(now) - start(ms)) / 86_400_000);
  if (days === 0) return "Today";
  if (days === 1) return "Yesterday";
  return new Date(ms).toLocaleDateString(undefined, {
    weekday: "short",
    day: "numeric",
    month: "short",
  });
}

/** Local midnight of `ms`: two messages share a day when these match. */
const dayStart = (ms: number): number => {
  const d = new Date(ms);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
};

/**
 * The day separator to draw before each message: its day's label on a new day, else null.
 * Labels are formatted once per day, not per message, so a 500-line spool stays cheap.
 */
export function dayHeads(
  messages: readonly ConversationMessage[],
  now: number = Date.now(),
): (string | null)[] {
  let prev: number | null = null;
  return messages.map((m, i) => {
    const day = dayStart(m.createdAtMs);
    // NaN never equals itself: lines with no valid time share one divider, as they always did.
    if (i > 0 && (day === prev || (Number.isNaN(day) && Number.isNaN(prev)))) return null;
    prev = day;
    return dayLabel(m.createdAtMs, now);
  });
}

/** One entry of the Mailspool's windowed list: a day divider or a message line. */
export type SpoolItem =
  | { kind: "day"; key: string; label: string }
  /** `pos`: the line's place among the lines (dividers not counted). */
  | { kind: "msg"; key: string; m: ConversationMessage; pos: number };

/** The spool as one flat list, oldest first, a divider heading each day. */
export function spoolItems(
  messages: readonly ConversationMessage[],
  now: number = Date.now(),
): SpoolItem[] {
  const heads = dayHeads(messages, now);
  const out: SpoolItem[] = [];
  const days = new Set<string>();
  messages.forEach((m, pos) => {
    const head = heads[pos];
    const key = msgKey(m);
    if (head) {
      // Keyed by its day, so trimming the day's first line keeps the same divider; a day seen
      // twice (out-of-order times) falls back to the line's own key.
      let dk = `day:${dayStart(m.createdAtMs)}`;
      if (days.has(dk)) dk = `day:${key}`;
      days.add(dk);
      out.push({ kind: "day", key: dk, label: head });
    }
    out.push({ kind: "msg", key, m, pos });
  });
  return out;
}

/**
 * Whether `m` is news against a snapshot of what was shown: not among `seen`, sent by someone
 * else, and no older than `since` (the newest time shown then), so older history filled in
 * later is not news. The "N new" count and the arrival announcement both use this.
 */
export function isNewSince(
  m: ConversationMessage,
  seen: ReadonlySet<string>,
  me: Self,
  since: number = -Infinity,
): boolean {
  return !seen.has(msgKey(m)) && !isSelf(me, m.from) && !(m.createdAtMs < since);
}

/**
 * Lines below a reader who scrolled up: news since they left the bottom (see
 * {@link isNewSince}). `seen` null means they are at the bottom.
 */
export function unseenBelow(
  messages: readonly ConversationMessage[],
  seen: ReadonlySet<string> | null,
  me: Self,
  since: number = -Infinity,
): number {
  if (!seen) return 0;
  let n = 0;
  for (const m of messages) if (isNewSince(m, seen, me, since)) n++;
  return n;
}

/** Messages that are news against `before` (see {@link isNewSince}); none without one. */
export function newArrivals(
  before: ReadonlySet<string> | null,
  messages: readonly ConversationMessage[],
  me: Self,
  since: number = -Infinity,
): ConversationMessage[] {
  if (!before) return [];
  return messages.filter((m) => isNewSince(m, before, me, since));
}

/** The polite screen-reader line for new mail: who sent it, or how many. */
export function arrivalNote(arrived: readonly ConversationMessage[], dir: Directory): string {
  if (arrived.length === 0) return "";
  if (arrived.length === 1) return `New message from ${labelOf(dir, arrived[0]!.from)}`;
  return `${arrived.length} new messages`;
}

export type ClaimBadge =
  | { kind: "queued"; label: string }
  | { kind: "claimed"; label: string }
  | { kind: "withdrawn"; label: string };

/** A role message's claim state. Reading never claims; only an agent's claim does. */
export function claimBadge(m: ConversationMessage, dir: Directory): ClaimBadge | null {
  if (!m.toRole) return null;
  const d = m.delivery;
  if (!d || d.state === "pending") return { kind: "queued", label: "queued · unclaimed" };
  if (d.state === "withdrawn") return { kind: "withdrawn", label: "withdrawn" };
  const who = d.claimedBy ? participant(dir, d.claimedBy).name : "an agent";
  return { kind: "claimed", label: `claimed by ${who}` };
}

/**
 * Every role the inbox knows of, with its live holder and a note on how pickup works. `always`
 * names roles to list even when nobody holds them and nothing was sent to them yet.
 */
export function roleSummaries(
  dir: Directory,
  messages: readonly ConversationMessage[],
  known: readonly RoleSummary[] = [],
  always: readonly string[] = [],
): RoleSummary[] {
  const byRole = new Map<string, RoleSummary>();
  for (const r of known) byRole.set(r.role, r);
  const pendingOf = (role: string) =>
    messages.filter((m) => m.toRole === role && (!m.delivery || m.delivery.state === "pending"))
      .length;
  const derive = (role: string): RoleSummary => {
    const teams = [
      ...new Map(
        messages
          .filter((m) => m.toRole === role)
          .map((m) => [`${m.team.org}:${m.team.team}`, m.team]),
      ).values(),
    ];
    const holders = [...dir.values()].filter((p) => isLive(p) && p.roles.includes(role));
    const holder = holders[0] ?? null;
    const where = holder?.machine ? ` on ${holder.machine}` : "";
    return {
      role,
      team: teams.length === 1 ? teams[0] : teams.length > 1 ? null : undefined,
      holder: holder?.key ?? null,
      holderCount: holders.length,
      pending: pendingOf(role),
      note: holder
        ? `held by ${holder.name}${where}`
        : "no live holder · queued until an agent claims it",
    };
  };
  for (const p of dir.values())
    for (const r of heldRoles(p)) if (!byRole.has(r)) byRole.set(r, derive(r));
  for (const m of messages)
    if (m.toRole && !byRole.has(m.toRole)) byRole.set(m.toRole, derive(m.toRole));
  for (const r of always) if (!byRole.has(r)) byRole.set(r, derive(r));
  return [...byRole.values()];
}

/** The composer's default recipient for a selected thread. */
export function defaultRecipient(
  threadKey: string | null,
  me: Self,
  held: readonly string[],
  messages: readonly ConversationMessage[],
): string | null {
  if (!threadKey) return null;
  if (typeof me !== "string") {
    // The human writes to the agent being looked at: its queue, or the session of its own that
    // last spoke in this conversation — never, on its behalf, to whoever it was talking with.
    if (isSelf(me, threadKey) || threadKey === HUMAN) return threadKey;
    // Its own queue; another agent's queue its sessions wrote to is answered to the session
    // of its own that last wrote there (else its own queue), not to the other agent.
    if (isRoleKey(threadKey)) {
      if (held.includes(threadKey.slice(1))) return threadKey;
      const own = [...messages].reverse().find((m) => isSelf(me, m.from));
      return own ? own.from : held[0] ? `#${held[0]}` : null;
    }
    const last = [...messages].reverse().find((m) => isSelf(me, m.from) || isSelf(me, m.to));
    return last ? replyTarget(last, me, held) : threadKey;
  }
  // A session's conversation with you: the human (who the composer always sends as) writes
  // to that session, not to themselves — wherever the answer would otherwise go to "you".
  const target = sessionDefault(threadKey, me, held, messages);
  return target === HUMAN && me !== HUMAN ? me : target;
}

function sessionDefault(
  threadKey: string,
  me: string,
  held: readonly string[],
  messages: readonly ConversationMessage[],
): string {
  if (threadKey === HUMAN) return HUMAN;
  // A queue this session holds is answered to whoever queued the message, not the queue.
  if (isRoleKey(threadKey) && held.includes(threadKey.slice(1))) {
    const role = threadKey.slice(1);
    const last = [...messages].reverse().find((m) => m.toRole === role && !isSelf(me, m.from));
    if (last) return repliesGoTo(last);
  }
  // The other side's latest message may say where answers go (its Reply-To).
  const theirs = [...messages].reverse().find((m) => m.from === threadKey);
  return theirs ? repliesGoTo(theirs) : threadKey;
}

/** The routing line under the To picker: what happens to the message once sent. */
export function routingHint(
  to: string | null,
  dir: Directory,
  roles: readonly RoleSummary[],
): { text: string; role: boolean } {
  if (!to) return { text: "Choose who gets this", role: false };
  if (isRoleKey(to)) {
    const r = roles.find((x) => x.role === to.slice(1));
    return {
      role: true,
      text: r?.holder
        ? `Queued on ${to} · ${participant(dir, r.holder).name} holds it`
        : `Queued on ${to} · no live holder yet, it waits until an agent claims it`,
    };
  }
  const p = participant(dir, to);
  if (p.kind === "human") return { text: "Goes to your own inbox.", role: false };
  const check = sendCheck(to, dir, []);
  if (check.kind === "ended") return { text: check.reason, role: false };
  if (check.kind === "offline") return { text: check.hint, role: false };
  return { text: "Delivered to the inbox. An idle session is woken to read it.", role: false };
}

export interface WireColumn {
  key: string;
  queue: boolean;
}

/**
 * Lifelines for the Wire lens: `first` (this tab, or the human) on the left, everyone else in
 * order of first appearance (or `order` when given), then a dashed lane per role queue.
 */
export function wireColumns(
  messages: readonly ConversationMessage[],
  first: string | null,
  order?: readonly string[],
): WireColumn[] {
  const seen: string[] = first ? [first] : [];
  for (const m of messages)
    for (const k of [m.from, m.to]) if (k && !seen.includes(k)) seen.push(k);
  const people = order ? order.filter((k) => seen.includes(k) || k === first) : seen;
  for (const k of seen) if (!people.includes(k)) people.push(k);
  const roles = [...new Set(messages.filter((m) => m.toRole).map((m) => m.toRole!))];
  return [
    ...people.map((key) => ({ key, queue: false })),
    ...roles.map((r) => ({ key: `#${r}`, queue: true })),
  ];
}

export interface WireArrow {
  message: ConversationMessage;
  /** Percent offsets across the lifeline grid. */
  leftPct: number;
  widthPct: number;
  tickPct: number;
  /** "self": a note to oneself, drawn as a loop off its own lifeline. */
  dir: "r" | "l" | "self";
  kinds: Array<"term" | "q" | "human">;
}

export function wireArrows(
  columns: readonly WireColumn[],
  messages: readonly ConversationMessage[],
): WireArrow[] {
  const n = columns.length;
  if (n === 0) return [];
  const colW = 100 / n;
  const idx = (k: string) => columns.findIndex((c) => c.key === k);
  const out: WireArrow[] = [];
  for (const m of messages) {
    const a = idx(m.from);
    const b = idx(toKeyOf(m));
    if (a < 0 || b < 0) continue;
    const lo = Math.min(a, b);
    const hi = Math.max(a, b);
    const kinds: WireArrow["kinds"] = [];
    if (m.via === "terminal") kinds.push("term");
    if (m.toRole) kinds.push("q");
    if (m.to === HUMAN) kinds.push("human");
    if (a === b) {
      out.push({
        message: m,
        leftPct: colW * a + colW / 2,
        widthPct: colW * 0.35,
        tickPct: colW * a + colW / 2,
        dir: "self",
        kinds,
      });
      continue;
    }
    out.push({
      message: m,
      leftPct: colW * lo + colW / 2,
      widthPct: colW * (hi - lo),
      tickPct: colW * a + colW / 2,
      dir: b > a ? "r" : "l",
      kinds,
    });
  }
  return out;
}

/** The same team: its org and its name. */
export const sameTeam = (a: InboxTeam, b: InboxTeam): boolean =>
  a.org === b.org && a.team === b.team;

/**
 * Whether the composer may send to `to`. A session id names one terminal launch, and a
 * relaunched agent gets a new one, so mail to an ended session would never be read: sending is
 * blocked, offering the role it held (`role`) instead. A session whose machine is offline is
 * still alive and reads its mail once the machine reconnects. Anyone else (a role, the human,
 * a session never seen) is fine.
 */
export type SendCheck =
  | { kind: "ok" }
  | { kind: "offline"; hint: string }
  | { kind: "ended"; name: string; reason: string; role: string | null };

/** A saved agent's role inbox: the role its own sessions (`members`) stand for. */
export interface RoleScope {
  role: string;
  members: Self;
}

export function sendCheck(
  to: string | null,
  dir: Directory,
  messages: readonly ConversationMessage[],
  scope?: RoleScope | null,
): SendCheck {
  if (!to || isRoleKey(to)) return { kind: "ok" };
  const p = participant(dir, to);
  if (p.kind !== "session") return { kind: "ok" };
  if (p.state === "offline")
    return {
      kind: "offline",
      hint: `Delivered when ${machineLabel(p)} reconnects`,
    };
  if (p.state !== "exited") return { kind: "ok" };
  return {
    kind: "ended",
    name: p.name,
    reason: `${p.name} ended — messages to it won't be read`,
    role: sessionRole(to, dir, messages, scope),
  };
}

/**
 * The role a session held: its known registration, else one its messages show (a role message it
 * claimed, or a `replyTo` naming a role), else the scope's role when the session is one of
 * the scoped agent's own (another agent's session never stands for this agent's role).
 */
export function sessionRole(
  key: string,
  dir: Directory,
  messages: readonly ConversationMessage[],
  scope?: RoleScope | null,
): string | null {
  const held = participant(dir, key).roles[0];
  if (held) return held;
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i]!;
    if (m.toRole && m.delivery?.claimedBy === key) return m.toRole;
    if (m.from === key) {
      const r = replyToKey(m);
      if (isRoleKey(r)) return r.slice(1);
    }
  }
  return scope?.role && isSelf(scope.members, key) ? scope.role : null;
}

/** Where a Reply aimed at `key` goes: an ended session's role when it held one, else `key`. */
export function replyKeyFor(
  key: string,
  dir: Directory,
  messages: readonly ConversationMessage[],
  scope?: RoleScope | null,
): string {
  const check = sendCheck(key, dir, messages, scope);
  return check.kind === "ended" && check.role ? `#${check.role}` : key;
}
