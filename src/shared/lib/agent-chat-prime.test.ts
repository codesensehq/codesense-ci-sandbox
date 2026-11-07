// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { buildPrimeLine, newHandshakeToken, primeAgentChat } from "@/shared/lib/agent-chat-prime";

describe("newHandshakeToken (AC-3b)", () => {
  it("returns a non-empty, collision-resistant token each call", () => {
    const a = newHandshakeToken();
    const b = newHandshakeToken();
    expect(a).not.toBe("");
    expect(a).not.toEqual(b);
  });
});

describe("buildPrimeLine (AC-0/AC-3b)", () => {
  it("carries the session id, the post_agent_message instruction, and the token verbatim", () => {
    const line = buildPrimeLine("shell-123", "cs-ack-abc");
    expect(line).toContain("shell-123");
    expect(line).toContain("post_agent_message");
    // The token must be present so the agent echoes it back as the pong.
    expect(line).toContain("cs-ack-abc");
  });

  it("is agent-agnostic — no vendor name (claude) in the priming text", () => {
    const line = buildPrimeLine("shell-123", "cs-ack-abc").toLowerCase();
    expect(line).not.toContain("claude");
  });
});

describe("primeAgentChat (AC-0)", () => {
  it("injects the prime line into the bound session (gate none)", async () => {
    const inject = vi.fn(
      async (
        _sessionId: string,
        _line: string,
        _deps?: undefined,
        _opts?: { gate?: "shell" | "none" | "auto" },
      ) => ({ ok: true }) as const,
    );
    await primeAgentChat("shell-123", "cs-ack-abc", { inject });
    expect(inject).toHaveBeenCalledTimes(1);
    const [sid, line, , opts] = inject.mock.calls[0];
    expect(sid).toBe("shell-123");
    expect(line).toContain("cs-ack-abc");
    expect(opts).toEqual({ gate: "none" });
  });

  it("never throws — a priming failure is logged, not fatal (the session is still usable)", async () => {
    const inject = vi.fn(async () => {
      throw new Error("session gone");
    });
    await expect(primeAgentChat("shell-123", "cs-ack-abc", { inject })).resolves.toBeUndefined();
  });
});
