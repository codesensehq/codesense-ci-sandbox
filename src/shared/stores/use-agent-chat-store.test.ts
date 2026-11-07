// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";

import { useAgentChatStore, resetAgentChatStoreForTests } from "./use-agent-chat-store";

describe("use-agent-chat-store @P1", () => {
  beforeEach(() => resetAgentChatStoreForTests());

  it("appends messages per session, keyed by sessionId", () => {
    const { appendMessage } = useAgentChatStore.getState();
    appendMessage("s1", "agent", "hello");
    appendMessage("s1", "user", "hi back");
    appendMessage("s2", "agent", "other session");

    const { transcripts } = useAgentChatStore.getState();
    expect(transcripts.s1.map((m) => [m.role, m.text])).toEqual([
      ["agent", "hello"],
      ["user", "hi back"],
    ]);
    expect(transcripts.s2.map((m) => m.text)).toEqual(["other session"]);
    // distinct ids for stable React keys
    expect(new Set(transcripts.s1.map((m) => m.id)).size).toBe(2);
  });

  it("caps a session transcript at the most-recent limit", () => {
    const { appendMessage } = useAgentChatStore.getState();
    for (let i = 0; i < 520; i++) appendMessage("s1", "agent", `m${i}`);
    const msgs = useAgentChatStore.getState().transcripts.s1;
    expect(msgs.length).toBe(500);
    // Keeps the tail (most recent), drops the oldest.
    expect(msgs[0].text).toBe("m20");
    expect(msgs.at(-1)?.text).toBe("m519");
  });

  it("clearTranscript drops only the target session", () => {
    const { appendMessage, clearTranscript } = useAgentChatStore.getState();
    appendMessage("s1", "agent", "a");
    appendMessage("s2", "agent", "b");
    clearTranscript("s1");
    const { transcripts } = useAgentChatStore.getState();
    expect(transcripts.s1).toBeUndefined();
    expect(transcripts.s2).toHaveLength(1);
  });

  // SW-WV-F6 — the render tripwire's routing decision rides the append call.
  it("stores an optional renderTarget on the appended message", () => {
    const { appendMessage } = useAgentChatStore.getState();
    appendMessage("s1", "agent", "see the doc", "drill_doc");
    const msg = useAgentChatStore.getState().transcripts.s1[0];
    expect(msg.renderTarget).toBe("drill_doc");
  });

  it("leaves renderTarget undefined when not passed", () => {
    const { appendMessage } = useAgentChatStore.getState();
    appendMessage("s1", "agent", "hi");
    const msg = useAgentChatStore.getState().transcripts.s1[0];
    expect(msg.renderTarget).toBeUndefined();
  });
});
