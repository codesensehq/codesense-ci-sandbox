// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  FX,
  fixtureMessages,
  fixtureRoles,
  fixtureSessionInbox,
  fixtureSessions,
} from "./fixtures";
import type { SessionListRow } from "@/tauri/commands";

import type { ConversationMessage } from "./api";
import {
  HUMAN,
  middleTruncate,
  plainPreview,
  messageRowPreview,
  repliesGoTo,
  replyToKey,
  shortHandle,
  buildDirectory,
  claimBadge,
  counterpart,
  defaultRecipient,
  direction,
  filterSpool,
  initials,
  labelOf,
  msgKey,
  normalizeMessage,
  replyTarget,
  roleSummaries,
  routingHint,
  replyKeyFor,
  sendCheck,
  sessionRole,
  formatClock,
  arrivalNote,
  dayHeads,
  newArrivals,
  spoolItems,
  unseenBelow,
  threadLanes,
  threadsFor,
  wireArrows,
  wireColumns,
} from "./model";

const NOW = new Date(2026, 9, 5, 9, 0).getTime();
const messages = fixtureMessages(NOW);
const dir = buildDirectory(fixtureSessions(), messages, { humanName: "Sam" });

describe("directory", () => {
  it("names the roster, the human and anyone only a message mentions", () => {
    expect(labelOf(dir, FX.child)).toBe("amelia-child-5plus5");
    expect(labelOf(dir, HUMAN)).toBe("you (Sam)");
    expect(labelOf(dir, "#reviewer")).toBe("#reviewer");
    const extra = buildDirectory([], [{ ...messages[0]!, from: "gone-1", fromName: "Gone" }]);
    expect(labelOf(extra, "gone-1")).toBe("Gone");
    expect(extra.get("gone-1")?.state).toBe("unknown");
  });

  it("marks a remote machine and keeps local sessions machine-less", () => {
    expect(dir.get(FX.rev)?.machine).toBe("buildbox");
    expect(dir.get(FX.t9)?.machine).toBeNull();
  });

  it("derives initials the way the mockup draws them", () => {
    expect(initials("impl-SW-AID-5")).toBe("I5");
    expect(initials("amelia-child-5plus5")).toBe("AC");
    expect(initials("Terminal 9")).toBe("T9");
  });

  it("takes initials from any script, not only ASCII", () => {
    expect(initials("équipe nord")).toBe("ÉN");
    expect(initials("调度 助手")).toBe("调助");
    expect(initials("---")).toBe("?");
    // Decomposed (NFD) names: a combining accent is part of its letter, not a word break.
    expect(initials("e\u0301quipe nord")).toBe("ÉN");
    expect(initials("Re\u0301mi Noe\u0308l")).toBe("RN");
    // A variation selector or modifier left from an emoji is not a word.
    expect(initials("\u2764\uFE0F ops")).toBe("O");
    expect(initials("\u{1F44B}\u{1F3FD} hi there")).toBe("HT");
  });
});

describe("threads", () => {
  it("groups a session's sent and received messages by counterpart, newest first", () => {
    const inbox = fixtureSessionInbox(FX.t9, NOW);
    const threads = threadsFor(inbox.messages, FX.t9);
    expect(threads.map((t) => t.key)).toEqual([FX.child]);
    expect(threads[0]!.messages.map((m) => m.id)).toEqual([8, 9, 10, 11]);
    expect(threads[0]!.unread).toBe(1);
  });

  it("puts a held role's queue in its own lane", () => {
    const inbox = fixtureSessionInbox(FX.rev, NOW);
    const lanes = threadLanes(threadsFor(inbox.messages, FX.rev), inbox.roles);
    expect(lanes.role.map((t) => t.key)).toEqual(["#reviewer"]);
    expect(lanes.rest.map((t) => t.key)).toEqual([FX.ctrl]);
  });

  it("counts the role queue as the counterpart of a role message", () => {
    const m = messages.find((x) => x.id === 7)!;
    expect(counterpart(m, FX.impl)).toBe("#reviewer");
    expect(counterpart(m, FX.rev)).toBe("#reviewer");
  });

  it("answers a held queue to whoever queued the message", () => {
    const inbox = fixtureSessionInbox(FX.rev, NOW);
    expect(defaultRecipient("#reviewer", FX.rev, ["reviewer"], inbox.messages)).toBe(FX.impl);
    expect(defaultRecipient("#architect", FX.rev, ["reviewer"], inbox.messages)).toBe("#architect");
  });
});

