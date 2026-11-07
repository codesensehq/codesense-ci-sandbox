// SPDX-License-Identifier: MIT
import { SESSION_STATE_CHANGED_EVENT } from "@/features/viewer/lib/session-events";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { act, fireEvent, render, screen, waitFor, within } from "@/test-utils";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { AGENT_INBOX_CHANGED_EVENT } from "@/tauri/events";
import { toast } from "sonner";
import {
  openAgentInboxRail,
  useAgentInboxRailStore,
} from "@/features/viewer/stores/agent-inbox-rail-store";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import type { AgentSessionRow } from "@/tauri/commands";

import type { SessionListRow } from "@/tauri/commands";

import type { SessionInboxDto } from "../api";
import { FX, fixtureSessionInbox, fixtureSessions } from "../fixtures";
import { useInboxSessionsStore } from "../hooks/use-inbox-sessions";

vi.mock("@tauri-apps/api/core", async (original) => ({
  ...(await original<typeof import("@tauri-apps/api/core")>()),
  invoke: vi.fn(async () => undefined),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
const locate = vi.fn((_handle: string) => ({ located: true }));
vi.mock("@/features/weft/lib/locate-agent", () => ({
  locateAgentByAgentId: (h: string) => locate(h),
}));
vi.mock("sonner", () => ({ toast: vi.fn() }));

const api = vi.hoisted(() => ({
  loadSessionInbox: vi.fn(),
  loadInboxSessions: vi.fn(),
  sendAsHuman: vi.fn(),
}));
vi.mock("../api", async (orig) => ({ ...(await orig<typeof import("../api")>()), ...api }));

import { useTabInboxCounts, useTabInboxUnread } from "../hooks/use-tab-inbox-unread";
import { InboxRail } from "./inbox-rail";
import { InboxSplit } from "./inbox-split";
import { clearInboxDrafts } from "./inbox-view-context";

const TEAM_KEY = "alderco:default";

function inbox(dto: SessionInboxDto) {
  api.loadSessionInbox.mockResolvedValue({ dto, source: "live" });
}

async function renderRail(handle: string, tabId = "tab-1", narrow = false) {
  const utils = render(
    <InboxRail
      tabId={tabId}
      sessionId={handle}
      teamKey={TEAM_KEY}
      composeRequest={0}
      narrow={narrow}
    />,
  );
  await within(utils.container).findByTestId("inbox-rail");
  await waitFor(() => expect(api.loadSessionInbox).toHaveBeenCalledWith(handle));
  return utils;
}

beforeEach(() => {
  clearInboxDrafts();
  vi.clearAllMocks();
  useInboxSessionsStore.setState({ sessions: null });
  useAgentListProjectionStore.setState({ sessions: null });
  useAgentInboxRailStore.setState({ bySession: {}, widthByTab: {}, lensByTab: {} });
  api.loadInboxSessions.mockResolvedValue(fixtureSessions());
  api.sendAsHuman.mockResolvedValue({ status: "sent", messageId: 99 });
});

describe("Threads lens", () => {
  it("shows a session's sent and received messages, with text typed into its terminal marked", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    await renderRail(FX.t9);
    const msgs = await screen.findByTestId("ibx-msgs");
    await waitFor(() => expect(msgs.querySelectorAll(".ibx-m")).toHaveLength(4));
    const out = msgs.querySelectorAll(".ibx-m.out");
    const incoming = msgs.querySelectorAll(".ibx-m.in");
    expect(out).toHaveLength(2);
    expect(incoming).toHaveLength(2);
    expect([...out].every((el) => el.classList.contains("term"))).toBe(true);
    expect(within(msgs).getAllByTestId("ibx-typed-via")).toHaveLength(2);
    expect(within(msgs).getByText(/used up all his cache/)).toBeInTheDocument();
    // The conversation is unread on the child's last reply, so it sits in the Unread lane.
    expect(screen.getByText("Unread")).toBeInTheDocument();
  });

  it('names the tab\'s own messages by its tab name: "You" is only the human reading', async () => {
    inbox(fixtureSessionInbox(FX.t9));
    await renderRail(FX.t9);
    const msgs = await screen.findByTestId("ibx-msgs");
    await waitFor(() => expect(msgs.querySelectorAll(".ibx-m.out")).toHaveLength(2));
    for (const m of msgs.querySelectorAll(".ibx-m.out")) {
      const meta = m.querySelector(".meta")!.textContent!;
      expect(meta).not.toMatch(/^You/);
      expect(meta).toMatch(/^Terminal 9/);
    }
    expect(screen.queryByText(/^You: /)).toBeNull();
  });

  it("shows the list or a thread on a narrow rail, never two cramped columns", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    await renderRail(FX.t9, "tab-1", true);
    const list = await screen.findByRole("list", { name: "Conversations" });
    await waitFor(() => expect(list.querySelector("[data-thread]")).not.toBeNull());
    expect(screen.queryByTestId("ibx-msgs")).toBeNull();
    fireEvent.click(list.querySelector<HTMLElement>("[data-thread]")!);
    expect(await screen.findByTestId("ibx-msgs")).toBeInTheDocument();
    expect(screen.queryByRole("list", { name: "Conversations" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Back to the inbox" }));
    expect(await screen.findByRole("list", { name: "Conversations" })).toBeInTheDocument();
  });

  it("lists a held role's queue in its own lane with the claim state; reading never claims", async () => {
    inbox(fixtureSessionInbox(FX.rev));
    await renderRail(FX.rev);
    await screen.findByText("Queued for", { exact: false });
    fireEvent.click(document.querySelector('[data-thread="#reviewer"]')!);
    expect(await screen.findByTestId("ibx-claim")).toHaveTextContent("queued · unclaimed");
    expect(screen.getByText(/Reading does not claim it/)).toBeInTheDocument();
    // The tray shows the held queue with its pending count.
    const chip = screen.getByTitle(/Queued for #reviewer/);
    expect(chip).toHaveTextContent("reviewer");
    expect(chip).toHaveTextContent("1");
  });

  it("shows who claimed a role message once an agent has", async () => {
    const dto = fixtureSessionInbox(FX.rev);
    dto.messages = dto.messages.map((m) =>
      m.toRole ? { ...m, delivery: { state: "claimed" as const, claimedBy: FX.rev } } : m,
    );
    inbox(dto);
    await renderRail(FX.rev);
    fireEvent.click(await waitFor(() => document.querySelector('[data-thread="#reviewer"]')!));
    expect(await screen.findByTestId("ibx-claim")).toHaveTextContent("claimed by review-DEMO-4");
  });

  it("gives a hand-started tab with no mail an honest empty state, not a missing pane", async () => {
    inbox({ handle: "shell-new", roles: [], messages: [] });
    await renderRail("shell-new");
    expect(await screen.findByTestId("ibx-empty")).toHaveTextContent("No messages yet");
    expect(screen.getByText("No role held")).toBeInTheDocument();
    expect(screen.getByText("Take a role")).toBeInTheDocument();
    expect(screen.getByText("Start a conversation")).toBeInTheDocument();
  });

  it("disables Go to session for an exited session, with the reason, and jumps to a live one", async () => {
    inbox(fixtureSessionInbox(FX.ctrl));
    await renderRail(FX.ctrl);
    await waitFor(() => expect(document.querySelector(`[data-thread="${FX.qa}"]`)).not.toBeNull());
    const qaGo = within(
      document.querySelector(`[data-thread="${FX.qa}"]`) as HTMLElement,
    ).getByLabelText(/Go to qa-DEMO-3/);
    expect(qaGo).toHaveAttribute("aria-disabled", "true");
    expect(qaGo).toHaveAttribute("title", "Session exited, cannot open");
    fireEvent.click(qaGo);
    expect(locate).not.toHaveBeenCalled();
    const implRow = document.querySelector(`[data-thread="${FX.impl}"]`) as HTMLElement;
    fireEvent.click(within(implRow).getByLabelText("Go to impl-DEMO-5"));
    expect(locate).toHaveBeenCalledWith(FX.impl);
  });
});

describe("composer routing", () => {
  it("routes to a role queue through the To picker, saying what happens, and sends as the human", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    await renderRail(FX.t9);
    await screen.findByTestId("ibx-msgs");
    expect(screen.getByTestId("ibx-route-hint")).toHaveTextContent(/idle session is woken/);
    fireEvent.click(screen.getByTestId("ibx-to"));
    const picker = await screen.findByTestId("ibx-picker");
    expect(within(picker).getByText("@amelia-child-5plus5")).toBeInTheDocument();
    expect(within(picker).getByText("@you")).toBeInTheDocument();
    fireEvent.change(within(picker).getByLabelText("Find recipient"), {
      target: { value: "#rev" },
    });
    fireEvent.keyDown(within(picker).getByLabelText("Find recipient"), { key: "Enter" });
    expect(screen.getByTestId("ibx-route-hint")).toHaveTextContent(
      "Queued on #reviewer · review-DEMO-4 holds it",
    );
    const box = screen.getByLabelText("Message");
    fireEvent.change(box, { target: { value: "please look" } });
    fireEvent.keyDown(box, { key: "Enter", metaKey: true });
    await waitFor(() =>
      expect(api.sendAsHuman).toHaveBeenCalledWith(
        { org: "alderco", team: "default" },
        "#reviewer",
        "please look",
        null,
      ),
    );
  });

  it("answers a role-holding sender with @agent or #role chips", async () => {
    // A message from the implementer (who holds #implementer) into the reviewer's own inbox.
    const dto = fixtureSessionInbox(FX.rev);
    dto.messages.push({
      ...dto.messages[0]!,
      id: 50,
      from: FX.impl,
      fromName: "impl-DEMO-5",
      to: FX.rev,
      toRole: null,
      delivery: null,
      replyTo: "role:implementer",
    });
    inbox(dto);
    await renderRail(FX.rev);
    fireEvent.click(await waitFor(() => document.querySelector(`[data-thread="${FX.impl}"]`)!));
    const chips = await screen.findByRole("button", { name: "#implementer" });
    fireEvent.click(chips);
    expect(screen.getByTestId("ibx-route-hint")).toHaveTextContent(/Queued on #implementer/);
    const box = screen.getByLabelText("Message");
    fireEvent.change(box, { target: { value: "ack" } });
    fireEvent.keyDown(box, { key: "Enter", metaKey: true });
    // A reply chip answers the message it sits under.
    await waitFor(() =>
      expect(api.sendAsHuman).toHaveBeenCalledWith(expect.anything(), "#implementer", "ack", 50),
    );
  });

  it("shows where a message's replies go and aims the composer there (Reply-To)", async () => {
    const dto = fixtureSessionInbox(FX.rev);
    dto.messages.push({
      ...dto.messages[0]!,
      id: 51,
      from: FX.impl,
      fromName: "impl-DEMO-5",
      to: FX.rev,
      toRole: null,
      via: null,
      delivery: null,
      replyTo: "role:manager",
    });
    inbox(dto);
    await renderRail(FX.rev);
    fireEvent.click(await waitFor(() => document.querySelector(`[data-thread="${FX.impl}"]`)!));
    expect(await screen.findByTestId("ibx-replies-go-to")).toHaveTextContent(
      "replies go to manager",
    );
    expect(screen.getByTestId("ibx-to")).toHaveTextContent("manager");
  });

  it("renders a withdrawn role message back on its queue", async () => {
    const dto = fixtureSessionInbox(FX.rev);
    dto.messages = dto.messages.map((m) =>
      m.toRole
        ? {
            ...m,
            toRole: null,
            to: m.toRole,
            toName: m.toRole,
            delivery: { state: "withdrawn" as const, claimedBy: null },
          }
        : m,
    );
    inbox(dto);
    await renderRail(FX.rev);
    fireEvent.click(await waitFor(() => document.querySelector('[data-thread="#reviewer"]')!));
    expect(await screen.findByTestId("ibx-claim")).toHaveTextContent("withdrawn");
  });
});

describe("ended and offline recipients", () => {
  /** The demo roster with some rows changed. */
  const roster = (patch: Record<string, Partial<SessionListRow>>) =>
    api.loadInboxSessions.mockResolvedValue(
      fixtureSessions().map((s) => ({ ...s, ...patch[s.handle] })),
    );
  const openThread = async (k: string) =>
    fireEvent.click(await waitFor(() => document.querySelector(`[data-thread="${k}"]`)!));

  it("disables Send to an ended session with the reason, and switches to its role keeping the draft", async () => {
    roster({ [FX.qa]: { role: "qa" } });
    const dto = fixtureSessionInbox(FX.ctrl);
    dto.messages = dto.messages.map((m) => (m.from === FX.qa ? { ...m, replyTo: "role:qa" } : m));
    inbox(dto);
    await renderRail(FX.ctrl);
    await openThread(FX.qa);
    fireEvent.click(screen.getByTestId("ibx-to"));
    fireEvent.click(within(screen.getByTestId("ibx-picker")).getByText("@qa-DEMO-3"));
    const reason = await screen.findByTestId("ibx-ended");
    expect(reason).toHaveTextContent("qa-DEMO-3 ended — messages to it won't be read");
    expect(screen.getByRole("button", { name: /^Send ⌘/ })).toBeDisabled();
    const box = screen.getByLabelText("Message");
    fireEvent.change(box, { target: { value: "rerun the checks" } });
    fireEvent.keyDown(box, { key: "Enter", metaKey: true });
    expect(api.sendAsHuman).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Send to #qa instead" }));
    expect(screen.getByTestId("ibx-to")).toHaveTextContent("qa");
    expect(screen.queryByTestId("ibx-ended")).toBeNull();
    expect(screen.getByLabelText("Message")).toHaveValue("rerun the checks");
    expect(screen.getByRole("button", { name: /^Send ⌘/ })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: /^Send ⌘/ }));
    await waitFor(() =>
      expect(api.sendAsHuman).toHaveBeenCalledWith(
        expect.anything(),
        "#qa",
        "rerun the checks",
        null,
      ),
    );
  });

  it("lists ended sessions dimmed as ended in the To picker; picking one shows the same block", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    await renderRail(FX.t9);
    await screen.findByTestId("ibx-msgs");
    fireEvent.click(screen.getByTestId("ibx-to"));
    const picker = await screen.findByTestId("ibx-picker");
    const qa = within(picker).getByText("@qa-DEMO-3").closest("[role=option]")!;
    expect(qa).toHaveClass("ended");
    expect(qa).toHaveTextContent("ended");
    const live = within(picker).getByText("@impl-DEMO-5").closest("[role=option]")!;
    expect(live).not.toHaveClass("ended");
    fireEvent.click(qa);
    expect(await screen.findByTestId("ibx-ended")).toHaveTextContent("qa-DEMO-3 ended");
    // No role known: no way round it, only the reason.
    expect(screen.queryByRole("button", { name: /instead$/ })).toBeNull();
    expect(screen.getByRole("button", { name: /^Send ⌘/ })).toBeDisabled();
  });

  it("allows sending to a session whose machine is offline, with a quiet hint", async () => {
    roster({ [FX.rev]: { state: "offline" } });
    inbox(fixtureSessionInbox(FX.ctrl));
    await renderRail(FX.ctrl);
    await openThread(FX.rev);
    await waitFor(() =>
      expect(screen.getByTestId("ibx-route-hint")).toHaveTextContent(
        "Delivered when buildbox reconnects",
      ),
    );
    expect(screen.queryByTestId("ibx-ended")).toBeNull();
    const box = screen.getByLabelText("Message");
    fireEvent.change(box, { target: { value: "when you are back" } });
    fireEvent.keyDown(box, { key: "Enter", metaKey: true });
    await waitFor(() =>
      expect(api.sendAsHuman).toHaveBeenCalledWith(
        expect.anything(),
        FX.rev,
        "when you are back",
        null,
      ),
    );
  });

  it("aims a Reply to an ended sender at its role when it held one", async () => {
    roster({ [FX.qa]: { role: "qa" } });
    const dto = fixtureSessionInbox(FX.ctrl);
    dto.messages = dto.messages.map((m) => (m.from === FX.qa ? { ...m, replyTo: "role:qa" } : m));
    inbox(dto);
    await renderRail(FX.ctrl);
    const row = await waitFor(() => document.querySelector(`[data-thread="${FX.qa}"]`)!);
    fireEvent.click(within(row as HTMLElement).getByLabelText("Reply to qa-DEMO-3"));
    await waitFor(() => expect(screen.getByTestId("ibx-to")).toHaveTextContent("qa"));
    expect(screen.queryByTestId("ibx-ended")).toBeNull();
  });

  it("keeps a Reply to an ended sender with no known role on it, with Send disabled and why", async () => {
    inbox(fixtureSessionInbox(FX.ctrl));
    await renderRail(FX.ctrl);
    const row = await waitFor(() => document.querySelector(`[data-thread="${FX.qa}"]`)!);
    fireEvent.click(within(row as HTMLElement).getByLabelText("Reply to qa-DEMO-3"));
    expect(await screen.findByTestId("ibx-ended")).toHaveTextContent(
      "qa-DEMO-3 ended — messages to it won't be read",
    );
    expect(screen.getByTestId("ibx-to")).toHaveTextContent("@qa-DEMO-3");
    expect(screen.getByRole("button", { name: /^Send ⌘/ })).toBeDisabled();
  });
});

describe("Mailspool lens", () => {
  it("draws one line per message, filters typed text, and opens a thread inline to reply", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    useAgentInboxRailStore.setState({ lensByTab: { "tab-1": "spool" } });
    await renderRail(FX.t9);
    const log = await screen.findByTestId("ibx-log");
    await waitFor(() => expect(log.querySelectorAll(".ibx-lrow")).toHaveLength(4));
    expect([...log.querySelectorAll(".ibx-lrow .dir")].map((d) => d.textContent)).toEqual([
      "⇢",
      "←",
      "⇢",
      "←",
    ]);
    fireEvent.click(screen.getByRole("button", { name: "via:terminal" }));
    expect(log.querySelectorAll(".ibx-lrow")).toHaveLength(2);
    fireEvent.click(screen.getByRole("button", { name: "all" }));
    // j/k move the selection, ↵ opens the thread with its reply prompt.
    fireEvent.keyDown(log, { key: "j" });
    fireEvent.keyDown(log, { key: "j" });
    expect(log.querySelector(".ibx-lrow.sel")).toHaveAttribute("data-log", "alderco/default#9");
    fireEvent.keyDown(log, { key: "Enter" });
    expect(screen.getByText("› reply to @amelia-child-5plus5:")).toBeInTheDocument();
    const reply = log.querySelector<HTMLInputElement>("[data-logreply]")!;
    fireEvent.change(reply, { target: { value: "thanks" } });
    await act(async () => {
      fireEvent.keyDown(reply, { key: "Enter" });
    });
    // The inline reply answers the line that was opened (#9), not just the latest one.
    expect(api.sendAsHuman).toHaveBeenCalledWith(expect.anything(), FX.child, "thanks", 9);
  });

  it("answers the latest line the other side sent when the opened line is your own", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    useAgentInboxRailStore.setState({ lensByTab: { "tab-1": "spool" } });
    await renderRail(FX.t9);
    const log = await screen.findByTestId("ibx-log");
    // #10 is t9's own typed line: the reply answers the child's latest (#11).
    fireEvent.click(await waitFor(() => log.querySelector('[data-log="alderco/default#10"]')!));
    const reply = log.querySelector<HTMLInputElement>("[data-logreply]")!;
    fireEvent.change(reply, { target: { value: "ok" } });
    await act(async () => {
      fireEvent.keyDown(reply, { key: "Enter" });
    });
    expect(api.sendAsHuman).toHaveBeenCalledWith(expect.anything(), FX.child, "ok", 11);
  });

  it("never sends on the ↵ that ends an IME composition in the reply line", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    useAgentInboxRailStore.setState({ lensByTab: { "tab-1": "spool" } });
    await renderRail(FX.t9);
    const log = await screen.findByTestId("ibx-log");
    fireEvent.click(await waitFor(() => log.querySelector('[data-log="alderco/default#11"]')!));
    const reply = log.querySelector<HTMLInputElement>("[data-logreply]")!;
    fireEvent.change(reply, { target: { value: "こんにちは" } });
    await act(async () => {
      fireEvent.keyDown(reply, { key: "Enter", keyCode: 229 });
    });
    expect(api.sendAsHuman).not.toHaveBeenCalled();
  });

  it("leaves ⌘/⌃/⌥ chords to the app: only bare j, k, r and g act on the log", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    useAgentInboxRailStore.setState({ lensByTab: { "tab-1": "spool" } });
    await renderRail(FX.t9);
    const log = await screen.findByTestId("ibx-log");
    await waitFor(() => expect(log.querySelectorAll(".ibx-lrow")).toHaveLength(4));
    fireEvent.keyDown(log, { key: "j" });
    const first = log.querySelector(".ibx-lrow.sel")!.getAttribute("data-log");
    for (const mod of [{ metaKey: true }, { ctrlKey: true }, { altKey: true }]) {
      const ev = new KeyboardEvent("keydown", {
        key: "j",
        bubbles: true,
        cancelable: true,
        ...mod,
      });
      log.dispatchEvent(ev);
      expect(ev.defaultPrevented).toBe(false);
      const r = new KeyboardEvent("keydown", { key: "r", bubbles: true, cancelable: true, ...mod });
      log.dispatchEvent(r);
      expect(r.defaultPrevented).toBe(false);
    }
    expect(log.querySelector(".ibx-lrow.sel")).toHaveAttribute("data-log", first!);
    expect(log.querySelector("[data-logreply]")).toBeNull();
  });
});

