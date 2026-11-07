// SPDX-License-Identifier: MIT
import {
  agentPinId,
  agentRowKey,
  sanitizeAgentPins,
  sessionPinId,
  type AgentPin,
} from "@/shared/lib/agent-pins";
import { describe, expect, it } from "vitest";

import {
  pinIdOfRow,
  agentDetailLine,
  agentMachineLabel,
  agentRowLine,
  agentModelLabel,
  CREW_STACK_MAX,
  duplicateFaceTags,
  endedSessionPins,
  faceKey,
  listedAgentsOf,
  filteredRowKeys,
  flattenRows,
  matchesAgentSearch,
  needsYouHandles,
  needsYouKeys,
  paneTeamLabel,
  pinForRow,
  pinnedEntries,
  pinOfRow,
  rowMarkers,
  passesStatusFilter,
  THIS_MAC,
  type AgentFace,
  type AgentTreeNode,
  resolveAgentFace,
  resolveAgentRole,
  teamPieceHue,
  workspaceCrew,
} from "@/features/viewer/lib/agent-list-view";
import { BUILTIN_AVATAR_PACKS, BUILTIN_FUNNY_PACK, faceSource } from "@/shared/lib/avatar-packs";
import { AGENT_STATUSES, type AgentStatus } from "@/shared/lib/agent-status-config";
import type { AgentSessionRow } from "@/tauri/commands";

function row(handle: string, over: Partial<AgentSessionRow> = {}): AgentSessionRow {
  return {
    handle,
    sessionId: `sid-${handle}`,
    kind: "dashboard-launched",
    alive: true,
    backgrounded: false,
    startedWith: null,
    startedAtMs: 1_000,
    launcher: null,
    lastOutputMs: null,
    ...over,
  };
}

function node(r: AgentSessionRow, children: AgentTreeNode[] = []): AgentTreeNode {
  return { row: r, story: null, workspace: null, children };
}

describe("Needs you and filters", () => {
  const team = [
    row("controller", { startedAtMs: 1 }),
    row("manager", { launchedBy: "controller", startedAtMs: 2 }),
    row("worker-a", { launchedBy: "manager", startedAtMs: 3 }),
    row("worker-b", { launchedBy: "manager", startedAtMs: 4 }),
    row("solo", { startedAtMs: 0 }),
  ];
  const tree = () => [
    node(team[0], [node(team[1], [node(team[3]), node(team[2])])]),
    node(team[4]),
  ];
  const status = { "worker-a": "awaiting-answer", solo: "paused", manager: "done" } as const;
  const statusOf = (r: AgentSessionRow) => status[r.handle as keyof typeof status] ?? "running";

  it("flattens every row of the tree, launched ones included", () => {
    expect(flattenRows(tree()).map((r) => r.handle)).toEqual([
      "controller",
      "manager",
      "worker-b",
      "worker-a",
      "solo",
    ]);
  });

  it("collects the handles whose status waits on a person", () => {
    expect([...needsYouHandles(tree(), statusOf)].sort()).toEqual(["solo", "worker-a"]);
  });

  it("puts every status in exactly one of Working, Needs you and Idle", () => {
    const buckets = ["working", "needs", "idle"] as const;
    for (const st of AGENT_STATUSES) {
      const hits = buckets.filter((f) => passesStatusFilter(st, f));
      expect({ st, hits: hits.length }).toEqual({ st, hits: 1 });
    }
  });

  it("files errors and crashes under Needs you; parked, finished and unknown under Idle", () => {
    for (const st of ["failed", "crashed", "errored", "paused", "awaiting-answer"] as const) {
      expect(passesStatusFilter(st, "needs")).toBe(true);
    }
    for (const st of ["idle", "unknown", "done", "completed"] as const) {
      expect(passesStatusFilter(st, "idle")).toBe(true);
    }
  });

  it("matches every word of a search, in any order and case", () => {
    expect(matchesAgentSearch("Verify on staging · Fix login timeout", "LOGIN verify")).toBe(true);
    expect(matchesAgentSearch("Verify on staging", "verify prod")).toBe(false);
    expect(matchesAgentSearch("anything", "  ")).toBe(true);
  });

  it("keeps the agents a search and a status filter both pass", () => {
    const text = (n: AgentTreeNode) =>
      n.row.handle === "worker-a" ? "staging check" : n.row.handle;
    const keep = (query: string, s: "all" | "working" | "needs" | "idle") =>
      [...filteredRowKeys(tree(), "t", { query, status: s }, statusOf, text)].sort();
    expect(keep("staging", "all")).toEqual([agentRowKey("t", "worker-a")]);
    expect(keep("", "needs")).toEqual([agentRowKey("t", "solo"), agentRowKey("t", "worker-a")]);
    expect(keep("", "working")).toEqual([
      agentRowKey("t", "controller"),
      agentRowKey("t", "worker-b"),
    ]);
    expect(keep("staging", "working")).toEqual([]);
    expect(keep("", "idle")).toEqual([agentRowKey("t", "manager")]);
  });

  it("keys what it keeps by team and handle, so a twin in another team is its own agent", () => {
    const twin = (teamKey?: string): AgentTreeNode => ({
      row: row("twin"),
      story: null,
      workspace: null,
      children: [],
      ...(teamKey ? { teamKey } : {}),
    });
    const kept = filteredRowKeys(
      [twin(), twin("acme:ops")],
      "team-1",
      { query: "", status: "all" },
      statusOf,
      (n) => n.row.handle,
    );
    expect([...kept].sort()).toEqual([
      agentRowKey("acme:ops", "twin"),
      agentRowKey("team-1", "twin"),
    ]);
  });
});

