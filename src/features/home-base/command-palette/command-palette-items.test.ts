// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";

import type { Team } from "@/shared/types/domain";

import {
  buildEpicFileItems,
  buildStaticItems,
  CP_TIER,
  decodeSelection,
  dispatchSelection,
  encodeSelection,
  type CommandPaletteHandlers,
  type CommandPaletteSelection,
} from "./command-palette-items";

function team(name: string, repos: Array<[string, string]> = []): Team {
  return {
    name,
    source_repos: repos.map(([owner, repo]) => ({ owner, repo })),
  };
}

describe("encode/decode round-trip", () => {
  const cases: CommandPaletteSelection[] = [
    { kind: "team", teamName: "Platform Team" }, // name with a space
    { kind: "repo", teamName: "web app", owner: "acme", repo: "web-ui" },
    { kind: "epic", teamName: "core", filePath: "/root/_team/core/content/epic-1/epic-1.md" },
    { kind: "account", userId: 683658 },
    { kind: "create-team" },
    { kind: "attach-repo" },
    { kind: "open-shortcuts" },
    { kind: "open-routines" },
    { kind: "open-agents" },
    { kind: "open-scratch-space" },
    { kind: "add-machine" },
  ];
  it.each(cases)("survives %o", (sel) => {
    expect(decodeSelection(encodeSelection(sel))).toEqual(sel);
  });

  it("returns null for an unrecognized or malformed id", () => {
    expect(decodeSelection("bogus")).toBeNull(); // unknown kind
    expect(decodeSelection("team")).toBeNull(); // known kind, missing payload
    // account kind with a non-numeric user id → rejected by the finite guard.
    expect(decodeSelection(encodeSelection({ kind: "account", userId: NaN }))).toBeNull();
  });
});

describe("buildStaticItems", () => {
  const inputs = {
    teams: [team("alpha", [["acme", "web"]]), team("bravo", [])],
    accounts: [{ userId: 1, label: "octocat" }],
    recentEpics: [
      { teamName: "alpha", fullPath: "/r/_team/alpha/content/e/e.md", relativePath: "e/e.md" },
    ],
    canAttach: true,
    canAddMachine: false,
    routinesNeedingYou: 0,
  };

  it("offers Add machine whenever it can run, and never a machine switch", () => {
    const labels = (i: typeof inputs) => buildStaticItems(i).map((x) => x.label);
    expect(labels(inputs)).not.toContain("Add machine…");
    const withRegistry = labels({ ...inputs, canAddMachine: true });
    expect(withRegistry).toContain("Add machine…");
    expect(withRegistry).not.toContain("Switch machine…");
  });

  it("indexes every team passed (curation is the caller's concern — G1/AC-2)", () => {
    // A curated-out team is passed here the same as any other; the builder must
    // not drop it. (The mount guarantees it passes org.teams, not visibleTeams.)
    const items = buildStaticItems({ ...inputs, teams: [team("shown"), team("hidden")] });
    const teamIds = items
      .map((i) => decodeSelection(i.id))
      .filter((s) => s?.kind === "team")
      .map((s) => (s as { teamName: string }).teamName);
    expect(teamIds).toEqual(["shown", "hidden"]);
  });

  it("dedupes source repos by owner/repo", () => {
    const items = buildStaticItems({
      ...inputs,
      teams: [
        team("alpha", [
          ["acme", "web"],
          ["acme", "web"],
          ["acme", "api"],
        ]),
      ],
    });
    const repos = items.filter((i) => decodeSelection(i.id)?.kind === "repo");
    expect(repos).toHaveLength(2);
  });

  it("emits create-team, attach-repo, and per-account switch actions", () => {
    const items = buildStaticItems(inputs);
    const kinds = items.map((i) => decodeSelection(i.id)?.kind);
    expect(kinds).toContain("create-team");
    expect(kinds).toContain("attach-repo");
    expect(kinds).toContain("account");
  });

  it("omits the attach-repo action when nothing can be attached", () => {
    const items = buildStaticItems({ ...inputs, canAttach: false });
    expect(items.some((i) => decodeSelection(i.id)?.kind === "attach-repo")).toBe(false);
  });

  it("always offers the keyboard-shortcuts launcher (OC-17 — the surface is global)", () => {
    const withTeam = buildStaticItems(inputs);
    const noTeam = buildStaticItems({ ...inputs, canAttach: false });
    const shortcuts = withTeam.find((i) => decodeSelection(i.id)?.kind === "open-shortcuts");
    expect(shortcuts?.label).toBe("View keyboard shortcuts…");
    expect(shortcuts?.tier).toBe(CP_TIER.action);
    expect(noTeam.some((i) => decodeSelection(i.id)?.kind === "open-shortcuts")).toBe(true);
  });

  it("always offers the Scratch space (SLW-PT-21)", () => {
    const item = buildStaticItems({ ...inputs, canAttach: false }).find(
      (i) => decodeSelection(i.id)?.kind === "open-scratch-space",
    );
    expect(item?.label).toBe("Private");
    expect(item?.tier).toBe(CP_TIER.action);
  });

  it("always offers Schedule (routines live in the Agents tab), carrying the needs-you count only above zero", () => {
    const routines = (n: number) =>
      buildStaticItems({ ...inputs, canAttach: false, routinesNeedingYou: n }).find(
        (i) => decodeSelection(i.id)?.kind === "open-routines",
      );
    expect(routines(0)?.label).toBe("Schedule");
    expect(routines(0)?.sublabel).toBe("action · routines, schedule, timers");
    expect(routines(2)?.sublabel).toBe("action · routines, schedule, timers · 2 need you");
    expect(routines(2)?.tier).toBe(CP_TIER.action);
  });

  it("always offers Agents", () => {
    const agents = buildStaticItems({ ...inputs, canAttach: false }).find(
      (i) => decodeSelection(i.id)?.kind === "open-agents",
    );
    expect(agents?.label).toBe("Agents");
    expect(agents?.tier).toBe(CP_TIER.action);
  });

  it("offers no widget gallery — custom panes are opened by name", () => {
    const labels = buildStaticItems(inputs).map((i) => i.label);
    expect(labels.some((l) => /widget/i.test(l))).toBe(false);
  });

  it("gives recent files the object tier without changing empty-query insertion order", () => {
    const items = buildStaticItems(inputs);
    const recent = items.find((i) => decodeSelection(i.id)?.kind === "epic")!;
    const action = items.find((i) => decodeSelection(i.id)?.kind === "create-team")!;
    const object = items.find((i) => decodeSelection(i.id)?.kind === "team")!;
    expect(recent.tier).toBe(object.tier);
    expect(action.tier).toBeLessThan(object.tier!);
    expect(items.indexOf(recent)).toBeLessThan(items.indexOf(action));
    expect(items.indexOf(action)).toBeLessThan(items.indexOf(object));
  });
});

