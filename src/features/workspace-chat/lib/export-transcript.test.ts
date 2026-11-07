// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";

import type { WorkspaceChatMessage } from "@/tauri/commands";

import { exportTranscript, renderTranscript, transcriptFilename } from "./export-transcript";

const writeContentFile =
  vi.fn<(path: string, content: string, expectedSha256?: string) => Promise<unknown>>();
vi.mock("@/tauri/commands", async () => {
  const actual = await vi.importActual<typeof import("@/tauri/commands")>("@/tauri/commands");
  return {
    ...actual,
    writeContentFile: (...a: unknown[]) =>
      writeContentFile(...(a as Parameters<typeof writeContentFile>)),
  };
});

function msg(id: number, overrides: Partial<WorkspaceChatMessage> = {}): WorkspaceChatMessage {
  return {
    messageId: id,
    workspaceId: "story:ws-1",
    from: `agent-${id}`,
    fromName: `agent ${id}`,
    createdAtMs: 1_700_000_000_000 + id * 1000,
    text: `message ${id}`,
    kind: "chat",
    ...overrides,
  };
}

describe("SW-WC-3 · renderTranscript", () => {
  it("stamps a header + a row per message, oldest-first", () => {
    const out = renderTranscript({
      workspaceId: "story:ws-1",
      displayName: "The thing",
      teamContentPath: "/ctx/content",
      messages: [
        msg(1, {
          kind: "system",
          from: "system",
          fromName: "System",
          text: "Chat started.",
        }),
        msg(2, { from: "human", fromName: "sam", text: "nice **work**" }),
        msg(3, { text: "ack" }),
      ],
    });
    expect(out).toMatch(/^# Workspace chat — The thing/);
    expect(out).toMatch(/Workspace id: `story:ws-1`/);
    expect(out).toMatch(/Messages: 3/);
    expect(out).toContain("[system]");
    expect(out).toContain("[you]");
    expect(out).toContain("@agent 3");
    // Markdown bodies preserved verbatim.
    expect(out).toContain("nice **work**");
  });

  it("notes an attachment when primaryFile is set", () => {
    const out = renderTranscript({
      workspaceId: "story:ws-1",
      teamContentPath: "/ctx",
      messages: [msg(1, { primaryFile: "/ctx/content/notes.md" })],
    });
    expect(out).toContain("`/ctx/content/notes.md`");
  });
});

describe("SW-WC-3 · transcriptFilename", () => {
  it("sanitizes id characters and appends an ISO-ish timestamp", () => {
    const name = transcriptFilename("story:my/weird id", new Date("2026-10-08T12:30:45.123Z"));
    expect(name).toBe("workspace-chat-story-my-weird-id-20261008123045.md");
  });
});

describe("SW-WC-3 · exportTranscript", () => {
  it("writes under {teamContentPath}/transcripts/", async () => {
    writeContentFile.mockReset().mockResolvedValue(undefined);
    const path = await exportTranscript({
      workspaceId: "story:ws-1",
      teamContentPath: "/content/root",
      messages: [msg(1)],
    });
    expect(path).toMatch(/^\/content\/root\/transcripts\/workspace-chat-story-ws-1-\d{14}\.md$/);
    expect(writeContentFile).toHaveBeenCalledTimes(1);
    const [filePath, body] = writeContentFile.mock.calls[0];
    expect(filePath).toBe(path);
    expect(body).toMatch(/Messages: 1/);
  });
});