describe("row detail line", () => {
  it("joins role, workspace, machine and model, leaving out what is unknown", () => {
    expect(
      agentDetailLine({
        role: "Worker",
        workspace: "Missing notifications",
        machine: "This Mac",
        model: "Claude",
      }),
    ).toBe("Worker · Missing notifications · This Mac · Claude");
    expect(
      agentDetailLine({ role: "Manager", workspace: null, machine: "This Mac", model: null }),
    ).toBe("Manager · This Mac");
  });

  it("a list row's line leaves the role to its piece and says what runs it and for how long", () => {
    const base = {
      team: null,
      role: "Worker",
      pieceShowsRole: true,
      workspace: null,
      machine: null,
      model: "Claude",
      elapsed: "18m",
      name: "Pane mockups",
      hasVia: false,
    };
    expect(agentRowLine(base)).toEqual({ lead: "", tail: "Claude · 18m" });
    // A Status row: team and workspace lead, ahead of where a launcher goes.
    expect(agentRowLine({ ...base, team: "riverside", workspace: "SV-3", hasVia: true })).toEqual({
      lead: "riverside · SV-3",
      tail: "Claude · 18m",
    });
    // A custom role's piece cannot say it, so the line does.
    expect(agentRowLine({ ...base, role: "Reviewer", pieceShowsRole: false }).lead).toBe(
      "Reviewer",
    );
    // With nothing else to say, the role stands in, unless it is the agent's own name.
    const bare = { ...base, model: null, elapsed: null };
    expect(agentRowLine(bare)).toEqual({ lead: "Worker", tail: "" });
    expect(agentRowLine({ ...bare, role: "Manager", name: "manager" })).toEqual({
      lead: "",
      tail: "",
    });
    expect(agentRowLine({ ...bare, hasVia: true })).toEqual({ lead: "", tail: "" });
    // A stopped run says so there before its role does.
    expect(agentRowLine({ ...bare, stopped: "Ended" })).toEqual({ lead: "Ended", tail: "" });
  });

  it("reads a model from the launch args, else names the CLI", () => {
    expect(agentModelLabel({ cli: "claude", args: ["--model", "opus"] })).toBe("opus");
    expect(agentModelLabel({ cli: "codex", args: ["--model=gpt-5"] })).toBe("gpt-5");
    expect(agentModelLabel({ cli: "claude" })).toBe("Claude");
    expect(agentModelLabel({ cli: "hermes", args: ["--model=first", "--mo=second"] })).toBe(
      "Hermes",
    );
    expect(agentModelLabel({ cli: null })).toBeNull();
  });
});

