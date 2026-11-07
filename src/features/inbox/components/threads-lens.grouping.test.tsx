// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";

import { act, fireEvent, render, screen, within } from "@/test-utils";
import type { SessionListRow } from "@/tauri/commands";

import type { ConversationMessage } from "../api";
import { HUMAN, buildDirectory, roleSummaries } from "../model";
import { InboxViewProvider, type InboxView } from "./inbox-view-context";
import { Conversation } from "./threads-lens";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
const sendFromInbox = vi.hoisted(() =>
  vi.fn(
    async (_opts: { onFailed?: (why: string, retryable?: boolean) => void; quiet?: boolean }) =>
      false,
  ),
);
vi.mock("../actions", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../actions")>()),
  sendFromInbox,
}));

// The inbox chat (`quick`) draws a sender's close run of messages as one group: the first
// carries who and when, the rest only what is still news about them.

const NOW = new Date(2026, 9, 5, 9, 0).getTime();
const AGENT = "worker-1";

const msg = (id: number, m: Partial<ConversationMessage> = {}): ConversationMessage => ({
  id,
  team: { org: "acme", team: "backend" },
  from: AGENT,
  fromName: AGENT,
  to: HUMAN,
  toName: HUMAN,
  toRole: null,
  text: `m${id}`,
  contentType: "plain",
  createdAtMs: NOW + id * 60_000,
  archivedAtMs: null,
  inReplyTo: null,
  via: null,
  replyTo: null,
  summary: null,
  unread: false,
  delivery: null,
  kind: "update",
  ...m,
});

function renderChat(
  messages: ConversationMessage[],
  opts: {
    sessions?: SessionListRow[];
    holdings?: { role: string; instanceId: string }[];
    openRoleInbox?: (roleKey: string) => void;
    offlineNoted?: boolean;
    quick?: boolean;
  } = {},
) {
  const dir = buildDirectory(opts.sessions ?? [], messages, { holdings: opts.holdings });
  const view: InboxView = {
    me: HUMAN,
    targets: [AGENT],
    dir,
    messages,
    roles: roleSummaries(dir, messages),
    held: [],
    fallbackTeam: null,
    source: "live",
    replyTo: () => {},
    goTo: () => {},
    openRoleInbox: opts.openRoleInbox,
    offlineNoted: opts.offlineNoted,
  };
  const { container } = render(
    <InboxViewProvider value={view}>
      <Conversation
        thread={{ key: AGENT, messages }}
        request={null}
        defaultTo={AGENT}
        quick={opts.quick ?? true}
      />
    </InboxViewProvider>,
  );
  const bubbles = () => Array.from(container.querySelectorAll<HTMLElement>(".ibx-m"));
  return { bubbles };
}

describe("the inbox chat groups a sender's close messages", () => {
  it("two messages a minute apart from the same sender share one sender-and-time line", () => {
    const { bubbles } = renderChat([msg(1), msg(2)]);
    const [first, second] = bubbles();
    expect(first.querySelector(".meta .who")).not.toBeNull();
    expect(second.classList.contains("grouped")).toBe(true);
    expect(second.querySelector(".meta")).toBeNull();
  });

  it("a message stamped before the one above it (out of order) starts its own group", () => {
    const { bubbles } = renderChat([msg(2), msg(1)]);
    const [, second] = bubbles();
    expect(second.classList.contains("grouped")).toBe(false);
    expect(second.querySelector(".meta .who")).not.toBeNull();
  });

  it("a grouped message still says where replies go: only who and when are left out", () => {
    const { bubbles } = renderChat([msg(1), msg(2, { replyTo: "lead-1" })]);
    const [, second] = bubbles();
    expect(second.classList.contains("grouped")).toBe(true);
    expect(second.querySelector(".meta .who")).toBeNull();
    expect(second.querySelector("time")).toBeNull();
    expect(second.querySelector("[data-testid='ibx-replies-go-to']")).not.toBeNull();
  });
});

describe("a grouped message keeps every mark its line would carry", () => {
  it("one typed into the terminal still says so", () => {
    const { bubbles } = renderChat([msg(1, { via: "terminal" }), msg(2, { via: "terminal" })]);
    const [, second] = bubbles();
    expect(second.classList.contains("grouped")).toBe(true);
    expect(second.querySelector(".meta .who")).toBeNull();
    expect(second.querySelector("[data-testid='ibx-typed-via']")).not.toBeNull();
  });

  it("a claimed role message still shows its badge", () => {
    const claimed = {
      toRole: "qa",
      to: "#qa",
      delivery: { state: "claimed", claimedBy: "worker-2", claimedAt: NOW } as never,
    };
    const { bubbles } = renderChat([msg(1, claimed), msg(2, claimed)]);
    const [, second] = bubbles();
    expect(second.classList.contains("grouped")).toBe(true);
    expect(second.querySelector("[data-testid='ibx-claim']")).not.toBeNull();
  });
});

