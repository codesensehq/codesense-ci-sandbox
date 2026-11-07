// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import type { RegistryEdit } from "@/tauri/commands";
import { commitRegistryEdit } from "./commit-registry-edit";

vi.mock("@/tauri/commands", () => ({
  commitRegistryEditLocal: vi.fn().mockResolvedValue(undefined),
  // Present so the module resolves, but the seam must NEVER call it.
  commitRegistryEditAsPr: vi.fn(),
}));

import { commitRegistryEditLocal, commitRegistryEditAsPr } from "@/tauri/commands";

describe("commitRegistryEdit (SW-OB-8 local-only seam)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  const edits: RegistryEdit[] = [
    { type: "RenameWorkspace", new_display_name: "New Name" },
    { type: "RemovePersonByUserId", github_user_id: 42 },
    { type: "AddSourceRepoToTeam", team_name: "default", owner: "acme", repo: "api" },
    { type: "RemoveSourceRepoFromTeam", team_name: "default", owner: "acme", repo: "api" },
  ];

  it.each(edits)("routes $type to the local commit, never the PR path", async (edit) => {
    await commitRegistryEdit(7, "acme/ctx", edit);

    expect(commitRegistryEditLocal).toHaveBeenCalledTimes(1);
    expect(commitRegistryEditLocal).toHaveBeenCalledWith(7, "acme/ctx", edit);
    expect(commitRegistryEditAsPr).not.toHaveBeenCalled();
  });

  it("propagates a backend failure to the caller", async () => {
    vi.mocked(commitRegistryEditLocal).mockRejectedValueOnce(new Error("io: disk full"));

    await expect(
      commitRegistryEdit(7, "acme/ctx", { type: "RenameWorkspace", new_display_name: "X" }),
    ).rejects.toThrow("io: disk full");
  });
});