describe("agent roles", () => {
  it("honours a role the launch named, known or free", () => {
    const tree = { launched: 0 };
    expect(resolveAgentRole({ role: "manager" }, tree).piece).toBe("manager");
    expect(resolveAgentRole({ role: "Lead" }, tree).piece).toBe("manager");
    expect(resolveAgentRole({ role: "worker" }, { launched: 3 }).piece).toBe("worker");
    // A role named controller is an ordinary role name: no crown.
    expect(resolveAgentRole({ role: "controller" }, tree)).toEqual({
      piece: "custom",
      label: "Controller",
    });
    expect(resolveAgentRole({ role: "reviewer" }, tree)).toEqual({
      piece: "custom",
      label: "Reviewer",
    });
  });

  it("reads the role from the launch tree when the launch named none", () => {
    expect(resolveAgentRole({}, { launched: 2 })).toEqual({
      piece: "manager",
      label: "Manager · launched 2",
    });
    expect(resolveAgentRole({ role: "  " }, { launched: 0 }).piece).toBe("worker");
  });

  it("uses the projected child count when a worker launches an agent", () => {
    expect(resolveAgentRole(row("dev"), { launched: 0 }).piece).toBe("worker");
    expect(resolveAgentRole(row("dev"), { launched: 1 }).piece).toBe("manager");
  });
});

describe("agent faces", () => {
  const opts = { name: "Login lead" };
  const crew = {
    id: "crew",
    name: "Crew",
    scope: "team" as const,
    faces: [
      { id: "grumpy", ref: "crew:grumpy", label: "Grumpy", src: "data:grumpy" },
      { id: "calm", ref: "crew:calm", label: null, src: "data:calm" },
    ],
  };
  const faces = faceSource([crew, ...BUILTIN_AVATAR_PACKS], {
    agentAvatars: { "acme/team/reviewer": "builtin-funny:skeptical-goat" },
  });
  const image = (ref: string, src: string) => ({ kind: "image", ref, src });

  it("gives an agent named controller a dealt face like any other agent", () => {
    const ctl = row("/content#controller", { agentRevision: "acme/team/controller@2" });
    const face = resolveAgentFace(ctl, { ...opts, faces });
    expect(face.kind).toBe("image");
    // A face a person picked for it wins, as for any agent.
    expect(resolveAgentFace(ctl, { ...opts, faces, choice: { face: "crew:calm" } })).toEqual(
      image("crew:calm", "data:calm"),
    );
    // Its role comes from the launch tree, never a crown.
    expect(resolveAgentRole(ctl, { launched: 0 }).piece).toBe("worker");
  });

  it("uses a pack face, a colour or a short glyph the launch named", () => {
    expect(resolveAgentFace(row("a", { avatar: "crew:grumpy" }), { ...opts, faces })).toEqual(
      image("crew:grumpy", "data:grumpy"),
    );
    expect(resolveAgentFace(row("a", { avatar: "#3fb950" }), opts)).toEqual({
      kind: "mark",
      text: "LL",
      color: "#3fb950",
    });
    expect(resolveAgentFace(row("a", { avatar: "🦊" }), opts)).toEqual({
      kind: "mark",
      text: "🦊",
      color: null,
    });
  });

  it("ranks a person's pick over the launch, the launch over the agent's own default", () => {
    const reviewer = row("r", { agentRevision: "acme/team/reviewer@2" });
    expect(resolveAgentFace(reviewer, { ...opts, faces }).kind).toBe("image");
    expect(resolveAgentFace(reviewer, { ...opts, faces })).toMatchObject({
      ref: "builtin-funny:skeptical-goat",
    });
    const launched = { ...reviewer, avatar: "crew:calm" };
    expect(resolveAgentFace(launched, { ...opts, faces })).toMatchObject({ ref: "crew:calm" });
    expect(
      resolveAgentFace(launched, { ...opts, faces, choice: { face: "crew:grumpy" } }),
    ).toMatchObject({ ref: "crew:grumpy" });
  });

  it("falls through a face whose pack is not on this machine, never drawing a broken one", () => {
    const fallback = resolveAgentFace(row("s", { agentRevision: "acme/team/triage@1" }), {
      ...opts,
      faces,
    });
    for (const missing of ["tpb:ricky", "crew:nobody"]) {
      expect(
        resolveAgentFace(row("s", { agentRevision: "acme/team/triage@1", avatar: missing }), {
          ...opts,
          faces,
          choice: { face: "gone:face" },
        }),
      ).toEqual(fallback);
    }
  });

  it("deals from the built-in funny pack, the same face for every session of an agent", () => {
    const a = resolveAgentFace(row("s1", { agentRevision: "acme/team/triage@3" }), opts);
    const b = resolveAgentFace(row("s2", { agentRevision: "acme/team/triage@4" }), opts);
    expect(a).toEqual(b);
    expect(a).toMatchObject({ kind: "image" });
    expect(a.kind === "image" && a.ref.startsWith(`${BUILTIN_FUNNY_PACK}:`)).toBe(true);
    // An unknown avatar name falls through to the same fallback.
    expect(
      resolveAgentFace(row("s3", { agentRevision: "acme/team/triage@1", avatar: "unicorn" }), opts),
    ).toEqual(a);
  });

  it("deals from the team's default pack when it set one", () => {
    const teamFaces = faceSource([crew, ...BUILTIN_AVATAR_PACKS], { defaultPack: "crew" });
    for (const name of ["Docs sweep", "Triage", "Release", "QA"]) {
      const face = resolveAgentFace(row(name, { displayName: name }), {
        ...opts,
        faces: teamFaces,
      });
      expect(face.kind === "image" && face.ref.startsWith("crew:")).toBe(true);
    }
    // A default pack that is not here deals from the built-in funny pack.
    const gone = faceSource(BUILTIN_AVATAR_PACKS, { defaultPack: "tpb" });
    const face = resolveAgentFace(row("x", { displayName: "Docs sweep" }), {
      ...opts,
      faces: gone,
    });
    expect(face.kind === "image" && face.ref.startsWith(`${BUILTIN_FUNNY_PACK}:`)).toBe(true);
  });

  it("picks from the name when the launch named no agent", () => {
    const face = resolveAgentFace(row("x", { displayName: "Docs sweep" }), opts);
    expect(resolveAgentFace(row("y", { displayName: "Docs sweep" }), opts)).toEqual(face);
  });
});