describe("mailspool", () => {
  const mine = fixtureSessionInbox(FX.t9, NOW).messages;

  it("marks typed, role, sent and received directions", () => {
    expect(direction(messages[7]!, FX.t9).glyph).toBe("⇢");
    expect(direction(messages[6]!, FX.impl).glyph).toBe("⤳");
    expect(direction(messages[5]!, FX.impl).glyph).toBe("→");
    expect(direction(messages[8]!, FX.t9).glyph).toBe("←");
  });

  it("filters by chip and by the prompt's from:/to:/text terms", () => {
    expect(filterSpool(mine, FX.t9, "terminal", "", dir).map((m) => m.id)).toEqual([8, 10]);
    expect(filterSpool(mine, FX.t9, "unread", "", dir).map((m) => m.id)).toEqual([11]);
    expect(filterSpool(mine, FX.t9, "all", "from:@amelia", dir).map((m) => m.id)).toEqual([9, 11]);
    expect(filterSpool(messages, HUMAN, "all", "to:#reviewer", dir).map((m) => m.id)).toEqual([7]);
    expect(filterSpool(mine, FX.t9, "all", '"cache"', dir).map((m) => m.id)).toEqual([11]);
  });
});

describe("claim and routing", () => {
  it("shows a queued role message as unclaimed until an agent claims it", () => {
    const m = messages.find((x) => x.id === 7)!;
    expect(claimBadge(m, dir)).toEqual({ kind: "queued", label: "queued · unclaimed" });
    expect(claimBadge({ ...m, delivery: { state: "claimed", claimedBy: FX.rev } }, dir)).toEqual({
      kind: "claimed",
      label: "claimed by review-DEMO-4",
    });
    expect(claimBadge(messages[0]!, dir)).toBeNull();
  });

  it("explains where each recipient's message goes", () => {
    const roles = roleSummaries(dir, messages, fixtureRoles());
    expect(routingHint(null, dir, roles).text).toBe("Choose who gets this");
    expect(routingHint("#reviewer", dir, roles)).toEqual({
      role: true,
      text: "Queued on #reviewer · review-DEMO-4 holds it",
    });
    expect(routingHint("#architect", dir, roles).text).toMatch(
      /no live holder yet, it waits until an agent claims it/,
    );
    expect(routingHint(FX.qa, dir, roles).text).toBe(
      "qa-DEMO-3 ended — messages to it won't be read",
    );
    expect(routingHint(HUMAN, dir, roles).text).toBe("Goes to your own inbox.");
    expect(routingHint(FX.child, dir, roles).text).toMatch(/idle session is woken/);
  });

  it("derives role queues from holders and messages when the rollup has not listed them", () => {
    const roles = roleSummaries(dir, messages);
    const names = roles.map((r) => r.role).sort();
    expect(names).toEqual(["architect", "reviewer"]);
    expect(roles.find((r) => r.role === "reviewer")).toMatchObject({ holder: null, pending: 1 });
    expect(roles.find((r) => r.role === "architect")).toMatchObject({ holder: null, pending: 1 });
  });
});

describe("wire", () => {
  it("draws this tab first, then participants, then dashed role lanes", () => {
    const inbox = fixtureSessionInbox(FX.impl, NOW);
    const cols = wireColumns(inbox.messages, FX.impl);
    expect(cols).toEqual([
      { key: FX.impl, queue: false },
      { key: FX.ctrl, queue: false },
      { key: HUMAN, queue: false },
      { key: "#reviewer", queue: true },
    ]);
    const arrows = wireArrows(cols, inbox.messages);
    const toReviewer = arrows.find((a) => a.message.id === 7)!;
    expect(toReviewer.dir).toBe("r");
    expect(toReviewer.kinds).toEqual(["q"]);
    expect(toReviewer.leftPct).toBeCloseTo(12.5);
    expect(toReviewer.widthPct).toBeCloseTo(75);
    expect(arrows.find((a) => a.message.id === 6)!.kinds).toEqual(["human"]);
    expect(arrows.find((a) => a.message.id === 3)!.dir).toBe("l");
  });

  it("draws a note to oneself as a loop on its own lifeline, not nothing", () => {
    const note = { ...messages[0]!, id: 900, from: HUMAN, to: HUMAN, toRole: null };
    const cols = wireColumns([note], null);
    const [loop] = wireArrows(cols, [note]);
    expect(loop?.dir).toBe("self");
    expect(loop?.leftPct).toBeCloseTo(50);
    expect(loop?.widthPct).toBeGreaterThan(0);
  });
});

