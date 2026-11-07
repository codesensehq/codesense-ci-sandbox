// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

const setActiveTeamForWave = vi.fn();
vi.mock("@/tauri/commands", () => ({
  setActiveTeamForWave: (...args: unknown[]) => setActiveTeamForWave(...args),
}));

import { commitActiveTeamForWave, useBackendActiveTeamStore } from "./backend-active-team";

const params = {
  codesenseHome: "/h",
  githubUserId: 1,
  orgSlug: "local",
  team: "e2e",
  sourceRepos: [],
};

describe("SW-LN-B57 · the backend's active team", () => {
  beforeEach(() => {
    setActiveTeamForWave.mockReset();
    useBackendActiveTeamStore.setState({ teamKey: null, epoch: 0 });
  });

  it("announces the team once the backend has committed it", async () => {
    setActiveTeamForWave.mockResolvedValue(undefined);
    await commitActiveTeamForWave(params);
    expect(useBackendActiveTeamStore.getState()).toEqual({ teamKey: "local:e2e", epoch: 1 });
  });

  it("sends commits to the backend one at a time, in call order", async () => {
    const sent: string[] = [];
    let releaseFirst!: () => void;
    setActiveTeamForWave.mockImplementationOnce((p: { team: string }) => {
      sent.push(p.team);
      return new Promise<void>((r) => (releaseFirst = r));
    });
    setActiveTeamForWave.mockImplementation((p: { team: string }) => {
      sent.push(p.team);
      return Promise.resolve();
    });
    const first = commitActiveTeamForWave({ ...params, team: "a" });
    const second = commitActiveTeamForWave({ ...params, team: "b" });
    await new Promise((r) => setTimeout(r, 0));
    // "b" waits for "a" to settle, so it can never land first and be overridden.
    expect(sent).toEqual(["a"]);
    releaseFirst();
    await Promise.all([first, second]);
    expect(sent).toEqual(["a", "b"]);
    expect(useBackendActiveTeamStore.getState().teamKey).toBe("local:b");
  });

  it("a failed commit doesn't block the next one", async () => {
    setActiveTeamForWave.mockRejectedValueOnce(new Error("boom"));
    setActiveTeamForWave.mockResolvedValue(undefined);
    await expect(commitActiveTeamForWave({ ...params, team: "a" })).rejects.toThrow("boom");
    await commitActiveTeamForWave({ ...params, team: "b" });
    expect(useBackendActiveTeamStore.getState().teamKey).toBe("local:b");
  });

  it("announces nothing when the commit fails", async () => {
    setActiveTeamForWave.mockRejectedValue(new Error("boom"));
    await expect(commitActiveTeamForWave(params)).rejects.toThrow("boom");
    expect(useBackendActiveTeamStore.getState().epoch).toBe(0);
  });
});
