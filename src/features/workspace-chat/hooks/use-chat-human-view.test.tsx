// SPDX-License-Identifier: MIT
import { afterEach, expect, it, vi } from "vitest";
import { act, render, waitFor } from "@/test-utils";
import { useRef } from "react";
import type { WorkspaceChatHandle } from "./use-workspace-chat";
import { useChatHumanView } from "./use-chat-human-view";

const viewed = vi.hoisted(() => vi.fn().mockResolvedValue(undefined));
vi.mock("@/tauri/commands", () => ({
  workspaceChatMarkViewed: viewed,
  workspaceChatRead: vi.fn().mockResolvedValue({ humanReadId: 0 }),
}));

afterEach(() => vi.restoreAllMocks());

it("keeps hidden and warm-tab posts unseen until the human reveals the room", async () => {
  const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
    width: 400,
    height: 400,
    top: 0,
    bottom: 400,
    left: 0,
    right: 400,
    x: 0,
    y: 0,
    toJSON: () => ({}),
  });
  const chat: WorkspaceChatHandle = {
    messages: [],
    humanReadId: 0,
    headId: 0,
    loading: false,
    error: null,
    archived: false,
    reload: async () => {},
  };
  function Room({ visible }: { visible: boolean }) {
    const root = useRef<HTMLDivElement>(null);
    useChatHumanView("story:SV-X", chat, root, visible);
    return <div ref={root} />;
  }
  const room = render(<Room visible />);
  chat.messages = [
    {
      workspaceId: "story:SV-X",
      messageId: 1,
      from: "agent",
      fromName: "Agent",
      createdAtMs: 1000,
      text: "arrived while hidden",
      kind: "chat",
      contentType: "plain",
    },
  ];
  room.rerender(<Room visible />);
  expect(viewed).not.toHaveBeenCalled();
  room.rerender(<Room visible={false} />);
  visibility.mockReturnValue("visible");
  act(() => document.dispatchEvent(new Event("visibilitychange")));
  expect(viewed).not.toHaveBeenCalled();
  room.rerender(<Room visible />);
  await waitFor(() => expect(viewed).toHaveBeenCalledExactlyOnceWith("story:SV-X", 1));
});
