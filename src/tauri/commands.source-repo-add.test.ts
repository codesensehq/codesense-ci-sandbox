// SPDX-License-Identifier: MIT
/**
 * The add-repo wrappers turn a Rust refusal (a plain `{ kind, message, … }`
 * object) into an `Error` carrying the typed detail, and leave anything else
 * as it was thrown.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const { mockInvoke } = vi.hoisted(() => ({ mockInvoke: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

import { addSourceRepoToTeam, attachSourceRepoClone, isAddRepoError } from "./commands";

const ADD = {
  githubUserId: 1,
  orgSlug: "acme/ctx",
  teamName: "default",
  input: { kind: "url", url: "acme/api" } as const,
};

beforeEach(() => mockInvoke.mockReset());

describe("add-repo refusals", () => {
  it("a typed refusal becomes an Error with its kind, fields and message", async () => {
    mockInvoke.mockRejectedValueOnce({
      kind: "alreadyTracked",
      owner: "acme",
      repo: "api",
      team: "default",
      message: "acme/api is already tracked on team default",
    });

    const caught = await addSourceRepoToTeam(ADD).catch((e: unknown) => e);

    expect(caught).toBeInstanceOf(Error);
    expect(isAddRepoError(caught)).toBe(true);
    if (!isAddRepoError(caught)) return;
    expect(caught.kind).toBe("alreadyTracked");
    expect(caught.message).toBe("acme/api is already tracked on team default");
    expect(mockInvoke).toHaveBeenCalledWith("add_source_repo_to_team", {
      githubUserId: 1,
      orgSlug: "acme/ctx",
      teamName: "default",
      input: { kind: "url", url: "acme/api" },
    });
  });

  it("the attach wrapper converts the same way", async () => {
    mockInvoke.mockRejectedValueOnce({
      kind: "notRepoRoot",
      path: "/c/api/src",
      root: "/c/api",
      message: "inside",
    });
    const caught = await attachSourceRepoClone({
      githubUserId: 1,
      repo: { owner: "acme", repo: "api", provider: "github", host: null },
      path: "/c/api/src",
    }).catch((e: unknown) => e);
    expect(isAddRepoError(caught) && caught.kind === "notRepoRoot" && caught.root).toBe("/c/api");
  });

  it("an unknown kind, a plain Error and a string are not add-repo refusals", async () => {
    for (const thrown of [{ kind: "authFailed", message: "x" }, new Error("boom"), "boom"]) {
      mockInvoke.mockRejectedValueOnce(thrown);
      const caught = await addSourceRepoToTeam(ADD).catch((e: unknown) => e);
      expect(caught).toBe(thrown);
      expect(isAddRepoError(caught)).toBe(false);
    }
    expect(isAddRepoError(Object.assign(new Error("x"), { kind: "authFailed" }))).toBe(false);
  });
});
