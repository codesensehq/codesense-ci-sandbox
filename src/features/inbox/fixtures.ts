// SPDX-License-Identifier: MIT
import type { MessageKind, SessionListRow } from "@/tauri/commands";

import type { ConversationMessage, RollupDto, RoleSummary, SessionInboxDto } from "./api";

// Demo data implementing the inbox read-model contract: a hand-started tab that spawned a
// child, a manager agent (a team-designed "controller") with two workers (one waiting on the human, one on a remote machine),
// an exited QA run and two role queues. Used by tests and, in development only, when the
// backend has no inbox command yet.

const TEAM = { org: "alderco", team: "default" };

/** Handles of the demo sessions. */
export const FX = {
  human: "human",
  t9: "shell-demo-1",
  child: "amelia-child-5plus5",
  ctrl: "ctrl-root",
  impl: "DEMO-5",
  rev: "DEMO-4#review",
  qa: "DEMO-3#qa",
} as const;

export function fixtureSessions(): SessionListRow[] {
  const row = (r: Partial<SessionListRow> & Pick<SessionListRow, "handle" | "displayName">) =>
    ({
      state: "running",
      launchedBy: null,
      unread: 0,
      oldestUnreadAt: null,
      ...r,
    }) as SessionListRow;
  return [
    row({ handle: FX.t9, displayName: "Terminal 9", agent: "claude · hand-started" }),
    row({
      handle: FX.child,
      displayName: "amelia-child-5plus5",
      state: "idle",
      launchedBy: FX.t9,
      agent: "builtin/claude",
    }),
    row({
      handle: FX.ctrl,
      displayName: "controller",
      agent: "team/controller",
    }),
    row({
      handle: FX.impl,
      displayName: "impl-DEMO-5",
      state: "needs-you",
      launchedBy: FX.ctrl,
      role: "implementer",
      agent: "builtin/claude-unattended",
    }),
    row({
      handle: FX.rev,
      displayName: "review-DEMO-4",
      launchedBy: FX.ctrl,
      role: "reviewer",
      machine: "buildbox",
      agent: "builtin/claude",
    }),
    row({
      handle: FX.qa,
      displayName: "qa-DEMO-3",
      state: "exited",
      launchedBy: FX.ctrl,
      machine: "buildbox",
      agent: "builtin/codex",
    }),
  ];
}

export function fixtureRoles(): RoleSummary[] {
  return [
    {
      role: "reviewer",
      holder: FX.rev,
      pending: 1,
      note: "held by review-DEMO-4 on buildbox · stays queued until it claims",
    },
    { role: "implementer", holder: FX.impl, pending: 0, note: "held by impl-DEMO-5" },
    {
      role: "architect",
      holder: null,
      pending: 1,
      note: "no live holder · queued until an agent claims it",
    },
    {
      role: "qa",
      holder: null,
      pending: 0,
      note: "no live holder · queued until an agent claims it",
    },
  ];
}

const NAMES: Record<string, string> = Object.fromEntries(
  fixtureSessions().map((s) => [s.handle, s.displayName]),
);
NAMES.human = "human";

/** Epoch ms of HH:MM today (local time), so the demo always reads as "today". */
function at(hhmm: string, now: number): number {
  const [h, m] = hhmm.split(":").map(Number);
  const d = new Date(now);
  d.setHours(h ?? 0, m ?? 0, 0, 0);
  return d.getTime();
}

type Seed = {
  id: number;
  t: string;
  from: string;
  to?: string;
  toRole?: string;
  text: string;
  read: boolean;
  via?: "terminal";
  /** What it asks of the human; `update` when unset. */
  kind?: MessageKind;
};