describe("buildEpicFileItems", () => {
  const entries = [
    {
      teamName: "alpha",
      name: "epic-1.md",
      relativePath: "e1/epic-1.md",
      fullPath: "/r/e1/epic-1.md",
    },
    {
      teamName: "alpha",
      name: "epic-2.md",
      relativePath: "e2/epic-2.md",
      fullPath: "/r/e2/epic-2.md",
    },
  ];

  it("builds one object-tier item per file", () => {
    const items = buildEpicFileItems(entries, new Set());
    expect(items).toHaveLength(2);
    expect(items.every((i) => i.tier === CP_TIER.object)).toBe(true);
  });

  it("excludes files already present as recent rows (no double-listing — R2)", () => {
    const recentId = encodeSelection({
      kind: "epic",
      teamName: "alpha",
      filePath: "/r/e1/epic-1.md",
    });
    const items = buildEpicFileItems(entries, new Set([recentId]));
    expect(items).toHaveLength(1);
    expect(items[0].label).toBe("epic-2.md");
  });

  it("collapses duplicate paths within the walk", () => {
    const items = buildEpicFileItems([entries[0], entries[0]], new Set());
    expect(items).toHaveLength(1);
  });
});

describe("dispatchSelection", () => {
  function handlers(): CommandPaletteHandlers & { calls: string[] } {
    const calls: string[] = [];
    return {
      calls,
      openTeam: (t) => calls.push(`openTeam:${t}`),
      attachRepo: (t) => calls.push(`attachRepo:${t}`),
      openEpicFile: (t, f) => calls.push(`openEpicFile:${t}:${f}`),
      switchAccount: (id) => calls.push(`switchAccount:${id}`),
      createTeam: () => calls.push("createTeam"),
      openPane: (id) => calls.push(`openPane:${id}`),
      runCommand: (id) => calls.push(`runCommand:${id}`),
      openShortcuts: () => calls.push("openShortcuts"),
      openRoutines: () => calls.push("openRoutines"),
      openAgents: () => calls.push("openAgents"),
      openScratchSpace: () => calls.push("openScratchSpace"),
      addMachine: () => calls.push("addMachine"),
    };
  }

  it("routes each object/action kind to the matching existing handler (AC-5)", () => {
    const h = handlers();
    dispatchSelection(encodeSelection({ kind: "team", teamName: "alpha" }), h, null);
    dispatchSelection(
      encodeSelection({ kind: "repo", teamName: "alpha", owner: "a", repo: "b" }),
      h,
      null,
    );
    dispatchSelection(
      encodeSelection({ kind: "epic", teamName: "alpha", filePath: "/x.md" }),
      h,
      null,
    );
    dispatchSelection(encodeSelection({ kind: "account", userId: 7 }), h, null);
    dispatchSelection(encodeSelection({ kind: "create-team" }), h, null);
    dispatchSelection(encodeSelection({ kind: "add-machine" }), h, null);
    expect(h.calls).toEqual([
      "openTeam:alpha",
      "attachRepo:alpha",
      "openEpicFile:alpha:/x.md",
      "switchAccount:7",
      "createTeam",
      "addMachine",
    ]);
  });

  it("offers each custom right pane as an Open pane row that reveals its panel", () => {
    const items = buildStaticItems({
      teams: [],
      accounts: [],
      recentEpics: [],
      canAttach: false,
      canAddMachine: false,
      routinesNeedingYou: 0,
      panes: [
        { panelId: "pane:acme/api:backend/board", title: "Board", layer: "backend" },
        { panelId: "pane:private:acme/api/board", title: "Board · Private", layer: "Private" },
      ],
    });
    const rows = items.filter((i) => i.label.startsWith("Open pane: "));
    expect(rows.map((r) => [r.label, r.sublabel])).toEqual([
      ["Open pane: Board", "pane · backend"],
      ["Open pane: Board · Private", "pane · Private"],
    ]);
    const h = handlers();
    expect(dispatchSelection(rows[1].id, h, null)).toBe(true);
    expect(h.calls).toEqual(["openPane:pane:private:acme/api/board"]);
  });

  it("routes the keyboard-shortcuts launcher to its handler (OC-17)", () => {
    const h = handlers();
    expect(dispatchSelection("open-shortcuts", h, null)).toBe(true);
    expect(h.calls).toEqual(["openShortcuts"]);
  });

  it("routes the Scratch space entry to its opener (SLW-PT-21)", () => {
    const h = handlers();
    expect(dispatchSelection("open-scratch-space", h, null)).toBe(true);
    expect(h.calls).toEqual(["openScratchSpace"]);
  });

  it("routes the Routines entry to its opener", () => {
    const h = handlers();
    expect(dispatchSelection("open-routines", h, null)).toBe(true);
    expect(h.calls).toEqual(["openRoutines"]);
  });

  it("routes the Agents entry to its opener", () => {
    const h = handlers();
    expect(dispatchSelection("open-agents", h, null)).toBe(true);
    expect(h.calls).toEqual(["openAgents"]);
  });

  it("routes the generic attach-repo action to the injected target team", () => {
    const h = handlers();
    expect(dispatchSelection("attach-repo", h, "bravo")).toBe(true);
    expect(h.calls).toEqual(["attachRepo:bravo"]);
  });

  it("no-ops the generic attach-repo action when there is no target team", () => {
    const h = handlers();
    expect(dispatchSelection("attach-repo", h, null)).toBe(false);
    expect(h.calls).toEqual([]);
  });

  it("returns false and calls nothing for an unrecognized id", () => {
    const h = handlers();
    const spy = vi.fn();
    expect(dispatchSelection("garbage", h, "alpha")).toBe(false);
    expect(h.calls).toEqual([]);
    expect(spy).not.toHaveBeenCalled();
  });
});

