// SPDX-License-Identifier: MIT
import { INBOX_PANEL_ID } from "@/features/shell/right-pane/right-pane-layout";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { usePaneStore } from "@/features/shell/right-pane/panes/pane-store";
import { useDashboardProfileStore } from "@/features/dashboard/lib/dashboard-profile";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup } from "@testing-library/react";

import { act, fireEvent, render, screen } from "@/test-utils";
import type { ConversationMessage, SessionListRow } from "@/tauri/commands";

const h = vi.hoisted(() => ({
  messages: [] as ConversationMessage[],
  sessions: [] as SessionListRow[],
  /** The agent the read names, when it is not the one asked about (a read still in flight). */
  readOf: null as string | null,
  /** A first read that failed: no data, this error. */
  readError: null as string | null,
  /** Where the read came from. */
  source: "live" as "live" | "fixture",
}));

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock("@/features/inbox/hooks/use-session-inbox", () => ({
  useSessionInbox: (handle: string) => ({
    data: h.readError ? null : { handle: h.readOf ?? handle, roles: [], messages: h.messages },
    source: h.source,
    error: h.readError,
    reload: vi.fn(),
  }),
}));
vi.mock("@/features/inbox/hooks/use-inbox-sessions", () => ({
  useInboxSessions: () => h.sessions,
}));
vi.mock("@/shared/lib/get-current-user", () => ({ useCurrentUser: () => null }));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  markInboxMessageRead: vi.fn(() => Promise.resolve()),
  sendInboxAsHuman: vi.fn(() => Promise.resolve({ status: "sent", messageId: 99 })),
}));

import { NO_TEAM_SENTINEL } from "@/shared/lib/team-key";
import { markInboxMessageRead, sendInboxAsHuman } from "@/tauri/commands";
import { InboxChatPopover } from "./agents-inbox-chat";

beforeEach(() => {
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "backend" });
  useDashboardProfileStore.setState({ org: "acme", profile: null });
  usePaneStore.setState({
    teamKey: "acme:backend",
    pendingIntents: {},
    listing: {
      contentRoot: "/ctx",
      chain: ["backend"],
      panes: [
        {
          id: "inbox",
          title: "Inbox",
          path: "/ctx/panes/inbox.html",
          panesDir: "/ctx/panes",
          panelId: "pane:acme:backend/inbox",
          scope: "team",
          team: "backend",
          dashboardRef: { scope: "team", org: "acme", team: "backend", id: "inbox" },
        },
      ],
    },
  });
});

const AGENT = "qa-worker";

const msg = (id: number): ConversationMessage =>
  ({
    id,
    team: { org: "acme", team: "backend" },
    from: AGENT,
    fromName: AGENT,
    to: "human",
    toName: "human",
    toRole: null,
    text: `m${id}`,
    contentType: "plain",
    createdAtMs: Date.now(),
    inReplyTo: null,
    via: null,
    replyTo: null,
    summary: null,
    unread: false,
    delivery: null,
    kind: "update",
  }) as ConversationMessage;

const ended = {
  handle: AGENT,
  displayName: AGENT,
  state: "exited",
  role: "qa",
  launchedBy: null,
  unread: 0,
  oldestUnreadAt: null,
} as unknown as SessionListRow;

const nextFrame = () =>
  act(async () => {
    await new Promise((resolve) => requestAnimationFrame(() => resolve(null)));
  });

/** The Agents pane's part: the row's button, and the chat it opens, closed as the pane closes it. */
function Pane({
  anchor,
  handle = AGENT,
  teamKey = "acme:backend",
  offlineMachine = null,
}: {
  anchor: HTMLElement;
  handle?: string;
  teamKey?: string;
  offlineMachine?: string | null;
}) {
  const [open, setOpen] = useState(true);
  if (!open) return null;
  return (
    <InboxChatPopover
      name={handle}
      statusWord="Ended"
      workspace={null}
      handle={handle}
      teamKey={teamKey}
      offlineMachine={offlineMachine}
      anchor={anchor}
      onClose={(restoreFocus) => {
        if (restoreFocus) anchor.focus({ preventScroll: true });
        setOpen(false);
      }}
      onOpenInCli={null}
    />
  );
}