describe("workspace crew", () => {
  const status =
    (by: Record<string, AgentStatus>) =>
    (r: AgentSessionRow): AgentStatus =>
      by[r.handle] ?? "running";
  const nodesOf = (rows: AgentSessionRow[]) =>
    rows.map((r) => ({ ...node(r), crewLead: r.handle === "lead" }));

  it("leads with the top of the tree that leads the most, then the rest in tree order", () => {
    const nodes = nodesOf([
      row("solo", { startedAtMs: 9 }),
      row("lead", { startedAtMs: 1 }),
      row("w1", { launchedBy: "lead", startedAtMs: 2 }),
      row("w2", { launchedBy: "lead", startedAtMs: 3 }),
    ]);
    const crew = workspaceCrew(nodes, status({}));
    expect(crew.lead?.handle).toBe("lead");
    expect(crew.crew.map((r) => r.handle).sort()).toEqual(["solo", "w1", "w2"]);
    expect(crew.more).toBe(0);
  });

  it("caps the stack and counts the rest", () => {
    const rows = [
      row("lead", { startedAtMs: 1 }),
      ...Array.from({ length: 5 }, (_, i) => row(`w${i}`, { launchedBy: "lead" })),
    ];
    const crew = workspaceCrew(nodesOf(rows), status({}));
    expect(crew.crew).toHaveLength(CREW_STACK_MAX);
    expect(crew.more).toBe(5 - CREW_STACK_MAX);
  });

  it("rolls up needs you, then an error or a crash, then work, then the quiet marks", () => {
    const rows = [row("lead"), row("w", { launchedBy: "lead" })];
    const mark = (st: Record<string, AgentStatus>) => workspaceCrew(nodesOf(rows), status(st)).mark;
    // the finer needs-you marks (question / permission / blocked)
    // replaced the generic `needs`; the finer working marks (thinking / tool)
    // replaced `working`; `idle` renamed to `ready`.
    expect(mark({ w: "awaiting-answer" })).toBe("question");
    expect(mark({ lead: "failed", w: "awaiting-answer" })).toBe("question");
    expect(mark({ lead: "failed" })).toBe("error");
    expect(mark({ w: "crashed" })).toBe("crashed");
    // Generic work means thinking; only confirmed tool work gets the tool mark.
    expect(mark({ lead: "idle" })).toBe("thinking");
    expect(mark({ lead: "idle", w: "tool-running" })).toBe("tool");
    expect(mark({ lead: "idle", w: "idle" })).toBe("ready");
    expect(mark({ lead: "done", w: "unknown" })).toBe("unknown");
    expect(workspaceCrew(nodesOf(rows), status({ w: "crashed" })).counts).toEqual({
      crashed: 1,
      thinking: 1,
    });
    expect(workspaceCrew([], status({})).lead).toBeNull();
  });

  it("gives each workspace its own team piece colour", () => {
    expect(teamPieceHue(0)).not.toBe(teamPieceHue(1));
    expect(teamPieceHue(6)).toBe(teamPieceHue(0));
  });
});