describe("Mailspool reply line to an ended session", () => {
  const openQaLine = async (knownRole = false) => {
    const dto = fixtureSessionInbox(FX.ctrl);
    if (knownRole)
      dto.messages = dto.messages.map((m) => (m.from === FX.qa ? { ...m, replyTo: "role:qa" } : m));
    inbox(dto);
    useAgentInboxRailStore.setState({ lensByTab: { "tab-1": "spool" } });
    await renderRail(FX.ctrl);
    const log = await screen.findByTestId("ibx-log");
    const row = await waitFor(() => log.querySelector(`[data-log="alderco/default#1"]`)!);
    fireEvent.click(row);
    return log.querySelector<HTMLInputElement>("[data-logreply]")!;
  };

  it("answers on the role it held", async () => {
    api.loadInboxSessions.mockResolvedValue(
      fixtureSessions().map((s) => (s.handle === FX.qa ? { ...s, role: "qa" } : s)),
    );
    const reply = await openQaLine(true);
    await waitFor(() => expect(screen.getByText("› reply to #qa:")).toBeInTheDocument());
    fireEvent.change(reply, { target: { value: "again please" } });
    await act(async () => {
      fireEvent.keyDown(reply, { key: "Enter" });
    });
    expect(api.sendAsHuman).toHaveBeenCalledWith(expect.anything(), "#qa", "again please", 1);
  });

  it("says it ended and sends nothing when no role is known", async () => {
    const reply = await openQaLine();
    const note = await screen.findByTestId("ibx-ended");
    expect(note).toHaveTextContent("qa-DEMO-3 ended — messages to it won't be read");
    // The line says it can't send, and points at why.
    expect(reply).toHaveAttribute("aria-disabled", "true");
    expect(reply).toHaveAttribute("aria-describedby", note.id);
    fireEvent.change(reply, { target: { value: "hello?" } });
    await act(async () => {
      fireEvent.keyDown(reply, { key: "Enter" });
    });
    expect(api.sendAsHuman).not.toHaveBeenCalled();
  });
});

