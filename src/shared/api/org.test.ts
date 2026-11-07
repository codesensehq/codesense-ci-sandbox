// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

const mockCreateWorkspaceCmd = vi.fn();
const mockConnectToOrgCmd = vi.fn();
vi.mock("@/tauri/commands", () => ({
  createWorkspaceCmd: (...a: unknown[]) => mockCreateWorkspaceCmd(...a),
  connectToOrgCmd: (...a: unknown[]) => mockConnectToOrgCmd(...a),
  discoverLocalOrgsCmd: vi.fn(),
}));

import { createWorkspace, connectToOrg } from "./org";

beforeEach(() => {
  vi.clearAllMocks();
});

describe("createWorkspace", () => {
  it("returns the created org", async () => {
    mockCreateWorkspaceCmd.mockResolvedValue({
      org: { slug: "acme", root: "/x", org: null, partial: {}, file_errors: [] },
      remote_warning: null,
    });

    const result = await createWorkspace(1, "Acme", "octocat", "Octo Cat", null);

    expect(result.org.slug).toBe("acme");
  });
});

describe("connectToOrg", () => {
  it("forwards the exact workspace binding to the Tauri command", async () => {
    mockConnectToOrgCmd.mockResolvedValue({
      org_slug: "foreign",
      org_root: "/y",
      clone_outcome: "cloned",
      loaded_org: { slug: "foreign", root: "/y", org: null, partial: {}, file_errors: [] },
    });
    const binding = { provider: "github", account: "work" };

    await connectToOrg("https://github.com/foreign/ctx", 1, binding);

    expect(mockConnectToOrgCmd).toHaveBeenCalledWith("https://github.com/foreign/ctx", 1, binding);
  });
});
