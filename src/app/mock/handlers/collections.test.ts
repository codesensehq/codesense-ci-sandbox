// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { teamHandlers } from "./collections";

describe("teamHandlers", () => {
  it("resolves a local registry edit (create/rename/archive/delete all route here) so the optimistic UI commits", () => {
    const h = teamHandlers();
    const outcome = h.commit_registry_edit_local({
      githubUserId: 1,
      orgSlug: "acme/widgets",
      edit: { type: "CreateTeam", team_name: "Platform" },
    }) as { commit_message: string };
    expect(typeof outcome.commit_message).toBe("string");
  });

  it("resolves team activation without error (the side-effect is inert)", () => {
    const h = teamHandlers();
    expect(h.switch_team_install).toBeUndefined();
    expect(h.set_active_team_for_wave({})).toBeUndefined();
  });
});
