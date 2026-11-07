// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { teamSessionLaunchRequest } from "./reopen-team-launch";
import type { AgentSessionRow } from "@/tauri/commands";
import type { Workspace } from "@/shared/stores/viewer-store";

const ws = {
  id: "cw_one",
  kind: "context",
  contextWorktreeId: "cw_one",
  cwd: "/w/default/content",
  status: "ready",
} as Workspace;
const row = {
  handle: "acme/default/reviewer",
  displayName: "Review pass",
  agentRevision: "acme/default/reviewer@7",
  role: "investigator",
  queueRole: "qa",
  launchedBy: "lead",
  startedWith: "/resume",
  avatar: "fun:robot",
} as AgentSessionRow;
describe("team relaunch", () => {
  it("opens the same worktree with the saved agent's name, role, manager, face and prompt", () => {
    expect(teamSessionLaunchRequest("acme:default", ws, row, "fallback")).toEqual({
      teamKey: "acme:default",
      where: "team-workspace",
      contextWorktreeId: "cw_one",
      agentKey: "acme/default/reviewer",
      name: "Review pass",
      role: "investigator",
      queueRole: "qa",
      managerHandle: "lead",
      task: "/resume",
      face: "fun:robot",
    });
  });
  it.each([null, "qa"])(
    "retains the effective queue choice %j separately from hierarchy",
    (queueRole) => {
      expect(
        teamSessionLaunchRequest("acme:default", ws, { ...row, queueRole }, "fallback"),
      ).toMatchObject({ role: "investigator", queueRole });
    },
  );
  it("leaves story launches and arbitrary terminals with their existing relaunch", () => {
    expect(teamSessionLaunchRequest("acme:default", ws, undefined, "Terminal")).toBeNull();
    expect(
      teamSessionLaunchRequest("acme:default", { ...ws, kind: "story" }, row, "fallback"),
    ).toBeNull();
    expect(
      teamSessionLaunchRequest(
        "acme:default",
        { ...ws, kind: "generic", cwd: "/arbitrary" },
        row,
        "fallback",
      ),
    ).toBeNull();
  });
  it("moves a retained legacy worker into the sheet without changing its folder", () => {
    const request = teamSessionLaunchRequest(
      "acme:default",
      {
        ...ws,
        kind: "generic",
        contextWorktreeId: undefined,
        cwd: "/home/users/42/_worker/acme/default/reviewer",
      },
      row,
      "fallback",
    );
    expect(request?.contextWorktreeId).toBeUndefined();
    expect(request?.name).toBe("Review pass");
  });
});