describe("message identity", () => {
  it("keys a message by team and id, since ids repeat across team stores", () => {
    const m = messages[0]!;
    expect(msgKey(m)).toBe("alderco/default#1");
    expect(msgKey({ ...m, team: { org: "o", team: "other" } })).not.toBe(msgKey(m));
  });

  it("puts a withdrawn role message back on its queue and leaves others alone", () => {
    const m = messages.find((x) => x.id === 7)!;
    const withdrawn = {
      ...m,
      toRole: null,
      to: "reviewer",
      toName: "reviewer",
      delivery: { state: "withdrawn" as const, claimedBy: null },
    };
    expect(normalizeMessage(withdrawn)).toMatchObject({ toRole: "reviewer", to: null });
    expect(normalizeMessage(messages[0]!)).toBe(messages[0]);
  });
});

describe("session names", () => {
  const worker = "_worker/acme/team-context/default/demo-child";

  it("names a session the roster has not listed by the last part of its handle", () => {
    const d = buildDirectory([], [], { me: { handle: worker, roles: [] } });
    expect(labelOf(d, worker)).toBe("demo-child");
    expect(shortHandle(worker)).toBe("demo-child");
    expect(shortHandle("SW-AID-4#review")).toBe("SW-AID-4#review");
  });

  it("prefers the roster's display name, then a name the caller knows, over the handle", () => {
    const row = { ...fixtureSessions()[0]!, handle: worker, displayName: "" };
    expect(labelOf(buildDirectory([row], []), worker)).toBe("demo-child");
    const named = buildDirectory([], [], { me: { handle: worker, roles: [], name: "Demo" } });
    expect(labelOf(named, worker)).toBe("Demo");
    // A message whose sender name is just its handle reads by the short name too.
    const m = { ...messages[0]!, from: worker, fromName: worker };
    expect(labelOf(buildDirectory([], [m]), worker)).toBe("demo-child");
  });

  it("middle-truncates a long handle, keeping both ends", () => {
    expect(middleTruncate(worker, 24)).toBe("_worker/acme…/demo-child");
    expect(middleTruncate(worker, 24).length).toBe(24);
    expect(middleTruncate("short", 24)).toBe("short");
  });
});

describe("row previews", () => {
  it("reads a markdown report as one plain line: no heading marks, table rules or fences", () => {
    const text =
      "# Status\n\n## Workers\n\n| worker | state |\n|---|---|\n| repro | done |\n\n```sh\npnpm test\n```";
    expect(plainPreview(text)).toBe("Status · Workers · worker  state · repro  done · pnpm test");
  });

  it("shows the sender's summary when it set one, and an html body's words, never its markup", () => {
    const base = messages[0]!;
    expect(messageRowPreview({ ...base, text: "## Long\n\nreport", summary: " All\ngreen " })).toBe(
      "All green",
    );
    expect(
      messageRowPreview({
        ...base,
        contentType: "html",
        summary: null,
        text: "<html><head><title>Build</title></head><body><p>is <b>green</b></p></body></html>",
      }),
    ).not.toMatch(/[<>]/);
    // Text typed into a terminal reads as typed.
    expect(messageRowPreview({ ...base, via: "terminal", summary: "x", text: "pnpm test" })).toBe(
      "pnpm test",
    );
  });
});

describe("Reply-To", () => {
  const asked: ConversationMessage = {
    ...messages[0]!,
    from: FX.impl,
    to: HUMAN,
    toRole: null,
    via: null,
    replyTo: "role:manager",
  };

  it("reads a stored replyTo as a composer key", () => {
    expect(replyToKey(asked)).toBe("#manager");
    expect(replyToKey({ ...asked, replyTo: FX.rev })).toBe(FX.rev);
    expect(replyToKey({ ...asked, replyTo: "role: " })).toBeNull();
    expect(replyToKey({ ...asked, replyTo: null })).toBeNull();
    expect(replyToKey({ ...asked, via: "terminal" })).toBeNull();
  });

  it("aims a reply where the sender asked answers to go, never back at the human", () => {
    expect(repliesGoTo(asked)).toBe("#manager");
    expect(repliesGoTo({ ...asked, replyTo: HUMAN })).toBe(FX.impl);
    expect(repliesGoTo({ ...asked, replyTo: null })).toBe(FX.impl);
    // In a session's own view and in the human's view of a set of sessions alike.
    expect(replyTarget(asked, HUMAN)).toBe("#manager");
    expect(replyTarget(asked, new Set([FX.impl]))).toBe("#manager");
    // The composer's default for that conversation follows it too.
    expect(defaultRecipient(FX.impl, HUMAN, [], [asked])).toBe("#manager");
  });
});