describe("dashboard commands", () => {
  const Icon = () => null;
  const base = {
    teams: [],
    accounts: [],
    recentEpics: [],
    canAttach: false,
    canAddMachine: false,
    routinesNeedingYou: 0,
  };

  it("lists each active command with its dashboard, icon and chord, and runs it on pick", () => {
    const actionId = "dashboard:team:acme:core/inbox/archive";
    const items = buildStaticItems({
      ...base,
      commands: [
        { actionId, label: "Archive thread", dashboardTitle: "Inbox", icon: Icon, chord: "⌘⇧E" },
        { actionId: `${actionId}-2`, label: "Reply", dashboardTitle: "Inbox", chord: "" },
      ],
    });
    const archive = items.find((i) => i.label === "Archive thread")!;
    expect(archive).toMatchObject({
      sublabel: "Inbox",
      tier: CP_TIER.action,
      icon: Icon,
      chord: "⌘⇧E",
    });
    expect(items.find((i) => i.label === "Reply")!.chord).toBeUndefined();
    const calls: string[] = [];
    const ran = dispatchSelection(
      archive.id,
      { runCommand: (id: string) => calls.push(id) } as unknown as CommandPaletteHandlers,
      null,
    );
    expect(ran).toBe(true);
    expect(calls).toEqual([actionId]);
  });
});