describe("duplicate-face number tags", () => {
  const img = (ref: string) => ({ kind: "image" as const, ref, src: `data:${ref}` });
  const entry = (handle: string, startedAtMs: number | null, face: AgentFace = img("p:owl")) => ({
    handle,
    startedAtMs,
    face,
  });

  it("numbers agents sharing a face by start time, and leaves a unique face untagged", () => {
    const tags = duplicateFaceTags([
      entry("late", 300),
      entry("early", 100),
      entry("solo", 50, img("p:cat")),
      entry("mid", 200),
    ]);
    expect(Object.fromEntries(tags)).toEqual({ early: 1, mid: 2, late: 3 });
    expect(tags.has("solo")).toBe(false);
  });

  it("is stable: an unknown start sorts last, ties go by handle, input order never matters", () => {
    const a = duplicateFaceTags([entry("b", null), entry("a", null), entry("c", 5)]);
    const b = duplicateFaceTags([entry("c", 5), entry("a", null), entry("b", null)]);
    expect(Object.fromEntries(a)).toEqual({ c: 1, a: 2, b: 3 });
    expect(Object.fromEntries(b)).toEqual(Object.fromEntries(a));
  });

  it("tells marks apart by text and colour, and counts one agent once", () => {
    const mark = (text: string, color: string | null) => ({ kind: "mark" as const, text, color });
    expect(faceKey(mark("LL", "#fff"))).not.toBe(faceKey(mark("LL", null)));
    expect(
      duplicateFaceTags([entry("x", 1, mark("LL", null)), entry("y", 2, mark("LL", "#fff"))]).size,
    ).toBe(0);
    expect(duplicateFaceTags([entry("x", 1), entry("x", 1)]).size).toBe(0);
  });
});

