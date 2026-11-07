// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  sanitizeWorkspaceGroups,
  validateGroupName,
  customRailKey,
  validateGroupEdit,
} from "./workspace-groups";
import { trayWorkspaceOrder, railKeyOf, epicRailEntries } from "./group-workspaces-by-epic";
import type { Workspace } from "@/shared/stores/viewer-store";
const ws = (id: string, kind: Workspace["kind"] = "story"): Workspace => ({
  id,
  kind,
  cwd: "/tmp",
  status: "ready",
});
const groups = [{ id: "one", name: "Release checks", workspaceIds: ["b", "a"] }];
describe("custom workspace groups", () => {
  it("sanitizes without losing stable ids of closed members", () => {
    expect(
      sanitizeWorkspaceGroups({
        t: [
          ...groups,
          { ...groups[0] },
          { id: "x", name: " release CHECKS ", workspaceIds: [] },
          { id: "z", name: "Other", workspaceIds: ["b", "closed", "closed", null] },
        ],
      }),
    ).toEqual({ t: [...groups, { id: "z", name: "Other", workspaceIds: ["closed"] }] });
  });
  it("validates names once, allowing case-only renames", () => {
    expect(validateGroupName(groups, " RELEASE CHECKS ")).toContain("already");
    expect(validateGroupName(groups, " RELEASE CHECKS ", "one")).toBeNull();
    expect(validateGroupName(groups, " ")).toContain("Type a name");
    expect(validateGroupName(groups, "x".repeat(81))).toContain("80");
    expect(
      validateGroupEdit(groups, { action: "create", name: "new", workspaceIds: ["missing"] }, []),
    ).toContain("Unknown workspace");
    expect(
      validateGroupEdit(groups, { action: "create", name: "new", workspaceIds: ["root:r"] }, [
        ws("root:r", "root"),
      ]),
    ).toContain("Repositories");
  });
  it("validates only newly added ids when the group holds a closed member", () => {
    const groups = [{ id: "g", name: "Checks", workspaceIds: ["closed"] }];
    const open = [ws("new")];
    expect(
      validateGroupEdit(
        groups,
        { action: "set_members", groupId: "g", workspaceIds: ["closed", "new"] },
        open,
      ),
    ).toBeNull();
    expect(
      validateGroupEdit(groups, { action: "add", groupId: "g", workspaceId: "new" }, open),
    ).toBeNull();
    expect(
      validateGroupEdit(groups, { action: "remove", groupId: "g", workspaceId: "closed" }, open),
    ).toBeNull();
    expect(
      validateGroupEdit(groups, { action: "add", groupId: "g", workspaceId: "missing" }, open),
    ).toBe("Unknown workspace.");
  });
  it("orders roots, custom groups, and ungrouped; keeps epic color identity separate", () => {
    const entries = trayWorkspaceOrder(
      [ws("c"), ws("a"), ws("root:r", "root"), ws("b")],
      {},
      null,
      groups,
    );
    expect(entries.map((e) => e.workspace.id)).toEqual(["root:r", "a", "b", "c"]);
    expect(entries.map(railKeyOf)).toEqual([
      " roots",
      customRailKey("one"),
      customRailKey("one"),
      "",
    ]);
    expect(entries[1].epicDirName).toBe("");
    expect(epicRailEntries(entries, "t", groups).map((r) => r.title)).toEqual([
      "~",
      "Release checks",
      "Ungrouped",
    ]);
  });
});
