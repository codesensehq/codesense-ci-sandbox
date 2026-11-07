// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { HTML_BRIDGE_ALLOWLIST } from "./html-bridge";
import { dismissAgentMark } from "@/tauri/commands";
import { frameFixture } from "../../../../tests/support/fixtures/frame";
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  dismissAgentMark: vi.fn(),
}));
beforeEach(() => vi.clearAllMocks());
describe("dismiss_agent_mark page binding", () => {
  it("uses the native/MCP write with the immutable frame identity", async () => {
    const frame = frameFixture({
      documentPath: "/ctx/board.html",
      contentRoot: "/ctx",
      slot: "right-pane",
    });
    vi.mocked(dismissAgentMark).mockResolvedValue({
      sessionId: "s",
      crashId: "exit",
      dismissed: true,
      requestId: "request",
    });
    await expect(
      HTML_BRIDGE_ALLOWLIST.dismiss_agent_mark(
        { sessionId: "s", crashId: "exit", requestId: "request" },
        { frame },
      ),
    ).resolves.toEqual({ sessionId: "s", crashId: "exit", dismissed: true, requestId: "request" });
    expect(dismissAgentMark).toHaveBeenCalledWith("s", "exit", "request", {
      origin: frame.origin,
      token: frame.token,
    });
  });
});
