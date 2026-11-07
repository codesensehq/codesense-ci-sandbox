// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { renderHook } from "@testing-library/react";

let capturedHandler: ((event: { payload: unknown }) => void) | null = null;
vi.mock("@tauri-apps/api/event", () => ({
  listen: (_event: string, handler: (e: { payload: unknown }) => void) => {
    capturedHandler = handler;
    return Promise.resolve(() => {});
  },
}));

import { useAgentMessageListener } from "./use-agent-message-listener";
import {
  useAgentChatStore,
  resetAgentChatStoreForTests,
} from "@/shared/stores/use-agent-chat-store";

describe("useAgentMessageListener (AC-4) @P1", () => {
  beforeEach(() => {
    resetAgentChatStoreForTests();
    capturedHandler = null;
  });

  it("appends an inbound agent message to the session transcript", () => {
    renderHook(() => useAgentMessageListener());
    expect(capturedHandler).toBeTypeOf("function");
    capturedHandler?.({ payload: { sessionId: "s1", role: "agent", text: "hi from agent" } });
    expect(useAgentChatStore.getState().transcripts.s1?.[0]).toMatchObject({
      role: "agent",
      text: "hi from agent",
    });
  });

  it("ignores payloads missing sessionId or text", () => {
    renderHook(() => useAgentMessageListener());
    capturedHandler?.({ payload: { sessionId: "", text: "x" } });
    capturedHandler?.({ payload: { sessionId: "s1", text: "" } });
    expect(useAgentChatStore.getState().transcripts.s1).toBeUndefined();
  });

  // SW-WV-F6 — carry a valid renderTarget through to the store; reject bad values.
  it("passes a valid renderTarget through to the transcript", () => {
    renderHook(() => useAgentMessageListener());
    capturedHandler?.({
      payload: { sessionId: "s1", text: "companion", renderTarget: "drill_doc" },
    });
    expect(useAgentChatStore.getState().transcripts.s1?.[0]?.renderTarget).toBe("drill_doc");
  });

  it("drops an unknown renderTarget value (stores undefined)", () => {
    renderHook(() => useAgentMessageListener());
    capturedHandler?.({
      payload: { sessionId: "s1", text: "hi", renderTarget: "carrier-pigeon" },
    });
    expect(useAgentChatStore.getState().transcripts.s1?.[0]?.renderTarget).toBeUndefined();
  });
});
