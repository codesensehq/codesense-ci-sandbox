// SPDX-License-Identifier: MIT
import { describe, expect, it, vi, beforeEach } from "vitest";

const h = vi.hoisted(() => ({
  resolveAgentCli: vi.fn<(basename: string) => Promise<string | null>>(),
}));

vi.mock("@/tauri/commands", () => ({ resolveAgentCli: h.resolveAgentCli }));

const { resolveAgentCliPath, DEFAULT_AGENT_CLI } = await import("./default-agent");

beforeEach(() => h.resolveAgentCli.mockReset());

describe("resolveAgentCliPath", () => {
  it("asks the backend for the built-in Claude CLI by default", async () => {
    h.resolveAgentCli.mockResolvedValue("/tmp/a/claude");
    expect(DEFAULT_AGENT_CLI).toBe("claude");
    expect(await resolveAgentCliPath()).toBe("/tmp/a/claude");
    expect(h.resolveAgentCli).toHaveBeenCalledWith("claude");
  });

  it("resolves a named CLI by its own basename, never another one", async () => {
    h.resolveAgentCli.mockImplementation(async (b) => (b === "codex" ? "/tmp/b/codex" : null));
    expect(await resolveAgentCliPath("codex")).toBe("/tmp/b/codex");
    expect(await resolveAgentCliPath("claude")).toBeNull();
  });

  it("is null when the CLI isn't installed", async () => {
    h.resolveAgentCli.mockResolvedValue(null);
    expect(await resolveAgentCliPath()).toBeNull();
  });

  it("asks again on every call (no cross-call cache)", async () => {
    h.resolveAgentCli.mockResolvedValueOnce("/tmp/a/claude").mockResolvedValueOnce("/tmp/b/claude");
    expect(await resolveAgentCliPath()).toBe("/tmp/a/claude");
    expect(await resolveAgentCliPath()).toBe("/tmp/b/claude");
    expect(h.resolveAgentCli).toHaveBeenCalledTimes(2);
  });
});
