// SPDX-License-Identifier: MIT
// @vitest-environment jsdom
import { beforeEach, expect, it, vi } from "vitest";
import { frameFixture } from "../../../../tests/support/fixtures/frame";
import { runBridgeCommand } from "./html-bridge";
import type { DocTheme } from "./doc-theme";
import { agentReadInbox, mintFrame, releaseFrame } from "@/tauri/commands";
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  agentReadInbox: vi.fn(),
  mintFrame: vi.fn(),
  releaseFrame: vi.fn(async () => {}),
}));
const team = { org: "acme", team: "mail" };
const pane = frameFixture({
  documentPath: "/ctx/panes/inbox.html",
  contentRoot: "/ctx",
  slot: "right-pane",
});
const theme: DocTheme = {
  scheme: "dark",
  tokens: { "--cs-bg": "#0d1117", "--cs-fg": "#f0f6fc" },
};
const embedMessage = vi.fn();
const ctx = { frame: pane, embedMessage, theme };
beforeEach(() => vi.clearAllMocks());
it("mints a child with the stored sender and team and returns only a URL", async () => {
  vi.mocked(agentReadInbox).mockResolvedValue({
    messages: [
      {
        messageId: 7,
        from: "Mia",
        to: "human",
        toHuman: true,
        contentType: "html",
        text: "<button>Go</button>",
      },
    ],
  } as never);
  const frame = frameFixture({
    documentPath: "inbox-message://acme/mail/7",
    contentRoot: "/mail",
    slot: "message",
    message: { ...team, messageId: 7 },
    parentFrame: pane.frameId,
  });
  vi.mocked(mintFrame).mockResolvedValue(frame);
  const result = await runBridgeCommand("embedMessage", { ...team, messageId: 7 }, ctx);
  expect(mintFrame).toHaveBeenCalledWith(
    expect.objectContaining({
      message: { ...team, messageId: 7 },
      parentFrame: pane.frameId,
      slot: "message",
    }),
  );
  expect(embedMessage).toHaveBeenCalledWith(frame);
  expect(result).toEqual({
    url: expect.stringContaining("/message/"),
    height: 160,
    allow: "clipboard-write",
    frameId: frame.frameId,
  });
  const childUrl = new URL((result as { url: string }).url);
  expect(JSON.parse(childUrl.searchParams.get("theme")!)).toEqual(theme);
  expect(JSON.stringify(result)).not.toContain(frame.token);
  vi.mocked(mintFrame).mockResolvedValue({ ...frame, parentFrame: "another-pane" });
  await expect(runBridgeCommand("embedMessage", { ...team, messageId: 7 }, ctx)).rejects.toThrow(
    "denied: parent frame",
  );
  expect(releaseFrame).toHaveBeenCalledWith(frame.frameId);
});
it("sanitizes human HTML using the existing host sanitizer and never mints agent mail", async () => {
  vi.mocked(agentReadInbox).mockResolvedValue({
    messages: [
      {
        messageId: 7,
        from: "human",
        to: "Mia",
        contentType: "html",
        text: '<b>Sent</b><script>bad()</script><img src="x" onerror="bad()">',
      },
    ],
  } as never);
  const result = (await runBridgeCommand("embedMessage", { ...team, messageId: 7 }, ctx)) as {
    html: string;
  };
  expect(result.html).toContain("<b>Sent</b>");
  expect(result.html).not.toMatch(/script|onerror|bad\(/);
  expect(mintFrame).not.toHaveBeenCalled();
  vi.mocked(agentReadInbox).mockResolvedValue({
    messages: [
      {
        messageId: 7,
        from: "Mia",
        to: "Ori",
        toHuman: false,
        contentType: "html",
        text: "private",
      },
    ],
  } as never);
  await expect(runBridgeCommand("embedMessage", { ...team, messageId: 7 }, ctx)).rejects.toThrow(
    "message unavailable",
  );
  expect(mintFrame).not.toHaveBeenCalled();
});

it("marks an owned message boot as a candidate while its parent is not presented", async () => {
  vi.mocked(agentReadInbox).mockResolvedValue({
    messages: [
      {
        messageId: 7,
        from: "Mia",
        to: "human",
        toHuman: true,
        contentType: "html",
        text: "<input>",
      },
    ],
  } as never);
  vi.mocked(mintFrame).mockResolvedValue(
    frameFixture({
      documentPath: "inbox-message://acme/mail/7",
      contentRoot: "/mail",
      slot: "message",
      message: { ...team, messageId: 7 },
      parentFrame: pane.frameId,
    }),
  );
  const hidden = { ...ctx, presented: false };
  const result = (await runBridgeCommand("embedMessage", { ...team, messageId: 7 }, hidden)) as {
    url: string;
  };
  expect(new URL(result.url).searchParams.get("codesenseCandidate")).toBe("1");
});