describe("Wire lens", () => {
  it("draws this tab's lifeline first, a dashed lane per role queue, and opens an arrow's detail", async () => {
    inbox(fixtureSessionInbox(FX.impl));
    await renderRail(FX.impl);
    fireEvent.click(screen.getByRole("button", { name: /Wire/ }));
    expect(useAgentInboxRailStore.getState().lensByTab["tab-1"]).toBe("wire");
    const wire = await screen.findByTestId("ibx-wire");
    expect(screen.getByText("(this tab)")).toBeInTheDocument();
    expect(document.querySelector(".ibx-whead.queue")).toHaveTextContent("reviewer");
    expect(wire.querySelectorAll(".ibx-life.queue")).toHaveLength(1);
    const toReviewer = wire.querySelector('[data-arrow="alderco/default#7"]')!;
    expect(toReviewer.className).toContain("q");
    expect(wire.querySelector('[data-arrow="alderco/default#6"]')!.className).toContain("human");
    fireEvent.click(toReviewer);
    const detail = await screen.findByTestId("ibx-wire-detail");
    expect(within(detail).getByRole("button", { name: /Reply/ })).toBeInTheDocument();
  });
});

describe("Wire lens composer", () => {
  it("keeps its draft when the selected arrow changes or closes (no per-thread scope here)", async () => {
    inbox(fixtureSessionInbox(FX.impl));
    await renderRail(FX.impl);
    fireEvent.click(screen.getByRole("button", { name: /Wire/ }));
    const wire = await screen.findByTestId("ibx-wire");
    fireEvent.change(screen.getByLabelText("Message"), { target: { value: "half a thought" } });
    fireEvent.click(wire.querySelector('[data-arrow="alderco/default#7"]')!);
    await screen.findByTestId("ibx-wire-detail");
    expect(screen.getByLabelText("Message")).toHaveValue("half a thought");
  });
});

