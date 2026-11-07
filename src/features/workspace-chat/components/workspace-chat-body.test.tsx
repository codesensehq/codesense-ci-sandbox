// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@/test-utils";
import type { WorkspaceChatMessage } from "@/tauri/commands";
import { WorkspaceChatBody } from "./workspace-chat-body";

vi.mock("@/features/weft/components/inbox-message-content", () => ({
  InboxMessageContent: ({ text, contentType }: { text: string; contentType?: string }) => (
    <span data-testid="inbox-content" data-content-type={contentType ?? "markdown"}>
      {text}
    </span>
  ),
}));

function chatMsg(id: number, overrides: Partial<WorkspaceChatMessage> = {}): WorkspaceChatMessage {
  return {
    messageId: id,
    workspaceId: "story:ws-1",
    from: `agent-${id}`,
    fromName: `agent ${id}`,
    createdAtMs: 1_700_000_000_000 + id * 1000,
    text: `msg ${id}`,
    kind: "chat",
    ...overrides,
  };
}

describe("WorkspaceChatBody", () => {
  it("shows loading feedback until the first chat read completes", () => {
    render(<WorkspaceChatBody messages={null} loading error={null} density="b" />);
    expect(screen.getByRole("status")).toHaveTextContent("Loading chat…");
    expect(screen.queryByText("No messages yet")).toBeNull();
  });

  it.each([
    [
      "workspace chat engine not installed",
      "The workspace chat engine isn't installed in this build.",
    ],
    ["Could not read chat", "Could not read chat"],
  ])("explains an initial read failure: %s", (error, detail) => {
    render(<WorkspaceChatBody messages={null} loading={false} error={error} density="b" />);
    expect(screen.getByRole("status")).toHaveTextContent("Chat isn't available");
    expect(screen.getByRole("status")).toHaveTextContent(detail);
    expect(screen.queryByText("No messages yet")).toBeNull();
  });

  it("auto-scrolls the body to the tail on first paint, so a long log lands on the newest message", () => {
    // Create a long enough history that scrollTop=0 would hide the tail.
    const messages = Array.from({ length: 100 }, (_, i) => chatMsg(i + 1));
    // Spy on `scrollTop` SET on the body element so the test survives in
    // environments where happy-dom doesn't lay out dimensions. We patch the
    // prototype so the body's assignment is captured regardless of order.
    const scrollTopSets: number[] = [];
    const originalDesc = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollTop");
    Object.defineProperty(HTMLElement.prototype, "scrollTop", {
      configurable: true,
      get: originalDesc?.get ?? (() => 0),
      set(value: number) {
        if ((this as HTMLElement).getAttribute?.("data-testid") === "workspace-chat-body") {
          scrollTopSets.push(value);
        }
        originalDesc?.set?.call(this, value);
      },
    });
    // scrollHeight is read-only — define a prototype getter the body picks up.
    const originalScrollHeight = Object.getOwnPropertyDescriptor(
      HTMLElement.prototype,
      "scrollHeight",
    );
    Object.defineProperty(HTMLElement.prototype, "scrollHeight", {
      configurable: true,
      get() {
        if ((this as HTMLElement).getAttribute?.("data-testid") === "workspace-chat-body") {
          return 9999;
        }
        return originalScrollHeight?.get?.call(this) ?? 0;
      },
    });

    try {
      render(<WorkspaceChatBody messages={messages} loading={false} error={null} density="b" />);
      // On first paint with messages present, the body should get scrollTop
      // assigned to scrollHeight (unconditional first-paint tail anchor).
      expect(scrollTopSets).toContain(9999);
    } finally {
      if (originalDesc) Object.defineProperty(HTMLElement.prototype, "scrollTop", originalDesc);
      if (originalScrollHeight)
        Object.defineProperty(HTMLElement.prototype, "scrollHeight", originalScrollHeight);
    }
  });

  it("renders system rows as text and delegates chat bodies to the inbox renderer", () => {
    render(
      <WorkspaceChatBody
        messages={[
          chatMsg(1, { kind: "system", from: "system", text: "Chat started." }),
          chatMsg(2, { from: "human", fromName: "sam" }),
          chatMsg(3),
        ]}
        loading={false}
        error={null}
        density="b"
      />,
    );
    expect(screen.getByTestId("workspace-chat-system")).toHaveTextContent("Chat started.");
    expect(screen.getAllByTestId("inbox-content")).toHaveLength(2);
    const [human, agent] = screen.getAllByTestId("workspace-chat-message");
    expect(human).toHaveAttribute("data-you", "true");
    expect(agent).not.toHaveAttribute("data-you");
  });

  it("places the human unread divider before an agent post, never the human's own post", () => {
    const props = {
      messages: [chatMsg(1, { from: "human", fromName: "sam" })],
      loading: false,
      error: null,
      density: "b" as const,
      unreadAfter: 0,
      unreadExclude: "human",
      unreadLabel: "New since you looked",
    };
    const { rerender } = render(<WorkspaceChatBody {...props} />);
    expect(screen.queryByRole("separator")).toBeNull();

    rerender(<WorkspaceChatBody {...props} messages={[...props.messages, chatMsg(2)]} />);
    const divider = screen.getByRole("separator");
    expect(divider).toHaveTextContent("New since you looked");
    expect(divider.nextElementSibling).toHaveAttribute("data-message-id", "2");
  });
});

it("keeps lifecycle context with a new-chat composer state and renders agent unread independently", () => {
  const lifecycle = chatMsg(1, { kind: "system", text: "Chat started." });
  const { rerender } = render(
    <WorkspaceChatBody
      messages={[lifecycle]}
      loading={false}
      error={null}
      density="b"
      emptyState={<strong>New chat</strong>}
    />,
  );
  expect(screen.getByText("Chat started.")).toBeInTheDocument();
  expect(screen.getByText("New chat")).toBeInTheDocument();
  rerender(
    <WorkspaceChatBody
      messages={[lifecycle, chatMsg(2)]}
      loading={false}
      error={null}
      density="b"
      unreadAfter={1}
      unreadLabel="1 unread by Reviewer"
      identify={() => ({
        name: "Reviewer",
        initials: "R",
        color: "#58a6ff",
        tag: "this session",
        machine: "buildbox",
      })}
    />,
  );
  expect(screen.queryByText("New chat")).toBeNull();
  expect(screen.getByRole("separator")).toHaveTextContent("1 unread by Reviewer");
  expect(screen.getByText("this session")).toBeInTheDocument();
  expect(screen.getByText("buildbox")).toBeInTheDocument();
});