describe("pins", () => {
  const node = (r: AgentSessionRow, children: AgentTreeNode[] = []): AgentTreeNode => ({
    row: r,
    story: null,
    workspace: null,
    children,
  });

  it("pins only the row's own session, even when it runs a saved Agent", () => {
    expect(
      pinForRow("team-1", row("m", { agentRevision: "a/b/manager@3", story: "SW-1" }), "Manager"),
    ).toEqual({
      kind: "session",
      teamKey: "team-1",
      handle: "m",
      sessionId: "sid-m",
      name: "Manager",
    });
    expect(pinIdOfRow("team-1", row("m", { agentRevision: "a/b/manager@3" }))).toBe(
      sessionPinId("team-1", "m"),
    );
  });

  it("matches a pin to its own team's session: a twin handle in another team is not pinned", () => {
    const pin: AgentPin = {
      kind: "session",
      teamKey: "acme:ops",
      handle: "twin",
      sessionId: null,
      name: "twin",
    };
    const entries = pinnedEntries(
      [{ teamKey: "acme:ops", pin }],
      [
        { teamKey: "team-1", nodes: [node(row("twin"))] },
        {
          teamKey: "acme:ops",
          nodes: [{ ...node(row("twin", { sessionId: "sid-2" })), teamKey: "acme:ops" }],
        },
      ],
    );
    expect(entries.map((e) => [e.id, e.nodes.map((n) => n.row.sessionId)])).toEqual([
      [sessionPinId("acme:ops", "twin"), ["sid-2"]],
    ]);
  });

  it("matches a pin by its session id when the trees place its row in another team, and shows a session once", () => {
    const pin = (teamKey: string, handle: string, sessionId: string | null): AgentPin => ({
      kind: "session",
      teamKey,
      handle,
      sessionId,
      name: handle,
    });
    // acme's pinned session has no tab yet, so the trees place it in team-1.
    const trees = [{ teamKey: "team-1", nodes: [node(row("far", { sessionId: "sid-far" }))] }];
    const kept = { teamKey: "acme:ops", pin: pin("acme:ops", "far", "sid-far") };
    // An older duplicate, kept in team-1 by the same session.
    const dup = { teamKey: "team-1", pin: pin("team-1", "far", "sid-far") };
    const entries = pinnedEntries([kept, dup], trees);
    expect(entries.map((e) => [e.teamKey, e.id, e.nodes.map((n) => n.row.handle)])).toEqual([
      ["acme:ops", sessionPinId("acme:ops", "far"), ["far"]],
    ]);
    expect(pinOfRow([kept], "team-1", { handle: "far", sessionId: "sid-far" })).toBe(kept);
    // Its own team's pin first, then the session's.
    expect(pinOfRow([kept, dup], "team-1", { handle: "far", sessionId: "sid-far" })).toBe(dup);
    expect(pinOfRow([kept], "team-1", { handle: "far", sessionId: "sid-other" })).toBeNull();
    expect(pinOfRow([kept], "team-1", { handle: "far", sessionId: null })).toBeNull();
  });

  it("lists each pinned session once, in pin order; an ended session is not listed", () => {
    const m1 = row("m1", { agentRevision: "a/b/manager@1" });
    const m2 = row("m2", { agentRevision: "a/b/manager@2" });
    const trees = [{ teamKey: "team-1", nodes: [node(m1), node(m2)] }];
    const entries = pinnedEntries(
      [
        { kind: "session", teamKey: "team-1", handle: "m2", sessionId: null, name: "m2" },
        { kind: "session", teamKey: "team-1", handle: "gone", sessionId: null, name: "gone" },
        { kind: "session", teamKey: "team-1", handle: "m1", sessionId: null, name: "m1" },
      ].map((pin) => ({ teamKey: "team-1", pin: pin as AgentPin })),
      trees,
    );
    expect(entries.map((e) => [e.id, e.nodes.map((n) => n.row.handle)])).toEqual([
      [sessionPinId("team-1", "m2"), ["m2"]],
      [sessionPinId("team-1", "m1"), ["m1"]],
    ]);
  });

  it("drops a pinned session once it has no live row and no tab", () => {
    const pin = (handle: string, sessionId: string, teamKey = "t") =>
      ({ kind: "session", teamKey, handle, sessionId, name: "" }) as const;
    const pins = [
      pin("live", "s1"),
      pin("restoring", "s2"),
      pin("ended", "s3"),
      // Its handle runs, but in another team.
      pin("live", "s4", "other"),
    ];
    const listed = { keys: new Set([agentRowKey("t", "live")]), sessionIds: new Set(["s1"]) };
    expect(endedSessionPins(pins, listed, (sid) => sid === "s2")).toEqual([
      sessionPinId("t", "ended"),
      sessionPinId("other", "live"),
    ]);
  });

  it("keeps a pin whose session is listed under another team's key for now", () => {
    // A row with no tab and no story follows the active team: after a team switch its key names
    // the new team, but its session is the pinned one.
    const pin: AgentPin = {
      kind: "session",
      teamKey: "team-1",
      handle: "solo",
      sessionId: "sid-solo",
      name: "solo",
    };
    const listed = listedAgentsOf([
      { teamKey: "acme:ops", nodes: [node(row("solo", { sessionId: "sid-solo" }))] },
    ]);
    expect([...listed.keys]).toEqual([agentRowKey("acme:ops", "solo")]);
    expect(endedSessionPins([pin], listed, () => false)).toEqual([]);
    // Another session of the same handle there does not hold it.
    const other = listedAgentsOf([
      { teamKey: "acme:ops", nodes: [node(row("solo", { sessionId: "sid-new" }))] },
    ]);
    expect(endedSessionPins([pin], other, () => false)).toEqual([sessionPinId("team-1", "solo")]);
  });

  it("lists every agent of the trees by team and handle, and by session id", () => {
    const listed = listedAgentsOf([
      {
        teamKey: "team-1",
        nodes: [node(row("lead"), [node(row("dev", { sessionId: null }))])],
      },
      { teamKey: "x", nodes: [{ ...node(row("w")), teamKey: "acme:ops" }] },
    ]);
    expect([...listed.keys].sort()).toEqual([
      agentRowKey("acme:ops", "w"),
      agentRowKey("team-1", "dev"),
      agentRowKey("team-1", "lead"),
    ]);
    expect([...listed.sessionIds].sort()).toEqual(["sid-lead", "sid-w"]);
  });

  it("a pin holds one team's session: a saved pin of a whole Agent, or of no team, is dropped on load", () => {
    const pins = sanitizeAgentPins([
      { kind: "agent", agentKey: "acme/backend/triage", name: "Triage" },
      { kind: "session", handle: "old", sessionId: "s0", name: "Old" },
      { kind: "session", teamKey: "t", handle: "kept", sessionId: "s1", name: "Kept" },
      { kind: "session", teamKey: "t", handle: "kept", sessionId: "s1", name: "Again" },
    ]);
    expect(pins).toEqual([
      { kind: "session", teamKey: "t", handle: "kept", sessionId: "s1", name: "Kept" },
    ]);
    expect(pins.map(agentPinId)).toEqual([sessionPinId("t", "kept")]);
    expect(pinIdOfRow("t", { handle: "kept" })).toBe(agentPinId(pins[0]!));
    expect(endedSessionPins(pins, { keys: new Set(), sessionIds: new Set() }, () => false)).toEqual(
      [sessionPinId("t", "kept")],
    );
  });

  it("names a machine no list knows in words, never by its raw id", () => {
    const machines = [{ machineId: "box", label: "Studio" }];
    expect(agentMachineLabel("box", machines)).toBe("Studio");
    expect(agentMachineLabel("m-7f3a", machines)).toBe("a remote machine");
    expect(agentMachineLabel("m-7f3a", undefined)).toBe("a remote machine");
    expect(agentMachineLabel("local", machines)).toBe(THIS_MAC);
    expect(agentMachineLabel(null, machines)).toBe(THIS_MAC);
  });

  it("keeps one trailing marker in a narrow pane, most urgent first", () => {
    const base = {
      hidden: false,
      headless: false,
      pinned: false,
    };
    expect(rowMarkers({ ...base, hidden: true, pinned: true }, false)).toEqual(["hidden", "pin"]);
    expect(rowMarkers({ ...base, hidden: true, pinned: true }, true)).toEqual(["hidden"]);
    expect(rowMarkers({ ...base, pinned: true }, true)).toEqual(["pin"]);
    expect(rowMarkers(base, true)).toEqual([]);
  });

  it("marks the session a routine's wake is queued for: due, before the rest", () => {
    const base = {
      hidden: false,
      headless: false,
      pinned: false,
    };
    expect(rowMarkers({ ...base, due: true, headless: true, pinned: true }, false)).toEqual([
      "due",
      "headless",
      "pin",
    ]);
    expect(rowMarkers({ ...base, due: true, pinned: true }, true)).toEqual(["due"]);
    expect(rowMarkers({ ...base, due: true, hidden: true }, true)).toEqual(["due"]);
  });
});