describe("sending to an ended or offline session", () => {
  const withRows = (patch: Record<string, Partial<SessionListRow>>) =>
    buildDirectory(
      fixtureSessions().map((s) => ({ ...s, ...patch[s.handle] })),
      messages,
    );

  it("blocks an ended session without treating its display label as queue membership", () => {
    const d = withRows({ [FX.qa]: { role: "qa" } });
    expect(sendCheck(FX.qa, d, messages)).toEqual({
      kind: "ended",
      name: "qa-DEMO-3",
      reason: "qa-DEMO-3 ended — messages to it won't be read",
      role: null,
    });
    expect(replyKeyFor(FX.qa, d, messages)).toBe(FX.qa);
  });

  it("finds an ended session's role from a role message it claimed, else the scope's role", () => {
    const claimed: ConversationMessage = {
      ...messages[1]!,
      id: 50,
      toRole: "architect",
      delivery: { state: "claimed", claimedBy: FX.qa },
    };
    expect(sessionRole(FX.qa, dir, [...messages, claimed])).toBe("architect");
    expect(sessionRole(FX.qa, dir, messages)).toBeNull();
    const scope = { role: "builtin-codex", members: new Set([FX.qa]) };
    expect(sessionRole(FX.qa, dir, messages, scope)).toBe("builtin-codex");
    // Another agent's session (one with no role of its own) never stands for this agent's role.
    expect(sessionRole("another-agent-session", dir, messages, scope)).toBeNull();
  });

  it("finds an ended session's role from a role it named as its Reply-To", () => {
    const fromQa: ConversationMessage = {
      ...messages[1]!,
      id: 51,
      from: FX.qa,
      replyTo: "role:qa",
    };
    expect(sessionRole(FX.qa, dir, [...messages, fromQa])).toBe("qa");
  });

  it("blocks with no role when none is known, and a reply stays on the session", () => {
    const check = sendCheck(FX.qa, dir, messages);
    expect(check).toMatchObject({ kind: "ended", role: null });
    expect(replyKeyFor(FX.qa, dir, messages)).toBe(FX.qa);
  });

  it("allows an offline session with a quiet hint naming its machine", () => {
    const d = withRows({ [FX.rev]: { state: "offline" } });
    expect(sendCheck(FX.rev, d, messages)).toEqual({
      kind: "offline",
      hint: "Delivered when buildbox reconnects",
    });
    expect(routingHint(FX.rev, d, fixtureRoles()).text).toBe("Delivered when buildbox reconnects");
  });

  it("allows an unknown or live session, a role and the human, with no warning", () => {
    expect(sendCheck("someone-never-seen", dir, messages)).toEqual({ kind: "ok" });
    expect(sendCheck(FX.child, dir, messages)).toEqual({ kind: "ok" });
    expect(sendCheck("#qa", dir, messages)).toEqual({ kind: "ok" });
    expect(sendCheck(HUMAN, dir, messages)).toEqual({ kind: "ok" });
    expect(sendCheck(null, dir, messages)).toEqual({ kind: "ok" });
  });
});

describe("review round 1", () => {
  it("shows no clock for a timestamp it cannot read", () => {
    expect(formatClock(Number.NaN)).toBe("--:--");
  });
});

describe("review pr-inbox round 1", () => {
  const base = messages[0]!;
  const human = { ...base, from: HUMAN, to: FX.impl, toRole: null, replyTo: null, via: null };
  const fromImpl = { ...base, from: FX.impl, to: HUMAN, toRole: null, replyTo: null, via: null };

  it("aims a reply in a tab's conversation with you at that session, never at yourself", () => {
    expect(replyTarget(human, FX.impl)).toBe(FX.impl);
    expect(replyTarget(fromImpl, FX.impl)).toBe(FX.impl);
    // The human's own view still answers the session.
    expect(replyTarget(fromImpl, HUMAN)).toBe(FX.impl);
  });
});

describe("review pr-inbox round 5", () => {
  it("never aims an agent's composer at another agent's queue its session wrote to", () => {
    const base = messages[0]!;
    const toOther = {
      ...base,
      from: FX.impl,
      to: null,
      toRole: "reviewer",
      replyTo: null,
      via: null,
    };
    const self = new Set([FX.impl]);
    expect(defaultRecipient("#reviewer", self, ["implementer"], [toOther])).toBe(FX.impl);
    expect(defaultRecipient("#reviewer", self, ["implementer"], [])).toBe("#implementer");
    expect(defaultRecipient("#implementer", self, ["implementer"], [toOther])).toBe("#implementer");
    expect(replyTarget(toOther, self, ["implementer"])).toBe(FX.impl);
    // Its own queue still answers the queue.
    expect(replyTarget({ ...toOther, toRole: "implementer" }, self, ["implementer"])).toBe(
      "#implementer",
    );
  });
});