describe("Header", () => {
  it("names the tab's session by its display name, with the raw handle only as secondary text", async () => {
    const worker = "_worker/acme/team-context/default/demo-child";
    inbox({ handle: worker, roles: [], messages: [] });
    await renderRail(worker);
    const scope = document.querySelector(".ibx-scope")!;
    await waitFor(() => expect(scope.querySelector(".t")).toHaveTextContent(/^demo-child$/));
    const h = scope.querySelector(".h")!;
    expect(h).toHaveAttribute("title", worker);
    expect(h.textContent).toContain("…");
    expect(h.textContent).toContain("demo-child");
  });
});

describe("review round 1", () => {
  const withImplThread = () => {
    const dto = fixtureSessionInbox(FX.rev);
    dto.messages.push({
      ...dto.messages[0]!,
      id: 50,
      from: FX.impl,
      fromName: "impl-DEMO-5",
      to: FX.rev,
      toRole: null,
      delivery: null,
    });
    return dto;
  };

  it("asks at the stable session id and uses the returned canonical handle", async () => {
    useAgentListProjectionStore.setState({
      sessions: [{ sessionId: "sid-remote", handle: FX.t9 } as unknown as AgentSessionRow],
    });
    inbox(fixtureSessionInbox(FX.t9));
    render(
      <InboxRail
        tabId="tab-1"
        sessionId="sid-remote"
        teamKey={TEAM_KEY}
        composeRequest={0}
        narrow={false}
      />,
    );
    await waitFor(() => expect(api.loadSessionInbox).toHaveBeenCalledWith("sid-remote"));
    expect(api.loadSessionInbox).not.toHaveBeenCalledWith(FX.t9);
  });

  it("answers a message once: the next send in the thread is not another reply to it", async () => {
    inbox(withImplThread());
    await renderRail(FX.rev);
    fireEvent.click(await waitFor(() => document.querySelector(`[data-thread="${FX.impl}"]`)!));
    fireEvent.click(await screen.findByRole("button", { name: "#implementer" }));
    const box = screen.getByLabelText("Message");
    fireEvent.change(box, { target: { value: "ack" } });
    fireEvent.keyDown(box, { key: "Enter", metaKey: true });
    await waitFor(() =>
      expect(api.sendAsHuman).toHaveBeenCalledWith(expect.anything(), "#implementer", "ack", 50),
    );
    await waitFor(() => expect(box).toHaveValue(""));
    fireEvent.change(box, { target: { value: "and one more" } });
    fireEvent.keyDown(box, { key: "Enter", metaKey: true });
    await waitFor(() =>
      expect(api.sendAsHuman).toHaveBeenLastCalledWith(
        expect.anything(),
        "#implementer",
        "and one more",
        null,
      ),
    );
  });

  it("sends once however fast ⌘↵ repeats, and keeps text typed while sending", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    await renderRail(FX.t9);
    await screen.findByTestId("ibx-msgs");
    let resolve: (v: { status: "sent"; messageId: number }) => void = () => {};
    api.sendAsHuman.mockImplementationOnce(
      () =>
        new Promise((r) => {
          resolve = r;
        }),
    );
    const box = screen.getByLabelText("Message");
    fireEvent.change(box, { target: { value: "once" } });
    fireEvent.keyDown(box, { key: "Enter", metaKey: true });
    fireEvent.keyDown(box, { key: "Enter", metaKey: true });
    await waitFor(() => expect(api.sendAsHuman).toHaveBeenCalledTimes(1));
    fireEvent.change(box, { target: { value: "once more" } });
    await act(async () => resolve({ status: "sent", messageId: 7 }));
    expect(api.sendAsHuman).toHaveBeenCalledTimes(1);
    expect(box).toHaveValue("once more");
  });

  it("keeps each conversation's draft to itself", async () => {
    inbox(withImplThread());
    await renderRail(FX.rev);
    const open = async (key: string) =>
      fireEvent.click(await waitFor(() => document.querySelector(`[data-thread="${key}"]`)!));
    await open(FX.impl);
    fireEvent.change(screen.getByLabelText("Message"), { target: { value: "for impl only" } });
    const other = [...document.querySelectorAll<HTMLElement>("[data-thread]")]
      .map((el) => el.dataset.thread!)
      .find((k) => k !== FX.impl)!;
    await open(other);
    expect(screen.getByLabelText("Message")).toHaveValue("");
    await open(FX.impl);
    expect(screen.getByLabelText("Message")).toHaveValue("for impl only");
  });

  it("honours a compose request on Mailspool by switching to Threads, whose composer takes focus", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    useAgentInboxRailStore.setState({ lensByTab: { "tab-1": "spool" } });
    render(
      <InboxRail
        tabId="tab-1"
        sessionId={FX.t9}
        teamKey={TEAM_KEY}
        composeRequest={1}
        narrow={false}
      />,
    );
    await waitFor(() =>
      expect(useAgentInboxRailStore.getState().lensByTab["tab-1"]).toBe("threads"),
    );
    await waitFor(() => expect(screen.getByLabelText("Message")).toHaveFocus());
  });

  it("honours a compose request made while the inbox is still loading", async () => {
    let land: (v: { dto: SessionInboxDto; source: "live" }) => void = () => {};
    api.loadSessionInbox.mockImplementation(
      () =>
        new Promise((r) => {
          land = r;
        }),
    );
    render(
      <InboxRail
        tabId="tab-1"
        sessionId={FX.t9}
        teamKey={TEAM_KEY}
        composeRequest={1}
        narrow={false}
      />,
    );
    await waitFor(() => expect(api.loadSessionInbox).toHaveBeenCalled());
    await act(async () => land({ dto: fixtureSessionInbox(FX.t9), source: "live" }));
    await waitFor(() => expect(screen.getByLabelText("Message")).toHaveFocus());
  });

  it("never acts again on a compose request it already handled when it remounts", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    useAgentInboxRailStore.setState({
      bySession: { [FX.t9]: { composeRequest: 2, composeHandled: 2 } },
      lensByTab: { "tab-1": "spool" },
    });
    render(
      <InboxRail
        tabId="tab-1"
        sessionId={FX.t9}
        teamKey={TEAM_KEY}
        composeRequest={2}
        narrow={false}
      />,
    );
    await screen.findByTestId("ibx-log");
    await new Promise((r) => setTimeout(r, 20));
    expect(useAgentInboxRailStore.getState().lensByTab["tab-1"]).toBe("spool");
  });

  it("records a handled request in the store, so a later remount leaves the lens alone", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    useAgentInboxRailStore.setState({ bySession: { [FX.t9]: { composeRequest: 1 } } });
    const props = { tabId: "tab-1", sessionId: FX.t9, teamKey: TEAM_KEY, composeRequest: 1 };
    const { unmount } = render(<InboxRail {...props} narrow={false} />);
    await waitFor(() => expect(screen.getByLabelText("Message")).toHaveFocus());
    expect(useAgentInboxRailStore.getState().bySession[FX.t9]?.composeHandled).toBe(1);
    unmount();
    act(() => useAgentInboxRailStore.getState().setLens("tab-1", "spool"));
    render(<InboxRail {...props} narrow={false} />);
    await screen.findByTestId("ibx-log");
    await new Promise((r) => setTimeout(r, 20));
    expect(useAgentInboxRailStore.getState().lensByTab["tab-1"]).toBe("spool");
  });

  it("lets a lens picked while the inbox loads win over a request made before it", async () => {
    let land: (v: { dto: SessionInboxDto; source: "live" }) => void = () => {};
    api.loadSessionInbox.mockImplementation(
      () =>
        new Promise((r) => {
          land = r;
        }),
    );
    render(
      <InboxRail
        tabId="tab-1"
        sessionId={FX.t9}
        teamKey={TEAM_KEY}
        composeRequest={1}
        narrow={false}
      />,
    );
    await waitFor(() => expect(api.loadSessionInbox).toHaveBeenCalled());
    fireEvent.click(screen.getByRole("button", { name: "Mailspool" }));
    await act(async () => land({ dto: fixtureSessionInbox(FX.t9), source: "live" }));
    await screen.findByTestId("ibx-log");
    await new Promise((r) => setTimeout(r, 20));
    expect(useAgentInboxRailStore.getState().lensByTab["tab-1"]).toBe("spool");
  });

  it("on a narrow rail with no thread open, opens one so the request has a composer to focus", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    render(
      <InboxRail tabId="tab-1" sessionId={FX.t9} teamKey={TEAM_KEY} composeRequest={1} narrow />,
    );
    await waitFor(() => expect(screen.getByLabelText("Message")).toHaveFocus());
  });

  it("on a narrow rail with no mail and no role, still has a composer for the request", async () => {
    inbox({ handle: "shell-new", roles: [], messages: [] });
    render(
      <InboxRail
        tabId="tab-1"
        sessionId="shell-new"
        teamKey={TEAM_KEY}
        composeRequest={1}
        narrow
      />,
    );
    await waitFor(() => expect(screen.getByLabelText("Message")).toHaveFocus());
    // "Message this agent" writes to this session, even with nothing exchanged yet.
    expect(screen.getByTestId("ibx-to")).toHaveTextContent("@shell-new");
  });
});