describe("the chat's composer for an agent that has ended", () => {
  const ended = (role: string | null) =>
    ({
      handle: AGENT,
      displayName: AGENT,
      state: "exited",
      role,
      launchedBy: null,
      unread: 0,
      oldestUnreadAt: null,
    }) as unknown as SessionListRow;

  it("whose mail would go to its role offers that role's inbox, not a switch it can't make", () => {
    const openRoleInbox = vi.fn();
    renderChat([msg(1)], {
      sessions: [ended(null)],
      holdings: [{ role: "qa", instanceId: AGENT }],
      openRoleInbox,
    });
    expect(screen.getByTestId("ibx-ended")).toHaveTextContent("ended");
    expect(screen.queryByRole("button", { name: "Send to #qa instead" })).toBeNull();
    const open = screen.getByRole("button", { name: "Open #qa's inbox" });
    // Beside the announced reason, not inside its live region.
    const status = within(screen.getByTestId("ibx-ended")).getByRole("status");
    expect(status).toHaveTextContent("ended");
    expect(status).not.toContainElement(open);
    fireEvent.click(open);
    expect(openRoleInbox).toHaveBeenCalledWith("#qa");
  });

  it("with no role, says it ended and offers nothing", () => {
    renderChat([msg(1)], { sessions: [ended(null)], openRoleInbox: vi.fn() });
    expect(screen.getByTestId("ibx-ended")).toBeInTheDocument();
    expect(screen.queryByTestId("ibx-open-role-inbox")).toBeNull();
  });
});

describe("the chat's composer after a send that failed", () => {
  it("says so for that conversation only: another one shows no failure", async () => {
    const OTHER = "worker-2";
    const view = (key: string): InboxView => {
      const messages = [msg(1, { from: key, fromName: key })];
      const dir = buildDirectory([], messages, {});
      return {
        me: HUMAN,
        targets: [key],
        dir,
        messages,
        roles: roleSummaries(dir, messages),
        held: [],
        fallbackTeam: null,
        source: "live",
        replyTo: () => {},
        goTo: () => {},
      };
    };
    const ui = (key: string) => (
      <InboxViewProvider value={view(key)}>
        <Conversation
          thread={{ key, messages: view(key).messages }}
          request={null}
          defaultTo={key}
          quick
        />
      </InboxViewProvider>
    );
    const { rerender } = render(ui(AGENT));
    const box = screen.getByLabelText("Message");
    fireEvent.change(box, { target: { value: "hello" } });
    await act(async () => {
      fireEvent.keyDown(box, { key: "Enter" });
    });
    expect(sendFromInbox).toHaveBeenCalled();
    expect(screen.getByTestId("ibx-send-failed")).toBeInTheDocument();
    rerender(ui(OTHER));
    expect(screen.queryByTestId("ibx-send-failed")).toBeNull();
  });
});

const session = (state: string, extra: Record<string, unknown> = {}) =>
  ({
    handle: AGENT,
    displayName: AGENT,
    state,
    role: null,
    launchedBy: null,
    unread: 0,
    oldestUnreadAt: null,
    ...extra,
  }) as unknown as SessionListRow;

describe("Reply-to chips", () => {
  it("the chat draws none (everything goes to its agent); a conversation pane does", () => {
    const sessions = [session("idle")];
    const holdings = [{ role: "qa", instanceId: AGENT }];
    const chat = renderChat([msg(1)], { sessions, holdings });
    expect(chat.bubbles()[0]!.querySelector(".ibx-rto")).toBeNull();
    document.body.innerHTML = "";
    const pane = renderChat([msg(1)], { sessions, holdings, quick: false });
    const chips = pane.bubbles()[0]!.querySelector(".ibx-rto");
    expect(chips).toHaveTextContent("Reply to");
    expect(chips).toHaveTextContent("#qa");
  });
});