describe("long logs: day dividers and the jump to latest", () => {
  const at = (d: number, h: number) => new Date(2026, 9, d, h, 0).getTime();
  const msg = (id: number, createdAtMs: number, from: string = FX.child): ConversationMessage => ({
    ...messages[0]!,
    id,
    from,
    to: FX.t9,
    createdAtMs,
  });

  it("heads each day's first message: Today, Yesterday, then the date", () => {
    const log = [msg(1, at(1, 9)), msg(2, at(1, 10)), msg(3, at(4, 8)), msg(4, at(5, 7))];
    const heads = dayHeads(log, NOW);
    expect(heads[1]).toBeNull();
    expect(heads[2]).toBe("Yesterday");
    expect(heads[3]).toBe("Today");
    expect(heads[0]).toBe(
      new Date(at(1, 9)).toLocaleDateString(undefined, {
        weekday: "short",
        day: "numeric",
        month: "short",
      }),
    );
  });

  it("flattens the spool into day dividers and lines, keyed apart", () => {
    const log = [msg(1, at(4, 8)), msg(2, at(4, 9)), msg(3, at(5, 7))];
    const items = spoolItems(log, NOW);
    expect(items.map((i) => (i.kind === "day" ? i.label : i.m.id))).toEqual([
      "Yesterday",
      1,
      2,
      "Today",
      3,
    ]);
    expect(new Set(items.map((i) => i.key)).size).toBe(items.length);
    // A line knows its place among the lines, for aria-posinset.
    expect(items.filter((i) => i.kind === "msg").map((i) => i.kind === "msg" && i.pos)).toEqual([
      0, 1, 2,
    ]);
  });

  it("lists the newest 500 lines with one divider per day", () => {
    const log = Array.from({ length: 500 }, (_, i) => msg(i, at(1, 0) + i * 15 * 60_000));
    const items = spoolItems(log, NOW);
    expect(items.filter((i) => i.kind === "msg")).toHaveLength(500);
    // 500 lines a quarter-hour apart from Oct 1 run into Oct 6: six days.
    expect(items.filter((i) => i.kind === "day")).toHaveLength(6);
  });

  it("keeps a day's divider key when that day's first line is trimmed (the 500 cap)", () => {
    const log = [msg(1, at(4, 8)), msg(2, at(4, 9)), msg(3, at(5, 7))];
    const before = spoolItems(log, NOW)
      .filter((i) => i.kind === "day")
      .map((i) => i.key);
    const after = spoolItems(log.slice(1), NOW)
      .filter((i) => i.kind === "day")
      .map((i) => i.key);
    expect(after).toEqual(before);
  });

  it("gives lines with no valid time one divider between them, as before", () => {
    const log = [msg(1, NaN), msg(2, NaN), msg(3, at(5, 7))];
    expect(dayHeads(log, NOW).filter(Boolean)).toHaveLength(2);
  });

  it("counts lines below the reader that someone else sent since they scrolled up", () => {
    const log = [msg(1, at(5, 7)), msg(2, at(5, 8))];
    const seen = new Set(log.map(msgKey));
    const later = [...log, msg(3, at(5, 8)), msg(4, at(5, 8), FX.t9), msg(5, at(5, 8))];
    expect(unseenBelow(later, null, FX.t9)).toBe(0);
    expect(unseenBelow(later, seen, FX.t9)).toBe(2);
  });

  it("names what arrived for a polite announcement, and nothing on first load", () => {
    const before = [msg(1, at(5, 7))];
    const after = [...before, msg(2, at(5, 8)), msg(3, at(5, 8), FX.t9)];
    const arrived = newArrivals(new Set(before.map(msgKey)), after, FX.t9);
    expect(arrived.map((m) => m.id)).toEqual([2]);
    expect(arrivalNote(arrived, dir)).toBe("New message from amelia-child-5plus5");
    expect(arrivalNote([msg(2, 0), msg(5, 0)], dir)).toBe("2 new messages");
    expect(arrivalNote([], dir)).toBe("");
    expect(newArrivals(null, after, FX.t9)).toEqual([]);
    // Older history filled in below what was shown is not news.
    const backfill = [msg(9, at(4, 7)), ...before];
    expect(newArrivals(new Set(before.map(msgKey)), backfill, FX.t9, at(5, 7))).toEqual([]);
  });
});