describe("review round 6", () => {
  /** Fire the inbox-changed event the rail listens for, as the backend would. */
  async function refresh() {
    const calls = vi.mocked(listen).mock.calls as unknown as [string, () => void][];
    await act(async () => {
      for (const [event, cb] of calls) if (event === AGENT_INBOX_CHANGED_EVENT) cb();
    });
  }
  const firstMail = (): SessionInboxDto => ({
    handle: "shell-new",
    roles: [],
    messages: [
      {
        ...fixtureSessionInbox(FX.t9).messages[0]!,
        id: 500,
        from: FX.t9,
        fromName: "Terminal 9",
        to: "shell-new",
        toRole: null,
        delivery: null,
      },
    ],
  });

  it("keeps a narrow rail's fresh composer, and its draft, when the first message arrives", async () => {
    inbox({ handle: "shell-new", roles: [], messages: [] });
    await renderRail("shell-new", "tab-1", true);
    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "half a thought" } });
    box.focus();
    inbox(firstMail());
    await refresh();
    await waitFor(() => expect(api.loadSessionInbox).toHaveBeenCalledTimes(2));
    await screen.findByRole("button", { name: "Back to the inbox" });
    expect(screen.getByLabelText("Message")).toBe(box);
    expect(box).toHaveValue("half a thought");
    expect(box).toHaveFocus();
    // Back leaves for the list, where the new conversation waits.
    fireEvent.click(screen.getByRole("button", { name: "Back to the inbox" }));
    const list = await screen.findByRole("list", { name: "Conversations" });
    expect(list.querySelector(`[data-thread="${FX.t9}"]`)).not.toBeNull();
  });

  it("opens the conversation a narrow rail's first message started, not the list", async () => {
    inbox({ handle: "shell-new", roles: [], messages: [] });
    await renderRail("shell-new", "tab-1", true);
    await screen.findByLabelText("Message");
    fireEvent.click(screen.getByTestId("ibx-to"));
    fireEvent.click(await screen.findByRole("option", { name: /@Terminal 9/ }));
    fireEvent.change(screen.getByLabelText("Message"), { target: { value: "hello" } });
    fireEvent.keyDown(screen.getByLabelText("Message"), { key: "Enter", metaKey: true });
    await waitFor(() =>
      expect(api.sendAsHuman).toHaveBeenCalledWith(expect.anything(), FX.t9, "hello", null),
    );
    expect(await screen.findByTestId("ibx-msgs")).toBeInTheDocument();
    expect(screen.queryByRole("list", { name: "Conversations" })).toBeNull();
    expect(screen.getByTestId("ibx-to")).toHaveTextContent("@Terminal 9");
  });

  it("aims a compose request at the rail's own session, not its latest correspondent", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    render(
      <InboxRail
        tabId="tab-1"
        sessionId={FX.t9}
        teamKey={TEAM_KEY}
        composeRequest={1}
        narrow={false}
      />,
    );
    await waitFor(() => expect(screen.getByLabelText("Message")).toHaveFocus());
    expect(screen.getByTestId("ibx-to")).toHaveTextContent("@Terminal 9");
    // The picker offers the rail's own session too.
    fireEvent.click(screen.getByTestId("ibx-to"));
    expect(await screen.findByRole("option", { name: /@Terminal 9/ })).toBeInTheDocument();
  });

  it("says why g cannot go to an ended session's line instead of a misleading jump", async () => {
    inbox(fixtureSessionInbox(FX.ctrl));
    useAgentInboxRailStore.setState({ lensByTab: { "tab-1": "spool" } });
    await renderRail(FX.ctrl);
    const log = await screen.findByTestId("ibx-log");
    fireEvent.click(await waitFor(() => log.querySelector(`[data-log="alderco/default#1"]`)!));
    fireEvent.keyDown(log, { key: "g" });
    expect(toast).toHaveBeenCalledWith("Session exited, cannot open");
    expect(locate).not.toHaveBeenCalled();
  });

  it("writes to the session itself from its conversation with you", async () => {
    const dto = fixtureSessionInbox(FX.t9);
    dto.messages.push({
      ...dto.messages[0]!,
      id: 501,
      from: "human",
      fromName: "you",
      to: FX.t9,
      toRole: null,
      delivery: null,
    });
    inbox(dto);
    await renderRail(FX.t9);
    fireEvent.click(await waitFor(() => document.querySelector('[data-thread="human"]')!));
    await waitFor(() => expect(screen.getByTestId("ibx-to")).toHaveTextContent("@Terminal 9"));
  });
});

describe("review pr-inbox round 1", () => {
  it("aims Reply on a tab's conversation with you at the session, not at yourself", async () => {
    inbox(fixtureSessionInbox(FX.impl));
    await renderRail(FX.impl);
    const row = await waitFor(() => document.querySelector<HTMLElement>('[data-thread="human"]')!);
    fireEvent.click(within(row).getByLabelText(/^Reply to /));
    await waitFor(() => expect(screen.getByTestId("ibx-to")).toHaveTextContent("@impl-DEMO-5"));
    expect(screen.getByTestId("ibx-route-hint")).not.toHaveTextContent("Goes to your own inbox");
  });

  it("aims the Wire composer at the session when the selected arrow is between it and you", async () => {
    inbox(fixtureSessionInbox(FX.impl));
    useAgentInboxRailStore.setState({ lensByTab: { "tab-1": "wire" } });
    await renderRail(FX.impl);
    const arrow = await waitFor(
      () => document.querySelector<HTMLElement>('[data-arrow="alderco/default#6"]')!,
    );
    fireEvent.click(arrow);
    await waitFor(() => expect(screen.getByTestId("ibx-to")).toHaveTextContent("@impl-DEMO-5"));
  });

  it("keeps an unsent draft when the inbox is closed and opened again", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    const first = await renderRail(FX.t9);
    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "draft that must survive" } });
    first.unmount();
    await renderRail(FX.t9);
    expect(await screen.findByLabelText("Message")).toHaveValue("draft that must survive");
    // Sent, it is gone for good.
    fireEvent.keyDown(screen.getByLabelText("Message"), { key: "Enter", metaKey: true });
    await waitFor(() => expect(api.sendAsHuman).toHaveBeenCalled());
    await waitFor(() => expect(screen.getByLabelText("Message")).toHaveValue(""));
  });

  it("keeps a draft when a refused send comes back, with a toast saying why", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    api.sendAsHuman.mockResolvedValueOnce({ status: "invalid", message: "nobody home" });
    await renderRail(FX.t9);
    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "please keep me" } });
    fireEvent.keyDown(box, { key: "Enter", metaKey: true });
    await waitFor(() => expect(toast).toHaveBeenCalledWith("Could not send: nobody home"));
    expect(box).toHaveValue("please keep me");
  });

  it("puts focus back on the list row when a narrow rail steps back from a thread", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    await renderRail(FX.t9, "tab-1", true);
    const list = await screen.findByRole("list", { name: "Conversations" });
    const row = await waitFor(() => list.querySelector<HTMLElement>("[data-thread]")!);
    const key = row.dataset.thread;
    fireEvent.click(row);
    const back = await screen.findByRole("button", { name: "Back to the inbox" });
    back.focus();
    fireEvent.click(back);
    const again = await screen.findByRole("list", { name: "Conversations" });
    await waitFor(() =>
      expect(document.activeElement).toBe(again.querySelector(`[data-thread="${key}"]`)),
    );
  });
});

