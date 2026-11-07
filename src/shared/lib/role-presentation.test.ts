// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { defaultRoleIcon, rolePresentation } from "./role-presentation";

describe("queue role identity", () => {
  it("uses a stable chess default distinct from hierarchy, with all six available for explicit picks", () => {
    for (const id of ["reviewer", "qa", "builder", "ops", "planner"]) {
      expect(defaultRoleIcon(id)).toMatch(/^builtin-chess:(king|rook|bishop|knight)$/);
      expect(defaultRoleIcon(id)).toBe(defaultRoleIcon(id));
    }
  });
  it("keeps queue identity and ignores missing images without substituting another handler", () => {
    const config = {
      id: "reviewer",
      name: "Review",
      icon: "missing:face",
      defaultHandler: "deleted-agent",
    };
    expect(rolePresentation("reviewer", [config])).toMatchObject({
      id: "reviewer",
      name: "Review",
      ref: defaultRoleIcon("reviewer"),
      title: "Queue role: Review (role:reviewer)",
    });
    expect(config.defaultHandler).toBe("deleted-agent");
    expect(rolePresentation("raw-queue", [])).toMatchObject({
      name: "raw-queue",
      configured: false,
    });
  });
});