describe("the inbox chat of an agent that ended", () => {
  afterEach(() => {
    usePaneStore.setState({ pendingIntents: {} });
    document.body.innerHTML = "";
  });

  it("hands over to its team's role inbox, and closing that returns focus to the agent's row", async () => {
    h.messages = [{ ...msg(1), replyTo: "role:qa" }];
    h.sessions = [ended];
    const anchor = document.createElement("button");
    anchor.textContent = "qa-worker row";
    document.body.appendChild(anchor);
    render(<Pane anchor={anchor} />);

    const open = await screen.findByRole("button", { name: "Open #qa's inbox" });
    open.focus();
    fireEvent.click(open);

    // The chat closes first, focus back on its row.
    expect(screen.queryByRole("dialog", { name: `Messages with ${AGENT}` })).toBeNull();
    expect(anchor).toHaveFocus();
    await nextFrame();
    expect(useViewerStore.getState().rightPaneOpen).toBe(true);
    expect(usePaneStore.getState().pendingIntents[INBOX_PANEL_ID]).toMatchObject({
      conversation: "#qa",
      org: "acme",
      team: "backend",
    });
  });
});

describe("the inbox chat of an agent that ended, with no team to name", () => {
  afterEach(() => {
    usePaneStore.setState({ pendingIntents: {} });
    document.body.innerHTML = "";
  });

  it("offers no role inbox to open: every team's queue of that role is not this one", async () => {
    h.messages = [];
    h.sessions = [ended];
    const anchor = document.createElement("button");
    document.body.appendChild(anchor);
    render(<Pane anchor={anchor} teamKey={NO_TEAM_SENTINEL} />);
    expect(await screen.findByTestId("ibx-ended")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Open #qa's inbox" })).toBeNull();
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    await nextFrame();
    expect(usePaneStore.getState().pendingIntents).toEqual({});
  });

  it("opens the role queue of the team its own messages are in", async () => {
    h.messages = [{ ...msg(1), replyTo: "role:qa" }];
    h.sessions = [ended];
    const anchor = document.createElement("button");
    document.body.appendChild(anchor);
    render(<Pane anchor={anchor} teamKey={NO_TEAM_SENTINEL} />);
    fireEvent.click(await screen.findByRole("button", { name: "Open #qa's inbox" }));
    await nextFrame();
    expect(usePaneStore.getState().pendingIntents[INBOX_PANEL_ID]).toMatchObject({
      org: "acme",
      team: "backend",
    });
  });
});

describe("the inbox chat's New line", () => {
  const OTHER = "qa-other";
  const unread = (id: number, from = AGENT): ConversationMessage => ({
    ...msg(id),
    from,
    fromName: from,
    unread: true,
  });
  afterEach(() => {
    h.readOf = null;
    document.body.innerHTML = "";
  });

  it("stays above what was unread after the read marks it read", () => {
    h.sessions = [];
    h.messages = [msg(1), unread(2), unread(3)];
    const anchor = document.createElement("button");
    document.body.appendChild(anchor);
    const view = render(<Pane anchor={anchor} />);
    const newLine = () => screen.queryByTestId("ibx-new");
    expect(newLine()?.nextElementSibling).toHaveAttribute(
      "data-mid",
      expect.stringContaining("#2"),
    );
    // The reload after the chat read them: nothing is unread any more.
    h.messages = [msg(1), { ...unread(2), unread: false }, { ...unread(3), unread: false }];
    view.rerender(<Pane anchor={anchor} />);
    expect(newLine()?.nextElementSibling).toHaveAttribute(
      "data-mid",
      expect.stringContaining("#2"),
    );
  });

  it("is placed again for another agent, from that agent's own inbox only", () => {
    h.sessions = [];
    h.messages = [msg(1), unread(2)];
    const anchor = document.createElement("button");
    document.body.appendChild(anchor);
    const view = render(<Pane anchor={anchor} />);
    expect(screen.getByTestId("ibx-new")).toBeInTheDocument();
    // Switched to another agent while the read still names the first: no line is placed yet.
    h.readOf = AGENT;
    h.messages = [unread(5, OTHER), unread(6, OTHER)];
    view.rerender(<Pane anchor={anchor} handle={OTHER} />);
    expect(screen.queryByTestId("ibx-new")).toBeNull();
    // Its own inbox arrives: the line goes above its first unread message.
    h.readOf = null;
    view.rerender(<Pane anchor={anchor} handle={OTHER} />);
    expect(screen.getByTestId("ibx-new").nextElementSibling).toHaveAttribute(
      "data-mid",
      expect.stringContaining("#5"),
    );
  });
});

describe("the inbox chat's conversation", () => {
  afterEach(() => {
    h.readError = null;
    document.body.innerHTML = "";
  });
  const inTeam = (id: number, team: string, at: number): ConversationMessage => ({
    ...msg(id),
    team: { org: "acme", team },
    createdAtMs: at,
  });

  it("shows only its own team's mail: a handle names a session within its team", () => {
    h.sessions = [];
    h.messages = [inTeam(1, "backend", 1_000), inTeam(2, "frontend", 2_000)];
    const anchor = document.createElement("button");
    document.body.appendChild(anchor);
    const { container } = render(<Pane anchor={anchor} teamKey="acme:backend" />);
    const shown = [...container.ownerDocument.querySelectorAll(".ibx-m")].map((m) =>
      m.getAttribute("data-mid"),
    );
    expect(shown).toHaveLength(1);
    expect(shown[0]).toContain("#1");
  });

  it("with no mail in the pane's team, follows the team the agent last wrote in, by time", () => {
    h.sessions = [];
    // Listed team by team, not by time: the latest is the first one listed.
    h.messages = [inTeam(1, "frontend", 3_000), inTeam(2, "ops", 2_000)];
    const anchor = document.createElement("button");
    document.body.appendChild(anchor);
    render(<Pane anchor={anchor} teamKey="acme:backend" />);
    const shown = [...document.querySelectorAll(".ibx-m")].map((m) => m.getAttribute("data-mid"));
    expect(shown).toHaveLength(1);
    expect(shown[0]).toContain("#1");
  });

  it("a first read that failed hands focus to Try again", () => {
    h.sessions = [];
    h.readError = "the inbox could not be read";
    const anchor = document.createElement("button");
    document.body.appendChild(anchor);
    render(<Pane anchor={anchor} />);
    expect(screen.getByRole("button", { name: "Try again" })).toHaveFocus();
  });
});

describe("the inbox chat's team", () => {
  afterEach(() => {
    vi.mocked(sendInboxAsHuman).mockClear();
    document.body.innerHTML = "";
  });
  const inTeam = (id: number, team: string, at: number): ConversationMessage => ({
    ...msg(id),
    team: { org: "acme", team },
    createdAtMs: at,
  });
  const shownIds = () =>
    [...document.querySelectorAll(".ibx-m")].map((m) => m.getAttribute("data-mid"));

  it("a send goes to the team it shows, though the agent last wrote in another", async () => {
    h.sessions = [];
    // It wrote in the pane's team, then later in another one (listed last).
    h.messages = [inTeam(1, "backend", 1_000), inTeam(2, "frontend", 2_000)];
    const anchor = document.createElement("button");
    document.body.appendChild(anchor);
    render(<Pane anchor={anchor} teamKey="acme:backend" />);
    const box = screen.getByLabelText("Message");
    fireEvent.change(box, { target: { value: "hello" } });
    await act(async () => {
      fireEvent.keyDown(box, { key: "Enter" });
    });
    expect(sendInboxAsHuman).toHaveBeenCalledTimes(1);
    expect(vi.mocked(sendInboxAsHuman).mock.calls[0]![0]).toEqual({
      org: "acme",
      team: "backend",
    });
  });

  it("is kept once opened: mail arriving in the pane's team does not move the conversation", () => {
    h.sessions = [];
    h.messages = [inTeam(1, "frontend", 3_000)];
    const anchor = document.createElement("button");
    document.body.appendChild(anchor);
    const view = render(<Pane anchor={anchor} teamKey="acme:backend" />);
    expect(shownIds()).toEqual([expect.stringContaining("frontend#1")]);
    h.messages = [inTeam(1, "frontend", 3_000), inTeam(5, "backend", 4_000)];
    view.rerender(<Pane anchor={anchor} teamKey="acme:backend" />);
    expect(shownIds()).toEqual([expect.stringContaining("frontend#1")]);
  });
});

describe("the inbox chat with no messages yet", () => {
  afterEach(() => {
    document.body.innerHTML = "";
  });
  const empty = () => screen.getByTestId("agent-inbox-chat-empty");
  const open = (sessions: SessionListRow[], offlineMachine: string | null = null) => {
    h.messages = [];
    h.sessions = sessions;
    const anchor = document.createElement("button");
    document.body.appendChild(anchor);
    render(<Pane anchor={anchor} offlineMachine={offlineMachine} />);
  };

  it("says a message waits for an offline machine", () => {
    open([{ ...ended, state: "offline", machine: "build-box" } as SessionListRow], "build-box");
    expect(empty()).toHaveTextContent("What you send waits for build-box to reconnect.");
    expect(
      screen.getByText("build-box is offline. Messages are delivered when it reconnects."),
    ).toBeInTheDocument();
  });

  it("names a machine no list knows plainly in the offline note", () => {
    open(
      [{ ...ended, state: "offline", machine: null } as unknown as SessionListRow],
      "a remote machine",
    );
    expect(
      screen.getByText("A remote machine is offline. Messages are delivered when it reconnects."),
    ).toBeInTheDocument();
  });

  it("does not infer a queue from an ended agent's hierarchy role", () => {
    open([ended]);
    expect(empty()).toHaveTextContent("qa-worker has ended");
    expect(empty()).not.toHaveTextContent("#qa");
    expect(screen.queryByRole("button", { name: "Open #qa's inbox" })).toBeNull();
  });

  it("says an idle agent wakes to read what is sent", () => {
    open([{ ...ended, state: "idle" } as SessionListRow]);
    expect(empty()).toHaveTextContent("it wakes to read it");
  });
});

describe("the inbox chat reading what the agent sent", () => {
  const unread = (id: number): ConversationMessage => ({ ...msg(id), unread: true });
  const setVisibility = (state: "hidden" | "visible") => {
    Object.defineProperty(document, "visibilityState", { value: state, configurable: true });
    document.dispatchEvent(new Event("visibilitychange"));
  };
  beforeEach(() => vi.mocked(markInboxMessageRead).mockClear());
  afterEach(() => {
    cleanup();
    h.source = "live";
    setVisibility("visible");
    vi.mocked(markInboxMessageRead).mockReset();
    vi.mocked(markInboxMessageRead).mockImplementation(() => Promise.resolve());
    document.body.innerHTML = "";
  });
  const open = () => {
    h.sessions = [];
    const anchor = document.createElement("button");
    document.body.appendChild(anchor);
    return { anchor, view: render(<Pane anchor={anchor} />) };
  };

  it("marks nothing while the window is hidden, and reads it once it shows", () => {
    h.messages = [unread(1)];
    setVisibility("hidden");
    open();
    expect(markInboxMessageRead).not.toHaveBeenCalled();
    act(() => setVisibility("visible"));
    expect(markInboxMessageRead).toHaveBeenCalledExactlyOnceWith(
      { org: "acme", team: "backend" },
      1,
    );
  });

  it("marks nothing in demo data", () => {
    h.messages = [unread(1)];
    h.source = "fixture";
    open();
    expect(markInboxMessageRead).not.toHaveBeenCalled();
  });

  it("tries a mark that failed again on the next read", async () => {
    vi.mocked(markInboxMessageRead).mockImplementationOnce(() => Promise.reject(new Error("busy")));
    h.messages = [unread(1)];
    const { anchor, view } = open();
    expect(markInboxMessageRead).toHaveBeenCalledTimes(1);
    await act(async () => {
      await Promise.resolve();
    });
    // The next read still has it unread.
    h.messages = [unread(1)];
    view.rerender(<Pane anchor={anchor} />);
    expect(markInboxMessageRead).toHaveBeenCalledTimes(2);
  });
});

it("maximizes the default Inbox dashboard when an ended chat hands off to its role", async () => {
  const previous = {
    org: useOrgStore.getState(),
    pane: usePaneStore.getState(),
    profile: useDashboardProfileStore.getState(),
    viewer: useViewerStore.getState(),
  };
  const panelId = "pane:acme:backend/inbox";
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "backend" });
  useDashboardProfileStore.setState({ org: "acme", profile: null });
  usePaneStore.setState({
    teamKey: "acme:backend",
    knownEntries: {},
    pendingIntents: {},
    listing: {
      contentRoot: "/ctx",
      chain: ["backend"],
      panes: [
        {
          id: "inbox",
          title: "Inbox",
          scope: "team",
          team: "backend",
          surface: "inbox",
          path: "/ctx/panes/inbox.html",
          panesDir: "/ctx/panes",
          panelId,
          dashboardRef: { scope: "team", org: "acme", team: "backend", id: "inbox" },
        },
      ],
    },
  });
  try {
    h.messages = [{ ...msg(1), replyTo: "role:qa" }];
    h.sessions = [ended];
    const anchor = document.createElement("button");
    document.body.appendChild(anchor);
    render(<Pane anchor={anchor} />);
    fireEvent.click(await screen.findByRole("button", { name: "Open #qa's inbox" }));
    await nextFrame();
    expect(usePaneStore.getState().pendingIntents[INBOX_PANEL_ID]).toMatchObject({
      conversation: "#qa",
      org: "acme",
      team: "backend",
    });
    expect(useViewerStore.getState().maximizedPanel["acme:backend"]).toBe(INBOX_PANEL_ID);
  } finally {
    useOrgStore.setState(previous.org);
    usePaneStore.setState(previous.pane);
    useDashboardProfileStore.setState(previous.profile);
    useViewerStore.setState(previous.viewer);
  }
});