describe("review pr-inbox round 2", () => {
  const withImplMessage = () => {
    const dto = fixtureSessionInbox(FX.rev);
    dto.messages.push({
      ...dto.messages[0]!,
      id: 50,
      from: FX.impl,
      fromName: "impl-DEMO-5",
      to: FX.rev,
      toRole: null,
      delivery: null,
    });
    return dto;
  };

  it("brings a kept draft back still aimed at the recipient it was written for", async () => {
    inbox(withImplMessage());
    const first = await renderRail(FX.rev);
    fireEvent.click(await waitFor(() => document.querySelector(`[data-thread="${FX.impl}"]`)!));
    fireEvent.click(await screen.findByRole("button", { name: "#implementer" }));
    fireEvent.change(screen.getByLabelText("Message"), { target: { value: "for the queue" } });
    first.unmount();
    await renderRail(FX.rev);
    fireEvent.click(await waitFor(() => document.querySelector(`[data-thread="${FX.impl}"]`)!));
    expect(await screen.findByLabelText("Message")).toHaveValue("for the queue");
    expect(screen.getByTestId("ibx-to")).toHaveTextContent("implementer");
    fireEvent.keyDown(screen.getByLabelText("Message"), { key: "Enter", metaKey: true });
    // Still the reply it was: to the queue, answering the message it sat under.
    await waitFor(() =>
      expect(api.sendAsHuman).toHaveBeenCalledWith(
        expect.anything(),
        "#implementer",
        "for the queue",
        50,
      ),
    );
  });

  it("never brings back a draft whose send finished after the inbox closed", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    let finish!: (v: unknown) => void;
    api.sendAsHuman.mockReturnValueOnce(new Promise((r) => (finish = r)));
    const first = await renderRail(FX.t9);
    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "sent while closing" } });
    fireEvent.keyDown(box, { key: "Enter", metaKey: true });
    await waitFor(() => expect(api.sendAsHuman).toHaveBeenCalled());
    first.unmount();
    await act(async () => finish({ status: "sent", messageId: 7 }));
    await renderRail(FX.t9);
    expect(await screen.findByLabelText("Message")).toHaveValue("");
  });

  it("keeps the Wire composer's draft when the inbox closes", async () => {
    inbox(fixtureSessionInbox(FX.impl));
    useAgentInboxRailStore.setState({ lensByTab: { "tab-1": "wire" } });
    const first = await renderRail(FX.impl);
    fireEvent.change(await screen.findByLabelText("Message"), { target: { value: "wired" } });
    first.unmount();
    await renderRail(FX.impl);
    expect(await screen.findByLabelText("Message")).toHaveValue("wired");
  });

  it("answers a legacy role:human Reply-To as the human, not a #human queue", async () => {
    const dto = fixtureSessionInbox(FX.rev);
    dto.messages.push({
      ...dto.messages[0]!,
      id: 52,
      from: FX.impl,
      fromName: "impl-DEMO-5",
      to: FX.rev,
      toRole: null,
      via: null,
      delivery: null,
      replyTo: "role:human",
    });
    inbox(dto);
    await renderRail(FX.rev);
    fireEvent.click(await waitFor(() => document.querySelector(`[data-thread="${FX.impl}"]`)!));
    await waitFor(() => expect(screen.getByTestId("ibx-to")).toHaveTextContent("@impl-DEMO-5"));
    expect(screen.getByTestId("ibx-to")).not.toHaveTextContent("human");
  });
});

describe("review pr-inbox round 3", () => {
  it("never offers again a draft whose send is still on its way when the inbox reopens", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    let finish!: (v: unknown) => void;
    api.sendAsHuman.mockReturnValueOnce(new Promise((r) => (finish = r)));
    const first = await renderRail(FX.t9);
    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "on its way" } });
    fireEvent.keyDown(box, { key: "Enter", metaKey: true });
    await waitFor(() => expect(api.sendAsHuman).toHaveBeenCalled());
    first.unmount();
    await renderRail(FX.t9);
    expect(await screen.findByLabelText("Message")).toHaveValue("");
    await act(async () => finish({ status: "sent", messageId: 8 }));
  });

  it("keeps the draft when a send that was on its way while the inbox closed fails", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    let finish!: (v: unknown) => void;
    api.sendAsHuman.mockReturnValueOnce(new Promise((r) => (finish = r)));
    const first = await renderRail(FX.t9);
    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "try again later" } });
    fireEvent.keyDown(box, { key: "Enter", metaKey: true });
    await waitFor(() => expect(api.sendAsHuman).toHaveBeenCalled());
    first.unmount();
    await act(async () => finish({ status: "invalid", message: "nope" }));
    await renderRail(FX.t9);
    expect(await screen.findByLabelText("Message")).toHaveValue("try again later");
  });

  it("starts a new session in the same tab without the previous session's drafts", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    const first = await renderRail(FX.t9, "tab-1");
    fireEvent.change(await screen.findByLabelText("Message"), { target: { value: "for t9" } });
    first.unmount();
    inbox(fixtureSessionInbox(FX.impl));
    await renderRail(FX.impl, "tab-1");
    expect(await screen.findByLabelText("Message")).toHaveValue("");
  });
});

describe("review pr-inbox round 4", () => {
  it("hands a send that fails after the inbox reopened back to the reopened box", async () => {
    inbox(fixtureSessionInbox(FX.t9));
    let finish!: (v: unknown) => void;
    api.sendAsHuman.mockReturnValueOnce(new Promise((r) => (finish = r)));
    const first = await renderRail(FX.t9);
    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "comes back" } });
    fireEvent.keyDown(box, { key: "Enter", metaKey: true });
    await waitFor(() => expect(api.sendAsHuman).toHaveBeenCalled());
    first.unmount();
    await renderRail(FX.t9);
    const reopened = await screen.findByLabelText("Message");
    expect(reopened).toHaveValue("");
    await act(async () => finish({ status: "invalid", message: "nope" }));
    await waitFor(() => expect(reopened).toHaveValue("comes back"));
  });

  it("never re-keeps a text on its way when the thread changes during its send", async () => {
    inbox(fixtureSessionInbox(FX.ctrl));
    let finish!: (v: unknown) => void;
    api.sendAsHuman.mockReturnValueOnce(new Promise((r) => (finish = r)));
    const first = await renderRail(FX.ctrl);
    const rows = await waitFor(() => {
      const r = document.querySelectorAll<HTMLElement>("[data-thread]");
      expect(r.length).toBeGreaterThan(1);
      return r;
    });
    const key = rows[0]!.dataset.thread!;
    fireEvent.click(rows[0]!);
    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "in flight" } });
    fireEvent.keyDown(box, { key: "Enter", metaKey: true });
    await waitFor(() => expect(api.sendAsHuman).toHaveBeenCalled());
    // Another thread, then close and reopen on the first, all before the send lands.
    fireEvent.click(rows[1]!);
    first.unmount();
    await renderRail(FX.ctrl);
    fireEvent.click(await waitFor(() => document.querySelector(`[data-thread="${key}"]`)!));
    expect(await screen.findByLabelText("Message")).toHaveValue("");
    await act(async () => finish({ status: "sent", messageId: 9 }));
  });
});