describe("paneTeamLabel", () => {
  const orgName = (slug: string) => ({ acme: "Acme Inc" })[slug];
  it("names a team by its name alone when no other team shares it", () => {
    expect(paneTeamLabel("acme:ops", ["acme:ops", "acme:dev", "beta:qa"], orgName)).toBe("ops");
  });
  it("names it org › team (the org's display name, else its slug) when another org shares it", () => {
    const among = ["acme:ops", "beta:ops"];
    expect(paneTeamLabel("acme:ops", among, orgName)).toBe("Acme Inc › ops");
    expect(paneTeamLabel("beta:ops", among, orgName)).toBe("beta › ops");
  });
  it("names a key with no team by its org", () => {
    expect(paneTeamLabel("acme", ["acme:ops"], orgName)).toBe("Acme Inc");
  });
});

describe("needsYouKeys", () => {
  it("keys each waiting agent by its own team and handle, in every team and depth", () => {
    const node = (r: AgentSessionRow, over: Partial<AgentTreeNode> = {}): AgentTreeNode => ({
      row: r,
      story: null,
      workspace: null,
      children: [],
      ...over,
    });
    const lead = node(row("lead"), { children: [node(row("child"))] });
    const twin = node(row("lead", { sessionId: "sid-twin" }), { teamKey: "acme:ops" });
    const waits = new Set(["sid-child", "sid-twin"]);
    const keys = needsYouKeys(
      [
        { teamKey: "org:team", nodes: [lead] },
        { teamKey: "acme:dev", nodes: [twin] },
      ],
      (r) => (waits.has(r.sessionId ?? "") ? "awaiting-answer" : "running"),
    );
    // The twin names its own team, over the group it is listed in; the lead itself does not wait.
    expect([...keys].sort()).toEqual(
      [agentRowKey("org:team", "child"), agentRowKey("acme:ops", "lead")].sort(),
    );
  });
});