const SEEDS: Seed[] = [
  {
    id: 1,
    t: "07:58",
    from: FX.qa,
    to: FX.ctrl,
    text: "QA pass on DEMO-3: 41 checks green. Exiting.",
    read: true,
  },
  {
    id: 2,
    t: "08:05",
    from: FX.human,
    toRole: "architect",
    text: "Before DEMO-6: sketch how remote panes register with the pane registry. No rush.",
    // A role message reads as unread until an agent claims it.
    read: false,
  },
  {
    id: 3,
    t: "08:12",
    from: FX.ctrl,
    to: FX.impl,
    text: "Dispatch: DEMO-5, custom panes match built-ins. Worktree is ready at ~/worktrees/DEMO-5.",
    read: true,
  },
  {
    id: 4,
    t: "08:20",
    from: FX.rev,
    to: FX.ctrl,
    text: "DEMO-4 review done: 2 findings. pane-registry.ts:88 drops the title on rehydrate; tab-strip.tsx:212 keys on index.",
    read: true,
  },
  {
    id: 5,
    t: "08:21",
    from: FX.ctrl,
    to: FX.human,
    text: "2 workers running, 1 waiting on you (impl-DEMO-5).\n#architect has 1 unclaimed message — I will start an architect at 08:45 if it is still waiting.",
    read: false,
  },
  {
    id: 6,
    t: "08:24",
    from: FX.impl,
    to: FX.human,
    text: "Decision needed: keep legacy pane ids (pane:7) or migrate to stable keys? Migration touches 14 saved layouts.",
    read: false,
    kind: "needs-you",
  },
  {
    id: 7,
    t: "08:26",
    from: FX.impl,
    toRole: "reviewer",
    text: "DEMO-5 ready for review: branch feat/demo-5-pane-parity, 6 files, tests green.",
    read: false,
  },
  {
    id: 8,
    t: "08:29",
    from: FX.t9,
    to: FX.child,
    via: "terminal",
    text: "What is 5 + 5? Then tell me a joke.",
    read: true,
  },
  {
    id: 9,
    t: "08:30",
    from: FX.child,
    to: FX.t9,
    text: "5 + 5 = **10**\n\nWhy do programmers prefer dark mode? Because light attracts bugs!",
    read: true,
  },
  {
    id: 10,
    t: "08:31",
    from: FX.t9,
    to: FX.child,
    via: "terminal",
    text: "Thanks, that works.",
    read: true,
  },
  {
    id: 11,
    t: "08:32",
    from: FX.child,
    to: FX.t9,
    text: "5 + 5 = **10**\n\nNew joke: Why did the developer go broke? Because he used up all his cache!\n\nSession id used: `shell-mcp-1700000000000000000-1`",
    read: false,
  },
];

export function fixtureMessages(now: number = Date.now()): ConversationMessage[] {
  return SEEDS.map((s) => ({
    id: s.id,
    team: TEAM,
    from: s.from,
    fromName: NAMES[s.from] ?? s.from,
    to: s.to ?? null,
    toName: s.to ? (NAMES[s.to] ?? s.to) : null,
    toRole: s.toRole ?? null,
    text: s.text,
    contentType: s.via === "terminal" ? "plain" : "markdown",
    createdAtMs: at(s.t, now),
    archivedAtMs: null,
    inReplyTo: null,
    via: s.via ?? null,
    replyTo: null,
    summary: null,
    unread: !s.read,
    delivery: s.toRole ? { state: "pending", claimedBy: null } : null,
    kind: s.kind ?? "update",
  }));
}

const ROLE_OF: Record<string, string | undefined> = Object.fromEntries(
  fixtureSessions().map((s) => [s.handle, s.role]),
);

/**
 * The demo inbox of `handle`. A demo handle gets its own slice; any other handle (a real tab in
 * a dev build) stands in for the hand-started tab, so every tab shows a populated inbox.
 */
export function fixtureSessionInbox(handle: string, now: number = Date.now()): SessionInboxDto {
  const known = Object.values(FX).includes(handle as (typeof FX)[keyof typeof FX]);
  const me = known ? handle : FX.t9;
  const role = ROLE_OF[me];
  const roles = role ? [role] : [];
  const swap = (h: string | null) => (h === FX.t9 && !known ? handle : h);
  const messages = fixtureMessages(now)
    .filter((m) => m.from === me || m.to === me || (m.toRole !== null && roles.includes(m.toRole)))
    .map((m) => ({ ...m, from: swap(m.from) ?? m.from, to: swap(m.to) }));
  return { handle, roles, messages };
}

/**
 * A long agent report (headings, a wide table, a code block), shown in the development demo
 * only, so the inbox's message-scale markdown can be seen at pane width.
 */
function demoReport(now: number): ConversationMessage {
  return {
    id: 12,
    team: TEAM,
    from: FX.impl,
    fromName: NAMES[FX.impl] ?? FX.impl,
    to: FX.human,
    toName: "human",
    toRole: null,
    text: [
      "# DEMO-5 status",
      "",
      "## Files touched",
      "",
      "| file | change | tests | notes |",
      "|---|---|---|---|",
      "| src/features/panes/pane-registry.ts | +38 −12 | 64 passed | keys panes by stable id |",
      "| src/features/panes/tab-strip.tsx | +6 −3 | 12 passed | drops the index key |",
      "| src/features/panes/layout-migrate.ts | +51 | 9 passed | new |",
      "",
      "Run it with:",
      "",
      "```sh",
      "pnpm vitest run src/features/panes --reporter=verbose",
      "```",
    ].join("\n"),
    contentType: "markdown",
    createdAtMs: at("08:16", now),
    archivedAtMs: null,
    inReplyTo: null,
    via: null,
    replyTo: null,
    summary: null,
    unread: false,
    delivery: null,
    kind: "update",
  };
}

export function fixtureRollup(now: number = Date.now()): RollupDto {
  const messages = fixtureMessages(now);
  if (import.meta.env.MODE !== "test") {
    const report = demoReport(now);
    messages.splice(
      messages.findIndex((m) => m.createdAtMs > report.createdAtMs),
      0,
      report,
    );
  }
  return { messages, roles: fixtureRoles() };
}