describe("review pr-inbox round 4 (read state and recipients)", () => {
  it("reads mail received by a hidden warm tab only when that tab is revealed", async () => {
    const markRead = vi
      .spyOn(await import("@/tauri/commands"), "markInboxMessageRead")
      .mockResolvedValue(undefined as never);
    const rects = vi
      .spyOn(HTMLElement.prototype, "getBoundingClientRect")
      .mockImplementation(function (this: HTMLElement) {
        return { width: this.closest("[inert]") ? 0 : 600, height: 600 } as DOMRect;
      });
    const tab = { id: "tab-1", sessionId: FX.impl };
    const surface = (visible: boolean) => (
      <div inert={!visible} style={visible ? undefined : { display: "none" }}>
        <InboxSplit tab={tab} teamKey={TEAM_KEY} visible={visible}>
          <div data-testid="warm-terminal" />
        </InboxSplit>
      </div>
    );
    try {
      const dto = fixtureSessionInbox(FX.impl);
      const original = dto.messages.find((m) => m.id === 6)!;
      inbox({ ...dto, messages: dto.messages.map((m) => ({ ...m, unread: false })) });
      // The inbox opens only beside a running agent.
      useAgentStateStore.setState({ processAgentBySession: { [FX.impl]: "codex" } });
      act(() => openAgentInboxRail(FX.impl));
      const view = render(surface(true));
      await screen.findByTestId("inbox-rail");
      await waitFor(() => expect(document.querySelector('[data-thread="human"]')).not.toBeNull());
      fireEvent.click(await waitFor(() => document.querySelector('[data-thread="human"]')!));
      const terminal = screen.getByTestId("warm-terminal");
      view.rerender(surface(false));
      inbox({ ...dto, messages: [{ ...original, id: 707, unread: true }] });
      await act(async () => {
        for (const [event, cb] of vi.mocked(listen).mock.calls as unknown as [
          string,
          () => void,
        ][]) {
          if (event === AGENT_INBOX_CHANGED_EVENT) cb();
        }
      });
      expect(markRead).not.toHaveBeenCalled();
      view.rerender(surface(true));
      await waitFor(() => expect(markRead).toHaveBeenCalledWith(expect.anything(), 707));
      expect(screen.getByTestId("warm-terminal")).toBe(terminal);
    } finally {
      useAgentStateStore.setState({ processAgentBySession: {} });
      rects.mockRestore();
      markRead.mockRestore();
    }
  });
  it("reads what the session sent you when its conversation with you is open on screen", async () => {
    const markRead = vi.spyOn(await import("@/tauri/commands"), "markInboxMessageRead");
    markRead.mockResolvedValue(undefined as never);
    const rects = vi
      .spyOn(HTMLElement.prototype, "getBoundingClientRect")
      .mockReturnValue({ width: 400, height: 600 } as DOMRect);
    try {
      inbox(fixtureSessionInbox(FX.impl));
      await renderRail(FX.impl);
      fireEvent.click(await waitFor(() => document.querySelector('[data-thread="human"]')!));
      await waitFor(() => expect(markRead).toHaveBeenCalledWith(expect.anything(), 6));
    } finally {
      rects.mockRestore();
      markRead.mockRestore();
    }
  });

  it("defaults a held queue's composer to the session when the human queued its last message", async () => {
    const dto = fixtureSessionInbox(FX.rev);
    dto.messages = dto.messages.map((m) => (m.toRole === "reviewer" ? { ...m, from: "human" } : m));
    inbox(dto);
    await renderRail(FX.rev);
    fireEvent.click(await waitFor(() => document.querySelector('[data-thread="#reviewer"]')!));
    await waitFor(() => expect(screen.getByTestId("ibx-to")).not.toHaveTextContent("pick"));
    expect(screen.getByTestId("ibx-route-hint")).not.toHaveTextContent("Goes to your own inbox");
  });
});

describe("canonical session identity from the production loader", () => {
  it("refreshes an early raw identity when launch registration finishes", async () => {
    const realApi = await vi.importActual<typeof import("../api")>("../api");
    const canonical = fixtureSessionInbox(FX.impl);
    let registered = false;
    const invoke = vi.spyOn(await import("@tauri-apps/api/core"), "invoke");
    invoke.mockImplementation(
      async <T,>(command: string) =>
        (command === "list_session_inbox"
          ? registered
            ? canonical
            : { handle: "raw-restoring", roles: [], messages: [] }
          : undefined) as T,
    );
    api.loadSessionInbox.mockImplementation(realApi.loadSessionInbox);
    try {
      await renderRail("raw-restoring");
      expect(document.querySelector('[data-thread="human"]')).toBeNull();
      registered = true;
      await act(async () => {
        for (const [event, callback] of vi.mocked(listen).mock.calls) {
          if (event === SESSION_STATE_CHANGED_EVENT) callback({ payload: null } as never);
        }
      });
      await waitFor(() => expect(document.querySelector('[data-thread="human"]')).not.toBeNull());
      expect(api.loadSessionInbox).toHaveBeenCalledTimes(2);
      expect(api.loadSessionInbox).toHaveBeenLastCalledWith("raw-restoring");
    } finally {
      invoke.mockRestore();
      api.loadSessionInbox.mockReset();
    }
  });
  it("renders and replies at the DTO handle before the roster arrives, keeping selection and draft afterward", async () => {
    const realApi = await vi.importActual<typeof import("../api")>("../api");
    const dto = fixtureSessionInbox(FX.impl);
    const invoke = vi.spyOn(await import("@tauri-apps/api/core"), "invoke");
    invoke.mockImplementation(async <T,>(command: string) => {
      return (command === "list_session_inbox" ? dto : undefined) as T;
    });
    api.loadSessionInbox.mockImplementation(realApi.loadSessionInbox);
    const rects = vi
      .spyOn(HTMLElement.prototype, "getBoundingClientRect")
      .mockReturnValue({ width: 400, height: 600 } as DOMRect);
    try {
      await renderRail("raw-impl");
      const humanThread = await waitFor(() => {
        const row = document.querySelector('[data-thread="human"]');
        expect(row).not.toBeNull();
        return row!;
      });
      fireEvent.click(humanThread);
      await waitFor(() =>
        expect(
          screen.getByTestId("ibx-msgs").querySelectorAll(".ibx-m.out").length,
        ).toBeGreaterThan(0),
      );
      expect(invoke).toHaveBeenCalledWith("list_session_inbox", { handle: "raw-impl" });
      await waitFor(() =>
        expect(invoke).toHaveBeenCalledWith(
          "mark_inbox_message_read",
          expect.objectContaining({ messageId: 6 }),
        ),
      );
      const box = screen.getByLabelText("Message");
      fireEvent.change(box, { target: { value: "before roster" } });
      fireEvent.keyDown(box, { key: "Enter", metaKey: true });
      await waitFor(() =>
        expect(api.sendAsHuman).toHaveBeenCalledWith(
          expect.anything(),
          FX.impl,
          "before roster",
          null,
        ),
      );
      await waitFor(() => expect(box).toHaveValue(""));
      fireEvent.change(box, { target: { value: "keep this draft" } });
      act(() =>
        useAgentListProjectionStore.setState({
          sessions: [
            {
              sessionId: "raw-impl",
              handle: FX.impl,
              displayName: "Implementer",
            } as AgentSessionRow,
          ],
        }),
      );
      expect(screen.getByLabelText("Message")).toBe(box);
      expect(box).toHaveValue("keep this draft");
      expect(document.querySelector('[data-thread="human"]')).toHaveAttribute(
        "aria-current",
        "true",
      );
      expect(api.loadSessionInbox).not.toHaveBeenCalledWith(FX.impl);
    } finally {
      invoke.mockRestore();
      rects.mockRestore();
      api.loadSessionInbox.mockReset();
    }
  });
});

describe("session history arrival through the production read path", () => {
  it("keeps two instances separate, loads retained history on reopen and updates an empty raw-id inbox on arrival", async () => {
    const realApi = await vi.importActual<typeof import("../api")>("../api");
    const message = fixtureSessionInbox(FX.impl).messages[0]!;
    const first = {
      ...message,
      id: 200,
      from: "human",
      fromName: "You",
      to: FX.impl,
      toRole: null,
      text: "first instance only",
      unread: true,
    };
    const second = { ...first, id: 201, to: FX.rev, text: "second instance only" };
    let firstDto: SessionInboxDto = { handle: FX.impl, roles: [], messages: [] };
    const secondDto: SessionInboxDto = { handle: FX.rev, roles: [], messages: [second] };
    const invoke = vi.spyOn(await import("@tauri-apps/api/core"), "invoke");
    invoke.mockImplementation(async <T,>(command: string, args?: unknown) => {
      return (
        command === "list_session_inbox"
          ? (args as { handle: string }).handle === "raw-first"
            ? firstDto
            : secondDto
          : undefined
      ) as T;
    });
    api.loadSessionInbox.mockImplementation(realApi.loadSessionInbox);
    useAgentListProjectionStore.setState({
      sessions: [
        { sessionId: "raw-first", handle: FX.impl, agentRevision: "acme/default/same@1" },
        { sessionId: "raw-second", handle: FX.rev, agentRevision: "acme/default/same@1" },
      ] as AgentSessionRow[],
    });
    try {
      const firstRail = await renderRail("raw-first", "first-tab");
      await renderRail("raw-second", "second-tab");
      await screen.findAllByText("second instance only");
      expect(
        within(screen.getAllByTestId("inbox-rail")[0]).queryByText("second instance only"),
      ).toBeNull();
      firstDto = { ...firstDto, messages: [first] };
      await act(async () => {
        for (const [event, callback] of vi.mocked(listen).mock.calls) {
          if (event === AGENT_INBOX_CHANGED_EVENT) callback({ payload: null } as never);
        }
      });
      await screen.findAllByText("first instance only");
      expect(
        within(screen.getAllByTestId("inbox-rail")[1]).queryByText("first instance only"),
      ).toBeNull();
      expect(invoke).not.toHaveBeenCalledWith("mark_inbox_message_read", expect.anything());
      firstRail.unmount();
      await renderRail("raw-first", "first-tab");
      await screen.findAllByText("first instance only");
      expect(api.loadSessionInbox).toHaveBeenCalledWith("raw-first");
      expect(api.loadSessionInbox).toHaveBeenCalledWith("raw-second");
    } finally {
      invoke.mockRestore();
      api.loadSessionInbox.mockReset();
    }
  });
});

