// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";
import { mcpAppsReply } from "./mcp-apps-binding";
describe("MCP Apps stable binding", () => {
  it("initializes with standard host fields and optional CodeSense extensions", async () => {
    const run = vi.fn();
    const response = await mcpAppsReply({ jsonrpc: "2.0", id: 1, method: "ui/initialize" }, run);
    expect(response).toMatchObject({
      jsonrpc: "2.0",
      id: 1,
      result: {
        protocolVersion: "2026-01-26",
        hostInfo: { name: "CodeSense" },
        hostCapabilities: {
          serverTools: {},
          experimental: {
            "codesense/subscribe": { optional: true },
            "codesense/slotContext": { optional: true },
          },
        },
      },
    });
    expect(run).not.toHaveBeenCalled();
  });
  it("a read tool uses the gate and returns CallToolResult", async () => {
    const run = vi.fn().mockResolvedValue([{ handle: "S-1" }]);
    const response = await mcpAppsReply(
      { jsonrpc: "2.0", id: "read", method: "tools/call", params: { name: "sessions" } },
      run,
    );
    expect(run).toHaveBeenCalledWith("sessions", {});
    expect(response).toMatchObject({
      result: {
        isError: false,
        content: [{ type: "text" }],
        structuredContent: { result: [{ handle: "S-1" }] },
      },
    });
  });
  it("unknown tools are audited by the gate and become isError", async () => {
    const run = vi.fn().mockRejectedValue(new Error("denied: not-allowed"));
    const response = await mcpAppsReply(
      { jsonrpc: "2.0", id: 3, method: "tools/call", params: { name: "unknown" } },
      run,
    );
    expect(run).toHaveBeenCalledWith("unknown", {});
    expect(response).toMatchObject({
      result: { isError: true, content: [{ text: "denied: not-allowed" }] },
    });
  });
});