describe("the chat composer's line", () => {
  it("for an offline agent defers to the chat's own offline note when it shows one", () => {
    const sessions = [session("offline", { machine: "build-box" })];
    renderChat([msg(1)], { sessions, offlineNoted: true });
    expect(screen.getByTestId("ibx-route-hint")).toHaveTextContent(
      `To ${AGENT}'s inbox · you stay on your tab`,
    );
    document.body.innerHTML = "";
    renderChat([msg(1)], { sessions });
    expect(screen.getByTestId("ibx-route-hint")).toHaveTextContent("reconnects");
  });

  it("for an ended agent draws no empty hint under the box", () => {
    renderChat([msg(1)], { sessions: [session("exited")] });
    expect(screen.queryByTestId("ibx-route-hint")).toBeNull();
  });

  it("an Enter on an empty box sends nothing and says to write a message first", async () => {
    sendFromInbox.mockClear();
    vi.useFakeTimers();
    try {
      renderChat([msg(1)], { sessions: [session("idle")] });
      fireEvent.keyDown(screen.getByLabelText("Message"), { key: "Enter" });
      expect(sendFromInbox).not.toHaveBeenCalled();
      expect(screen.getByTestId("ibx-send-empty")).toHaveTextContent("Write a message first");
      act(() => {
        vi.advanceTimersByTime(2500);
      });
      expect(screen.queryByTestId("ibx-send-empty")).toBeNull();
      expect(screen.getByTestId("ibx-route-hint")).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("a failed send says why and keeps where the message was going", async () => {
    sendFromInbox.mockImplementationOnce(async (opts) => {
      opts.onFailed?.("the team is not synced.");
      return false;
    });
    renderChat([msg(1)], { sessions: [session("idle")] });
    const box = screen.getByLabelText("Message");
    fireEvent.change(box, { target: { value: "hello" } });
    await act(async () => {
      fireEvent.keyDown(box, { key: "Enter" });
    });
    expect(screen.getByTestId("ibx-send-failed")).toHaveTextContent(
      "Not sent: the team is not synced. Your message is still here",
    );
    expect(screen.getByTestId("ibx-route-hint")).toBeInTheDocument();
  });

  it("offers ↵ again only for a send that could not reach the backend, not one it refused", async () => {
    const fail = async (retryable: boolean) => {
      document.body.innerHTML = "";
      sendFromInbox.mockImplementationOnce(async (opts) => {
        opts.onFailed?.("no such session", retryable);
        return false;
      });
      renderChat([msg(1)], { sessions: [session("idle")] });
      const box = screen.getByLabelText("Message");
      fireEvent.change(box, { target: { value: "hello" } });
      await act(async () => {
        fireEvent.keyDown(box, { key: "Enter" });
      });
      return screen.getByTestId("ibx-send-failed").textContent;
    };
    expect(await fail(false)).toBe("Not sent: no such session. Your message is still here.");
    expect(await fail(true)).toBe(
      "Not sent: no such session. Your message is still here; press ↵ to try again.",
    );
  });

  it("a send to an agent whose machine is offline is not quiet: the toast confirms it queued", async () => {
    sendFromInbox.mockClear();
    const send = async (state: string) => {
      document.body.innerHTML = "";
      renderChat([msg(1)], { sessions: [session(state, { machine: "build-box" })] });
      const box = screen.getByLabelText("Message");
      fireEvent.change(box, { target: { value: "hello" } });
      await act(async () => {
        fireEvent.keyDown(box, { key: "Enter" });
      });
      return sendFromInbox.mock.calls.at(-1)![0].quiet;
    };
    expect(await send("offline")).toBe(false);
    expect(await send("idle")).toBe(true);
  });
});

describe("the chat composer's keys and button", () => {
  it("keeps Send enabled on an empty box, and a click says to write a message first", () => {
    sendFromInbox.mockClear();
    renderChat([msg(1)], { sessions: [session("idle")] });
    const button = screen.getByRole("button", { name: "Send" });
    expect(button).toBeEnabled();
    fireEvent.click(button);
    expect(sendFromInbox).not.toHaveBeenCalled();
    expect(screen.getByTestId("ibx-send-empty")).toHaveTextContent("Write a message first");
  });

  it("⌥↵ starts a new line at the caret and sends nothing", () => {
    sendFromInbox.mockClear();
    renderChat([msg(1)], { sessions: [session("idle")] });
    const box = screen.getByLabelText<HTMLTextAreaElement>("Message");
    fireEvent.change(box, { target: { value: "ab" } });
    box.setSelectionRange(1, 1);
    fireEvent.keyDown(box, { key: "Enter", altKey: true });
    expect(sendFromInbox).not.toHaveBeenCalled();
    expect(box.value).toBe("a\nb");
    expect(box.selectionStart).toBe(2);
  });

  it("⌥↵ over a selection replaces it with the new line", () => {
    renderChat([msg(1)], { sessions: [session("idle")] });
    const box = screen.getByLabelText<HTMLTextAreaElement>("Message");
    fireEvent.change(box, { target: { value: "abcd" } });
    box.setSelectionRange(1, 3);
    fireEvent.keyDown(box, { key: "Enter", altKey: true });
    expect(box.value).toBe("a\nd");
    expect(box.selectionStart).toBe(2);
    expect(box.selectionEnd).toBe(2);
  });

  it("⌥↵ while a send is on its way changes nothing", async () => {
    let settle!: (ok: boolean) => void;
    sendFromInbox.mockImplementationOnce(
      () =>
        new Promise<boolean>((resolve) => {
          settle = resolve;
        }),
    );
    renderChat([msg(1)], { sessions: [session("idle")] });
    const box = screen.getByLabelText<HTMLTextAreaElement>("Message");
    fireEvent.change(box, { target: { value: "hello" } });
    await act(async () => {
      fireEvent.keyDown(box, { key: "Enter" });
    });
    expect(box).toHaveAttribute("readonly");
    fireEvent.keyDown(box, { key: "Enter", altKey: true });
    expect(box.value).toBe("hello");
    await act(async () => settle(true));
  });

  it("a new line by ⌥↵ is an edit: it clears a failure and the empty-box nudge", async () => {
    sendFromInbox.mockImplementationOnce(async (opts) => {
      opts.onFailed?.("the team is not synced", true);
      return false;
    });
    renderChat([msg(1)], { sessions: [session("idle")] });
    const box = screen.getByLabelText<HTMLTextAreaElement>("Message");
    fireEvent.change(box, { target: { value: "hello" } });
    await act(async () => {
      fireEvent.keyDown(box, { key: "Enter" });
    });
    expect(screen.getByTestId("ibx-send-failed")).toBeInTheDocument();
    fireEvent.keyDown(box, { key: "Enter", altKey: true });
    expect(screen.queryByTestId("ibx-send-failed")).toBeNull();

    fireEvent.change(box, { target: { value: "" } });
    fireEvent.keyDown(box, { key: "Enter" });
    expect(screen.getByTestId("ibx-send-empty")).toBeInTheDocument();
    fireEvent.keyDown(box, { key: "Enter", altKey: true });
    expect(screen.queryByTestId("ibx-send-empty")).toBeNull();
  });
});

describe("the chat composer's state belongs to the conversation that sent", () => {
  const OTHER = "worker-2";
  const view = (key: string): InboxView => {
    const messages = [msg(1, { from: key, fromName: key })];
    const dir = buildDirectory([], messages, {});
    return {
      me: HUMAN,
      targets: [key],
      dir,
      messages,
      roles: roleSummaries(dir, messages),
      held: [],
      fallbackTeam: null,
      source: "live",
      replyTo: () => {},
      goTo: () => {},
    };
  };
  const ui = (key: string) => (
    <InboxViewProvider value={view(key)}>
      <Conversation
        thread={{ key, messages: view(key).messages }}
        request={null}
        defaultTo={key}
        quick
      />
    </InboxViewProvider>
  );

  it("a send in flight leaves another conversation's box writable, and its failure lands on its own", async () => {
    let settle!: (ok: boolean) => void;
    let failed!: (why: string) => void;
    sendFromInbox.mockImplementationOnce(
      (opts) =>
        new Promise<boolean>((resolve) => {
          failed = (why) => opts.onFailed?.(why);
          settle = resolve;
        }),
    );
    const { rerender } = render(ui(AGENT));
    const box = () => screen.getByLabelText<HTMLTextAreaElement>("Message");
    fireEvent.change(box(), { target: { value: "hello" } });
    await act(async () => {
      fireEvent.keyDown(box(), { key: "Enter" });
    });
    expect(box()).toHaveAttribute("readonly");
    rerender(ui(OTHER));
    expect(box()).not.toHaveAttribute("readonly");
    await act(async () => {
      failed("the team is not synced");
      settle(false);
    });
    expect(screen.queryByTestId("ibx-send-failed")).toBeNull();
    rerender(ui(AGENT));
    expect(screen.getByTestId("ibx-send-failed")).toHaveTextContent(
      "Not sent: the team is not synced",
    );
  });
});