vi.mock("@/shared/hooks/use-avatar-packs", async () => {
  const { BUILTIN_AVATAR_PACKS, BUILTIN_FACE_SOURCE } = await import("@/shared/lib/avatar-packs");
  return {
    useAvatarPacks: () => ({ packs: BUILTIN_AVATAR_PACKS, faces: BUILTIN_FACE_SOURCE }),
    useTeamAvatarPacks: () => () => ({ packs: BUILTIN_AVATAR_PACKS, faces: BUILTIN_FACE_SOURCE }),
  };
});

vi.mock("@/shared/hooks/use-role-queues", async () => {
  const { fixtureRoles } = await import("../fixtures");
  const queues = fixtureRoles().map((r) => ({
    role: r.role,
    roleName: null,
    counts: { pending: r.pending, claimed: 0, accepted: 0, acknowledged: 0 },
    registrations: r.holder ? [{ instanceId: r.holder, live: true, generation: 1 }] : [],
    deliveries: [],
    deliveriesTruncated: false,
  }));
  return { useRoleQueues: () => ({ queues, error: null }) };
});

describe("workspace group chat", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockImplementation(async (command, args) =>
      command === "workspace_chat_read"
        ? {
            workspaceId: (args as Record<string, unknown> | undefined)?.workspaceId,
            messages: [],
            headId: 0,
            humanReadId: 0,
            archived: false,
          }
        : undefined,
    );
  });
  it("pins the bound room before any post and opens a usable empty composer", async () => {
    vi.mocked(invoke).mockImplementation(async (command, args) =>
      command === "workspace_chat_read"
        ? {
            workspaceId: (args as Record<string, unknown> | undefined)?.workspaceId,
            messages: [],
            headId: 0,
            humanReadId: 0,
            archived: false,
          }
        : undefined,
    );
    inbox({ handle: "room-agent", roles: [], messages: [] });
    useAgentListProjectionStore.setState({
      sessions: [
        {
          handle: "room-agent",
          sessionId: "room-session",
          alive: true,
          chatWorkspaceId: "story:SV-X",
          groupUnread: 0,
        } as AgentSessionRow,
      ],
    });
    await renderRail("room-session");
    const chip = await screen.findByRole("button", { name: /Open the SV-X group chat/ });
    expect(screen.getByText("Group chat")).toBeInTheDocument();
    fireEvent.click(chip);
    expect(await screen.findByTestId("workspace-chat-body")).toHaveTextContent("New chat");
    const compose = screen.getByPlaceholderText("Message everyone in SV-X…");
    fireEvent.change(compose, { target: { value: "draft for this room" } });
    fireEvent.click(chip);
    expect(screen.queryByPlaceholderText("Message everyone in SV-X…")).toBeNull();
    fireEvent.click(chip);
    expect(await screen.findByPlaceholderText("Message everyone in SV-X…")).toHaveValue(
      "draft for this room",
    );
    fireEvent.click(screen.getByTestId("workspace-chat-send"));
    await waitFor(() =>
      expect(screen.getByPlaceholderText("Message everyone in SV-X…")).toHaveValue(""),
    );
    fireEvent.click(chip);
    fireEvent.click(chip);
    expect(await screen.findByPlaceholderText("Message everyone in SV-X…")).toHaveValue("");
  });
  it("opens from any lens, focuses on a keyboard request, toggles back, and follows the session binding", async () => {
    inbox({ handle: "room-agent", roles: [], messages: [] });
    useAgentStateStore.getState().setProcessAgent("room-session", "codex");
    useAgentListProjectionStore.setState({
      sessions: [
        {
          handle: "room-agent",
          sessionId: "room-session",
          alive: true,
          chatWorkspaceId: "story:SV-X",
          groupUnread: 3,
        } as AgentSessionRow,
      ],
    });
    const utils = await renderRail("room-session");
    fireEvent.click(screen.getByRole("button", { name: "Wire" }));
    fireEvent.click(screen.getByRole("button", { name: /Open the SV-X group chat/ }), {
      detail: 1,
    });
    const compose = await screen.findByPlaceholderText("Message everyone in SV-X…");
    expect(document.activeElement).not.toBe(compose);
    expect(screen.getByRole("button", { name: /Open the SV-X group chat/ })).toHaveTextContent("3");
    fireEvent.click(screen.getByRole("button", { name: /Open the SV-X group chat/ }));
    expect(screen.getByRole("button", { name: "Wire" })).toHaveAttribute("aria-pressed", "true");
    act(() => useAgentInboxRailStore.getState().openGroup("room-session"));
    await waitFor(() =>
      expect(screen.getByPlaceholderText("Message everyone in SV-X…")).toHaveFocus(),
    );
    act(() => useAgentInboxRailStore.getState().openGroup("room-session"));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Wire" })).toHaveAttribute("aria-pressed", "true"),
    );
    act(() =>
      useAgentListProjectionStore.setState({
        sessions: [
          {
            handle: "other-agent",
            sessionId: "other-session",
            alive: true,
            chatWorkspaceId: "story:SV-Y",
            groupUnread: 0,
          } as AgentSessionRow,
        ],
      }),
    );
    inbox({ handle: "other-agent", roles: [], messages: [] });
    utils.rerender(
      <InboxRail
        tabId="tab-1"
        sessionId="other-session"
        teamKey={TEAM_KEY}
        composeRequest={0}
        narrow={false}
      />,
    );
    expect(
      await screen.findByRole("button", { name: /Open the SV-Y group chat/ }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Open the SV-X group chat/ })).toBeNull();
  });
});

it("combines direct and group unread in one tab count while retaining the split", async () => {
  const rows = fixtureSessions().map((s) =>
    s.handle === FX.t9 ? { ...s, unread: 2, groupUnread: 3 } : s,
  );
  api.loadInboxSessions.mockResolvedValue(rows);
  function Badge() {
    const counts = useTabInboxCounts(FX.t9);
    const total = useTabInboxUnread(FX.t9);
    return <span title={`${counts.direct} messages · ${counts.group} in group chat`}>{total}</span>;
  }
  render(<Badge />);
  expect(await screen.findByTitle("2 messages · 3 in group chat")).toHaveTextContent("5");
});

it.each(["chip", "keyboard"])(
  "returns to the direct conversation via %s after reselecting the pinned group row",
  async (toggle) => {
    vi.mocked(invoke).mockImplementation(async (command) =>
      command === "workspace_chat_read"
        ? { messages: [], headId: 0, humanReadId: 0, archived: false }
        : undefined,
    );
    useAgentStateStore.getState().setProcessAgent(FX.t9, "codex");
    inbox(fixtureSessionInbox(FX.t9));
    useAgentListProjectionStore.setState({
      sessions: [
        {
          handle: FX.t9,
          sessionId: FX.t9,
          alive: true,
          chatWorkspaceId: "story:SV-X",
          groupUnread: 0,
        } as AgentSessionRow,
      ],
    });
    await renderRail(FX.t9);
    fireEvent.click(
      await waitFor(() => {
        const row = [...document.querySelectorAll<HTMLElement>("[data-thread]")].find(
          (candidate) =>
            !candidate.dataset.thread?.startsWith("group:") &&
            !candidate.dataset.thread?.startsWith("#"),
        );
        expect(row).toBeDefined();
        return row!;
      }),
    );
    const directRecipient = screen.getByTestId("ibx-to").textContent;
    const chip = await screen.findByRole("button", { name: /Open the SV-X group chat/ });
    fireEvent.click(chip);
    await screen.findByPlaceholderText("Message everyone in SV-X…");
    fireEvent.click(document.querySelector('[data-thread="group:story:SV-X"]')!);
    if (toggle === "chip") fireEvent.click(chip);
    else act(() => useAgentInboxRailStore.getState().openGroup(FX.t9));
    await waitFor(() => expect(screen.getByTestId("ibx-to")).toHaveTextContent(directRecipient!));
    expect(screen.queryByPlaceholderText("Message everyone in SV-X…")).toBeNull();
  },
);
