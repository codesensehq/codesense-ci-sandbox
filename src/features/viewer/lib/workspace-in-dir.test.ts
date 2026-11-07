// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  layoutWorkspaceDir,
  tabDirWorkspace,
  tabLayoutDir,
  workspaceInDir,
  type WorkspaceSlices,
} from "./workspace-in-dir";
import type { TerminalLayoutProjection } from "@/shared/lib/terminal-layout-projection";
import { workspaceKey } from "@/shared/lib/workspace-key";
import type { Workspace } from "@/shared/stores/viewer-store";

const TEAM = "acme/widgets:core";

const ws = (id: string, cwd: string, extra: Partial<Workspace> = {}): Workspace =>
  ({ id, kind: "generic", cwd, status: "ready", ...extra }) as Workspace;

function slices(open: Workspace[], minimized: Workspace[] = []): WorkspaceSlices {
  return {
    workspaces: { [TEAM]: open },
    softClosedWorkspaces: {
      [TEAM]: Object.fromEntries(minimized.map((w) => [w.id, { workspace: w }])),
    },
  };
}

describe("workspaceInDir", () => {
  it("matches a directory with or without its trailing slash", () => {
    const s = slices([ws("w1", "/repo/a/")]);
    expect(workspaceInDir(s, TEAM, "/repo/a")?.id).toBe("w1");
    expect(workspaceInDir(slices([ws("w1", "/repo/a")]), TEAM, "/repo/a//")?.id).toBe("w1");
  });

  it("keeps the root directory as itself", () => {
    expect(workspaceInDir(slices([ws("root", "/")]), TEAM, "//")?.id).toBe("root");
    expect(workspaceInDir(slices([ws("w1", "/repo")]), TEAM, "/")).toBeUndefined();
  });

  it("finds an open workspace before one minimized into the tray", () => {
    const s = slices([ws("open", "/repo")], [ws("tray", "/repo")]);
    expect(workspaceInDir(s, TEAM, "/repo")?.id).toBe("open");
    expect(workspaceInDir(slices([], [ws("tray", "/repo")]), TEAM, "/repo")?.id).toBe("tray");
  });

  it("picks the first of two in the same directory: the team's order, else the tray record's", () => {
    const open = slices([ws("a", "/repo"), ws("b", "/repo/")], [ws("tray", "/repo")]);
    expect(workspaceInDir(open, TEAM, "/repo")?.id).toBe("a");
    const minimized = slices([], [ws("t1", "/repo/"), ws("t2", "/repo")]);
    expect(workspaceInDir(minimized, TEAM, "/repo")?.id).toBe("t1");
  });

  it("never matches a workspace on a remote machine, whatever its cwd says", () => {
    const remote = ws("r1", "/home/codesense/w", {
      location: { kind: "machine", machineId: "box", requestId: "req-1" },
    });
    expect(workspaceInDir(slices([remote], [remote]), TEAM, "/home/codesense/w")).toBeUndefined();
  });
});

describe("layoutWorkspaceDir", () => {
  const layout = (key: string, worktreePath: string) =>
    ({ [key]: { descriptor: { worktreePath } } }) as unknown as TerminalLayoutProjection["byKey"];

  it("reads a record-less story workspace by the story id the tab index names it by", () => {
    const key = workspaceKey(TEAM, { id: "SW-9-x", kind: "story", storyId: "SW-9" });
    expect(layoutWorkspaceDir(layout(key, "/wt/SW-9"), TEAM, "SW-9")).toBe("/wt/SW-9");
  });

  it("reads a generic directory workspace by its form", () => {
    const key = workspaceKey(TEAM, { id: "generic:/w2" });
    expect(layoutWorkspaceDir(layout(key, "/w2"), TEAM, "generic:/w2")).toBe("/w2");
    expect(layoutWorkspaceDir(layout(key, "/w2"), TEAM, "generic:/w3")).toBeNull();
  });
});

describe("tabDirWorkspace", () => {
  const key = workspaceKey(TEAM, { id: "generic:/w2" });
  const byKey = {
    [key]: { descriptor: { worktreePath: "/w2/" } },
  } as unknown as TerminalLayoutProjection["byKey"];
  const loc = (origin?: { machineId: string }) => ({
    teamKey: TEAM,
    workspaceId: "generic:/w2",
    tab: origin ? { origin } : {},
  });

  it("places a tab of a host workspace no record keys in the team's workspace in its directory", () => {
    const s = slices([ws("w2", "/w2")]);
    expect(tabLayoutDir(byKey, loc())).toBe("/w2/");
    expect(tabDirWorkspace(s, byKey, loc())?.id).toBe("w2");
    expect(tabDirWorkspace(slices([], [ws("tray", "/w2")]), byKey, loc())?.id).toBe("tray");
  });

  it("never places a remote tab by a directory, which is on its machine; This Mac's id is local", () => {
    const s = slices([ws("w2", "/w2")]);
    expect(tabLayoutDir(byKey, loc({ machineId: "box" }))).toBeNull();
    expect(tabDirWorkspace(s, byKey, loc({ machineId: "box" }))).toBeUndefined();
    expect(tabDirWorkspace(s, byKey, loc({ machineId: "local" }))?.id).toBe("w2");
  });
});
