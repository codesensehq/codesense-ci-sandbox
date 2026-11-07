// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { TeamFileEdit, TeamFileEditsStatus, TeamFileEditsWorkspace } from "@/tauri/commands";

import {
  counterSentence,
  counterVisible,
  discardSentence,
  fileActions,
  fileNote,
  plainError,
  publishAllPaths,
  workspaceLine,
} from "./team-edits";

const file = (
  path: string,
  kind: TeamFileEdit["kind"],
  extra: Partial<TeamFileEdit> = {},
): TeamFileEdit => ({
  path,
  kind,
  destination: { kind: "team", name: "backend" },
  teamPath: `/team/${path}`,
  ...extra,
});

const local = (
  files: TeamFileEdit[],
  extra: Partial<TeamFileEditsWorkspace> = {},
): TeamFileEditsWorkspace => ({
  target: { kind: "local", codeRepoPath: "/w/api" },
  name: "api",
  state: "checked",
  files,
  ...extra,
});

const status = (workspaces: TeamFileEditsWorkspace[], total: number): TeamFileEditsStatus => ({
  total,
  cleanWorkspaces: 0,
  workspaces,
});

describe("unsaved team edits words", () => {
  it("is hidden at zero and never shows a made-up zero", () => {
    expect(counterVisible(status([], 0))).toBe(false);
    expect(counterVisible(null)).toBe(false);
    expect(counterVisible(status([local([file("a.md", "changed")])], 1))).toBe(true);
    const unsure = status([local([], { state: "unknown", reason: "record unreadable" })], 0);
    expect(counterVisible(unsure)).toBe(true);
    expect(counterSentence(unsure)).toBe("Couldn't check 1 workspace for unsaved team edits");
  });

  it("says how many edits in how many workspaces, in plain words", () => {
    const s = status([local([file("a.md", "changed"), file("b.md", "new")])], 2);
    expect(counterSentence(s)).toBe(
      "2 unsaved team edits in 1 workspace: team files changed in a workspace and not in the team yet",
    );
  });

  it("marks conflicts and never shows an engine reason with hashes", () => {
    const c = file("CLAUDE.md", "conflict", {
      reason:
        "the team file changed since it was copied here (base 0123456789ab, team ba9876543210)",
    });
    expect(fileNote(c)).toBe("Changed here and in the team since it was copied");
    expect(fileActions(c)).toEqual({ view: true, publish: false, discard: true });
    expect(fileNote(file("big.bin", "cannotPublish", { discardable: true }))).toBe(
      "Too large to publish. Copy it into the team by hand, or discard it.",
    );
    expect(fileActions(file("big.bin", "cannotPublish"))).toEqual({
      view: false,
      publish: false,
      discard: false,
    });
    expect(fileActions(file("big.bin", "cannotPublish", { discardable: true })).discard).toBe(true);
  });

  it("labels where a file goes when it is not this team", () => {
    expect(fileNote(file("a.md", "changed"))).toBeNull();
    expect(fileNote(file("a.md", "changed", { destination: { kind: "private" } }))).toBe(
      "goes to your private files",
    );
    expect(
      fileNote(file("a.md", "new", { destination: { kind: "parentTeam", name: "platform" } })),
    ).toBe("goes to platform");
  });

  it("says what Discard does before it does it", () => {
    expect(discardSentence(file("notes/x.md", "new"))).toBe("Delete x.md from this workspace?");
    expect(discardSentence(file("a.md", "removed"))).toBe(
      "Put the team's a.md back in this workspace?",
    );
    expect(discardSentence(file("a.md", "changed"))).toBe(
      "Throw away the changes to a.md here and use the team's version?",
    );
  });

  it("publishes all only changed and new files", () => {
    const ws = local([
      file("a.md", "changed"),
      file("b.md", "new"),
      file("c.md", "removed"),
      file("d.md", "conflict"),
    ]);
    expect(publishAllPaths(ws)).toEqual(["a.md", "b.md"]);
  });

  it("tells a machine workspace's state with its age", () => {
    const now = 10 * 60_000;
    const remote = (extra: Partial<TeamFileEditsWorkspace>): TeamFileEditsWorkspace =>
      ({
        target: { kind: "remote", machineId: "pi", workspaceHandle: "remote-workspace-x" },
        name: "ws",
        files: [file("a.md", "changed")],
        ...extra,
      }) as TeamFileEditsWorkspace;
    expect(workspaceLine(local([]), now)).toBeNull();
    expect(workspaceLine(remote({ state: "checked", checkedAtMs: now - 3 * 60_000 }), now)).toBe(
      "on a machine · checked 3m ago",
    );
    expect(workspaceLine(remote({ state: "offline", checkedAtMs: now - 3 * 60_000 }), now)).toBe(
      "machine offline · last count 3m ago",
    );
    expect(workspaceLine(remote({ state: "notChecked", files: [] }), now)).toBe(
      "on a machine · not checked yet",
    );
  });

  it("never shows engine text in an error", () => {
    expect(plainError("the workspace file changed since the preview")).toBe(
      "it changed since you looked",
    );
    expect(plainError("reverse_sync_worktree failed at 0123456789abcdef /Users/x")).toBe(
      "it didn't go through (details are in the log)",
    );
  });
});

it("reports missing folders without counting their lost edits", () => {
  const missing = local([], {
    target: { kind: "remote", machineId: "devbox", workspaceHandle: "gone" },
    state: "missing",
    counts: { changed: 3, new: 0, removed: 0, conflicts: 0, cannotPublish: 0 },
  });
  expect(workspaceLine(missing, 0)).toBe(
    "folder no longer exists on devbox · its 3 unsaved team edits went with the folder",
  );
  expect(counterVisible(status([missing], 0))).toBe(true);
  expect(counterSentence(status([missing], 0))).toBe("1 workspace folder gone");
  expect(counterSentence(status([missing, local([file("a.md", "changed")])], 1))).toContain(
    "1 workspace folder gone",
  );
});

it("says why unsafe and occupied paths could not be checked", () => {
  expect(
    workspaceLine(
      local([], {
        state: "unknown",
        reason: "the workspace path on the machine isn't a normal folder",
      }),
      0,
    ),
  ).toBe("couldn't check: the workspace path on the machine isn't a normal folder");
  expect(
    workspaceLine(
      local([], { state: "unknown", reason: "something else is at the workspace path" }),
      0,
    ),
  ).toBe("couldn't check: something else is at the workspace path");
});

it("does not count conflicts lost with a missing folder", () => {
  const missing = local([], {
    state: "missing",
    counts: { changed: 0, new: 0, removed: 0, conflicts: 2, cannotPublish: 0 },
  });
  expect(counterSentence(status([missing, local([file("a.md", "changed")])], 1))).not.toContain(
    "changed here and in the team too",
  );
});
