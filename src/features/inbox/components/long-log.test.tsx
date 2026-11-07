// SPDX-License-Identifier: MIT
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import { act, fireEvent, render, screen, waitFor, within } from "@/test-utils";
import { listen } from "@tauri-apps/api/event";
import { AGENT_INBOX_CHANGED_EVENT } from "@/tauri/events";
import { useAgentInboxRailStore } from "@/features/viewer/stores/agent-inbox-rail-store";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";

import type { ConversationMessage, SessionInboxDto } from "../api";
import { FX, fixtureSessionInbox, fixtureSessions } from "../fixtures";
import { useInboxSessionsStore } from "../hooks/use-inbox-sessions";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock("@/features/weft/lib/locate-agent", () => ({
  locateAgentByAgentId: () => ({ located: true }),
}));
vi.mock("sonner", () => ({ toast: vi.fn() }));

const api = vi.hoisted(() => ({
  loadSessionInbox: vi.fn(),
  loadInboxSessions: vi.fn(),
  sendAsHuman: vi.fn(),
}));
vi.mock("../api", async (orig) => ({ ...(await orig<typeof import("../api")>()), ...api }));

import { InboxRail } from "./inbox-rail";
import { clearInboxDrafts } from "./inbox-view-context";

// Long logs: the Mailspool opens at its newest line, follows new mail while you are at the
// bottom, draws only the lines in view, and offers "Jump to latest" when mail lands below a
// reader who scrolled up. The Threads conversation does the same.

/**
 * happy-dom lays nothing out, so give the two scrollers (the Mailspool log and a Threads
 * conversation) a 100px viewport, a content height, and a scrollTop that clamps and fires
 * `scroll` the way a browser does.
 */
const VIEW = 100;
const top = new WeakMap<Element, number>();
const isScroller = (el: Element) =>
  el.classList?.contains("ibx-log") || el.classList?.contains("ibx-msgs");
function contentHeight(el: Element): number {
  if (el.classList.contains("ibx-log")) {
    const sizer = el.querySelector<HTMLElement>("[data-spool-sizer]");
    return Math.max(VIEW, parseFloat(sizer?.style.height ?? "") || 0);
  }
  return Math.max(VIEW, el.children.length * 40);
}
const maxTop = (el: Element) => contentHeight(el) - VIEW;
function setTop(el: Element, v: number) {
  top.set(el, Math.max(0, Math.min(v, maxTop(el))));
  el.dispatchEvent(new Event("scroll"));
}
/** Heights for Mailspool slots in a test that wants rows of different sizes (else 100px). */
let slotHeight: ((slot: HTMLElement) => number) | null = null;
const saved: [string, PropertyDescriptor | undefined][] = [];
beforeAll(() => {
  const proto = HTMLElement.prototype as unknown as Record<string, unknown>;
  const orig = (k: string) =>
    Object.getOwnPropertyDescriptor(HTMLElement.prototype, k) ??
    Object.getOwnPropertyDescriptor(Element.prototype, k);
  const define = (k: string, d: PropertyDescriptor) => {
    saved.push([k, Object.getOwnPropertyDescriptor(HTMLElement.prototype, k)]);
    Object.defineProperty(proto, k, { configurable: true, ...d });
  };
  const st = orig("scrollTop");
  define("scrollTop", {
    get(this: HTMLElement) {
      return isScroller(this) ? (top.get(this) ?? 0) : st?.get?.call(this);
    },
    set(this: HTMLElement, v: number) {
      if (isScroller(this)) setTop(this, v);
      else st?.set?.call(this, v);
    },
  });
  const sh = orig("scrollHeight");
  define("scrollHeight", {
    get(this: HTMLElement) {
      return isScroller(this) ? contentHeight(this) : sh?.get?.call(this);
    },
  });
  const ch = orig("clientHeight");
  define("clientHeight", {
    get(this: HTMLElement) {
      return isScroller(this) ? VIEW : ch?.get?.call(this);
    },
  });
  // A conversation's children sit 40px apart; its own top is 0.
  const rect = orig("getBoundingClientRect") ?? {
    value: Element.prototype.getBoundingClientRect,
  };
  define("getBoundingClientRect", {
    value(this: HTMLElement) {
      const parent = this.parentElement;
      if (this.classList.contains("ibx-msgs")) return new DOMRect(0, 0, 300, VIEW);
      if (parent?.classList.contains("ibx-msgs")) {
        const i = Array.prototype.indexOf.call(parent.children, this);
        return new DOMRect(0, i * 40 - parent.scrollTop, 300, 40);
      }
      return (rect.value as () => DOMRect).call(this);
    },
    writable: true,
  });
  const oh = orig("offsetHeight");
  define("offsetHeight", {
    get(this: HTMLElement) {
      if (slotHeight && this.classList.contains("ibx-lslot")) return slotHeight(this);
      return oh?.get?.call(this);
    },
  });
  const scrollTo = proto.scrollTo as ((...a: unknown[]) => void) | undefined;
  define("scrollTo", {
    value(this: HTMLElement, a: ScrollToOptions | number, b?: number) {
      if (!isScroller(this)) return scrollTo?.call(this, a, b);
      setTop(this, typeof a === "number" ? (b ?? 0) : (a.top ?? top.get(this) ?? 0));
    },
    writable: true,
  });
});
afterAll(() => {
  for (const [k, d] of saved.reverse()) {
    if (d) Object.defineProperty(HTMLElement.prototype, k, d);
    else delete (HTMLElement.prototype as unknown as Record<string, unknown>)[k];
  }
});

const TEAM_KEY = "alderco:default";
const DAY = 86_400_000;

/** `n` lines from the child to Terminal 9, fifteen minutes apart, ending now. */
function longInbox(n: number, now = Date.now()): SessionInboxDto {
  const base = fixtureSessionInbox(FX.t9, now).messages.find((m) => m.from === FX.child)!;
  const messages: ConversationMessage[] = Array.from({ length: n }, (_, i) => ({
    ...base,
    id: i + 1,
    text: `line ${i + 1}`,
    summary: null,
    unread: false,
    createdAtMs: now - (n - 1 - i) * 15 * 60_000,
  }));
  return { handle: FX.t9, roles: [], messages };
}

function more(dto: SessionInboxDto, ...lines: Partial<ConversationMessage>[]): SessionInboxDto {
  const last = dto.messages.at(-1)!;
  return {
    ...dto,
    messages: [
      ...dto.messages,
      ...lines.map((l, i) => ({
        ...last,
        id: last.id + i + 1,
        text: `new ${i + 1}`,
        createdAtMs: last.createdAtMs + (i + 1) * 1000,
        unread: true,
        ...l,
      })),
    ],
  };
}

function inbox(dto: SessionInboxDto) {
  api.loadSessionInbox.mockResolvedValue({ dto, source: "live" });
}

/** Fire the inbox-changed event the rail listens for, as the backend would. */
async function refresh() {
  const calls = vi.mocked(listen).mock.calls as unknown as [string, () => void][];
  await act(async () => {
    for (const [event, cb] of calls) if (event === AGENT_INBOX_CHANGED_EVENT) cb();
  });
}

async function renderRail(lens: "spool" | "threads") {
  useAgentInboxRailStore.setState({ lensByTab: { "tab-1": lens } });
  render(
    <InboxRail
      tabId="tab-1"
      sessionId={FX.t9}
      teamKey={TEAM_KEY}
      composeRequest={0}
      narrow={false}
    />,
  );
  await screen.findByTestId("inbox-rail");
}

const atEnd = (el: Element) => el.scrollTop === maxTop(el);

beforeEach(() => {
  slotHeight = null;
  clearInboxDrafts();
  vi.clearAllMocks();
  useInboxSessionsStore.setState({ sessions: null });
  useAgentListProjectionStore.setState({ sessions: null });
  useAgentInboxRailStore.setState({ bySession: {}, widthByTab: {}, lensByTab: {} });
  api.loadInboxSessions.mockResolvedValue(fixtureSessions());
  api.sendAsHuman.mockResolvedValue({ status: "sent", messageId: 99 });
});

describe("Mailspool on a long log", () => {
  it("opens 500 lines at the newest one and draws only a window of them", async () => {
    inbox(longInbox(500));
    await renderRail("spool");
    const log = await screen.findByTestId("ibx-log");
    await waitFor(() =>
      expect(log.querySelector('[data-log="alderco/default#500"]')).not.toBeNull(),
    );
    expect(atEnd(log)).toBe(true);
    expect(maxTop(log)).toBeGreaterThan(VIEW * 10);
    const rows = log.querySelectorAll(".ibx-lrow");
    expect(rows.length).toBeGreaterThan(0);
    expect(rows.length).toBeLessThan(60);
    expect(log.querySelector('[data-log="alderco/default#1"]')).toBeNull();
    // A windowed listbox still tells assistive tech where each line sits.
    const last = log.querySelector('[data-log="alderco/default#500"]')!;
    expect(last).toHaveAttribute("aria-setsize", "500");
    expect(last).toHaveAttribute("aria-posinset", "500");
  });

  it("follows new mail while the reader is at the bottom, with no jump offered", async () => {
    const dto = longInbox(120);
    inbox(dto);
    await renderRail("spool");
    const log = await screen.findByTestId("ibx-log");
    await waitFor(() => expect(atEnd(log)).toBe(true));
    inbox(more(dto, {}));
    await refresh();
    await waitFor(() =>
      expect(log.querySelector('[data-log="alderco/default#121"]')).not.toBeNull(),
    );
    await waitFor(() => expect(atEnd(log)).toBe(true));
    expect(screen.queryByRole("button", { name: /Jump to latest/ })).toBeNull();
  });

  it("leaves a reader who scrolled up in place and offers Jump to latest with what is new", async () => {
    const dto = longInbox(120);
    inbox(dto);
    await renderRail("spool");
    const log = await screen.findByTestId("ibx-log");
    await waitFor(() => expect(atEnd(log)).toBe(true));
    act(() => setTop(log, 200));
    // Two lines from the child and one of Terminal 9's own: two are new to the reader.
    inbox(more(dto, {}, {}, { from: FX.t9, to: FX.child }));
    await refresh();
    const jump = await screen.findByRole("button", { name: /Jump to latest/ });
    expect(jump).toHaveTextContent("2 new");
    expect(log.scrollTop).toBe(200);
    // The pill never takes focus on its own.
    expect(jump).not.toHaveFocus();
    fireEvent.click(jump);
    await waitFor(() => expect(atEnd(log)).toBe(true));
    await waitFor(() =>
      expect(log.querySelector('[data-log="alderco/default#123"]')).not.toBeNull(),
    );
    expect(screen.queryByRole("button", { name: /Jump to latest/ })).toBeNull();
    // The keyboard lands on the log, so j/k carry on from the newest lines.
    expect(log).toHaveFocus();
  });

  it("announces new mail politely and leaves focus where it was", async () => {
    const dto = longInbox(20);
    inbox(dto);
    await renderRail("spool");
    await screen.findByTestId("ibx-log");
    const filter = screen.getByLabelText("Filter messages");
    filter.focus();
    inbox(more(dto, {}));
    await refresh();
    const note = await waitFor(
      () => {
        const el = screen.getByTestId("ibx-arrivals");
        expect(el).toHaveTextContent("New message from amelia-child-5plus5");
        return el;
      },
      { timeout: 3000 },
    );
    expect(note).toHaveAttribute("aria-live", "polite");
    expect(filter).toHaveFocus();
  });

  it("still walks lines with j/k, bringing a line outside the window into view", async () => {
    inbox(longInbox(200));
    await renderRail("spool");
    const log = await screen.findByTestId("ibx-log");
    await waitFor(() => expect(atEnd(log)).toBe(true));
    // From no selection, j starts where the reader is: the newest line, at the bottom.
    fireEvent.keyDown(log, { key: "j" });
    expect(log.querySelector(".ibx-lrow.sel")).toHaveAttribute("data-log", "alderco/default#200");
    fireEvent.keyDown(log, { key: "k" });
    expect(log.querySelector(".ibx-lrow.sel")).toHaveAttribute("data-log", "alderco/default#199");
    // Walking to the oldest line scrolls it into the window and draws it.
    fireEvent.keyDown(log, { key: "j" });
    fireEvent.keyDown(log, { key: "Home" });
    for (let i = 0; i < 199; i++) fireEvent.keyDown(log, { key: "k" });
    await waitFor(() =>
      expect(log.querySelector(".ibx-lrow.sel")).toHaveAttribute("data-log", "alderco/default#1"),
    );
    // The selected line's slot sits inside the viewport.
    const slot = log.querySelector(".ibx-lrow.sel")!.closest<HTMLElement>(".ibx-lslot")!;
    const y = parseFloat(slot.style.transform.replace(/[^\d.]/g, ""));
    expect(y).toBeGreaterThanOrEqual(log.scrollTop);
    expect(y).toBeLessThan(log.scrollTop + VIEW);
    expect(log.getAttribute("aria-activedescendant")).toBe(log.querySelector(".ibx-lrow.sel")!.id);
    fireEvent.keyDown(log, { key: "j" });
    expect(log.querySelector(".ibx-lrow.sel")).toHaveAttribute("data-log", "alderco/default#2");
  });

  it("from nothing selected, j starts at the lowest line in view of a reader who scrolled up", async () => {
    inbox(longInbox(200));
    await renderRail("spool");
    const log = await screen.findByTestId("ibx-log");
    await waitFor(() => expect(atEnd(log)).toBe(true));
    act(() => setTop(log, 0));
    await waitFor(() => expect(log.querySelector('[data-log="alderco/default#1"]')).not.toBeNull());
    // The lowest row whose slot starts inside the 100px view.
    const bottom = log.scrollTop + VIEW;
    const lowest = [...log.querySelectorAll<HTMLElement>(".ibx-lslot")]
      .map((sl) => ({ sl, y: parseFloat(sl.style.transform.replace(/[^\d.]/g, "")) }))
      .filter(({ sl, y }) => y < bottom && sl.querySelector("[data-log]"))
      .sort((a, b) => b.y - a.y)[0]!
      .sl.querySelector("[data-log]")!
      .getAttribute("data-log");
    fireEvent.keyDown(log, { key: "j" });
    expect(log.querySelector(".ibx-lrow.sel")).toHaveAttribute("data-log", lowest!);
    expect(lowest).not.toBe("alderco/default#200");
  });

  it("follows at the 500 cap, where a line arrives as the oldest leaves, and announces it", async () => {
    const dto = longInbox(500);
    inbox(dto);
    await renderRail("spool");
    const log = await screen.findByTestId("ibx-log");
    await waitFor(() => expect(atEnd(log)).toBe(true));
    const next = more(dto, {});
    inbox({ ...next, messages: next.messages.slice(1) });
    await refresh();
    await waitFor(() =>
      expect(log.querySelector('[data-log="alderco/default#501"]')).not.toBeNull(),
    );
    await waitFor(() => expect(atEnd(log)).toBe(true));
    await waitFor(
      () => expect(screen.getByTestId("ibx-arrivals")).toHaveTextContent("New message from"),
      { timeout: 3000 },
    );
  });

  it("re-opens at the newest line and drops the pill when the filter changes", async () => {
    const dto = longInbox(120);
    inbox(dto);
    await renderRail("spool");
    const log = await screen.findByTestId("ibx-log");
    await waitFor(() => expect(atEnd(log)).toBe(true));
    act(() => setTop(log, 200));
    inbox(more(dto, {}));
    await refresh();
    await screen.findByRole("button", { name: /Jump to latest/ });
    fireEvent.change(screen.getByLabelText("Filter messages"), { target: { value: "line" } });
    await waitFor(() => expect(atEnd(log)).toBe(true));
    expect(screen.queryByRole("button", { name: /Jump to latest/ })).toBeNull();
  });

  it("announces a second message from the same sender again", async () => {
    let dto = longInbox(20);
    inbox(dto);
    await renderRail("spool");
    await screen.findByTestId("ibx-log");
    const note = screen.getByTestId("ibx-arrivals");
    dto = more(dto, {});
    inbox(dto);
    await refresh();
    await waitFor(() => expect(note).toHaveTextContent("New message from amelia-child-5plus5"), {
      timeout: 3000,
    });
    const first = note.textContent;
    inbox(more(dto, {}));
    await refresh();
    await waitFor(() => expect(note.textContent).not.toBe(first), { timeout: 3000 });
    expect(note).toHaveTextContent("New message from amelia-child-5plus5");
  });

  it("coalesces a burst of new mail into one announcement", async () => {
    let dto = longInbox(20);
    inbox(dto);
    await renderRail("spool");
    await screen.findByTestId("ibx-log");
    const note = screen.getByTestId("ibx-arrivals");
    for (let i = 0; i < 3; i++) {
      dto = more(dto, {});
      inbox(dto);
      await refresh();
      await waitFor(() =>
        expect(
          screen.getByTestId("ibx-log").querySelector(`[data-log="alderco/default#${21 + i}"]`),
        ).not.toBeNull(),
      );
    }
    // Nothing per refresh: one note once the window closes.
    expect(note).toHaveTextContent(/^$/);
    await waitFor(() => expect(note).toHaveTextContent("3 new messages"), { timeout: 3000 });
  });

  it("keeps the line a scrolled-up reader is reading in place as the oldest is trimmed, rows of mixed heights", async () => {
    // Every third line is a two-line row, dividers are short.
    slotHeight = (slot) => {
      const row = slot.querySelector<HTMLElement>("[data-log]");
      if (!row) return 31;
      return Number(row.dataset.log!.split("#")[1]) % 3 === 0 ? 52 : 26;
    };
    const dto = longInbox(500);
    inbox(dto);
    await renderRail("spool");
    const log = await screen.findByTestId("ibx-log");
    await waitFor(() => expect(atEnd(log)).toBe(true));
    act(() => setTop(log, Math.round(maxTop(log) / 2)));
    // The line at the top of the view, and where it sits in it.
    const seen = () => {
      const slots = [...log.querySelectorAll<HTMLElement>(".ibx-lslot")];
      const ys = slots.map((s) => ({
        s,
        y: parseFloat(s.style.transform.replace(/[^\d.]/g, "")),
      }));
      const first = ys
        .filter(({ s, y }) => y + slotHeight!(s) > log.scrollTop && s.querySelector("[data-log]"))
        .sort((a, b) => a.y - b.y)[0]!;
      return {
        key: first.s.querySelector("[data-log]")!.getAttribute("data-log"),
        at: first.y - log.scrollTop,
      };
    };
    await waitFor(() => expect(log.querySelector(".ibx-lslot")).not.toBeNull());
    const before = seen();
    const next = more(dto, {});
    inbox({ ...next, messages: next.messages.slice(1) });
    await refresh();
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /Jump to latest/ })).toBeInTheDocument(),
    );
    await waitFor(() => {
      const after = seen();
      expect(after.key).toBe(before.key);
      expect(after.at).toBe(before.at);
    });
  });

  it("r on a selected line scrolled out of the window brings it back with its reply line focused", async () => {
    inbox(longInbox(200));
    await renderRail("spool");
    const log = await screen.findByTestId("ibx-log");
    await waitFor(() => expect(atEnd(log)).toBe(true));
    fireEvent.keyDown(log, { key: "j" });
    await waitFor(() => expect(log.querySelector(".ibx-lrow.sel")).not.toBeNull());
    act(() => setTop(log, 0));
    await waitFor(() => expect(log.querySelector(".ibx-lrow.sel")).toBeNull());
    fireEvent.keyDown(log, { key: "r" });
    await waitFor(() => expect(log.querySelector("[data-logreply]")).toHaveFocus());
    expect(log.querySelector(".ibx-lrow.sel")).toHaveAttribute("data-log", "alderco/default#200");
  });

  it("keeps an open line, and its reply draft, while the reader scrolls away", async () => {
    inbox(longInbox(200));
    await renderRail("spool");
    const log = await screen.findByTestId("ibx-log");
    await waitFor(() => expect(atEnd(log)).toBe(true));
    fireEvent.click(log.querySelector('[data-log="alderco/default#200"]')!);
    const reply = log.querySelector<HTMLInputElement>("[data-logreply]")!;
    fireEvent.change(reply, { target: { value: "half a reply" } });
    act(() => setTop(log, 0));
    await waitFor(() => expect(log.querySelector('[data-log="alderco/default#1"]')).not.toBeNull());
    expect(log.querySelector("[data-logreply]")).toBe(reply);
    expect(reply).toHaveValue("half a reply");
  });

  it("heads each day with Today, Yesterday or the date", async () => {
    const now = Date.now();
    const dto = longInbox(3, now);
    dto.messages[0]!.createdAtMs = now - 3 * DAY;
    dto.messages[1]!.createdAtMs = now - DAY;
    inbox(dto);
    await renderRail("spool");
    const log = await screen.findByTestId("ibx-log");
    await waitFor(() => expect(log.querySelectorAll(".ibx-lday")).toHaveLength(3));
    const days = [...log.querySelectorAll(".ibx-lday")].map((d) => d.textContent);
    expect(days.slice(1)).toEqual(["Yesterday", "Today"]);
    expect(days[0]).toMatch(/\d/);
  });
});

describe("Threads conversation on a long log", () => {
  it("opens at the newest message and offers Jump to latest when mail lands below a reader", async () => {
    const dto = longInbox(30);
    inbox(dto);
    await renderRail("threads");
    const msgs = await screen.findByTestId("ibx-msgs");
    await waitFor(() => expect(msgs.querySelectorAll(".ibx-m").length).toBe(30));
    expect(atEnd(msgs)).toBe(true);
    act(() => setTop(msgs, 0));
    inbox(more(dto, {}));
    await refresh();
    const jump = await screen.findByRole("button", { name: /Jump to latest/ });
    expect(jump).toHaveTextContent("1 new");
    expect(msgs.scrollTop).toBe(0);
    await waitFor(
      () =>
        expect(
          within(screen.getByTestId("ibx-arrivals")).getByText(/New message from/),
        ).toBeTruthy(),
      { timeout: 3000 },
    );
    fireEvent.click(jump);
    await waitFor(() => expect(atEnd(msgs)).toBe(true));
    expect(screen.queryByRole("button", { name: /Jump to latest/ })).toBeNull();
    // Focus lands on the conversation, not the page body, as the pill goes.
    expect(msgs).toHaveFocus();
  });

  it("follows at the 500 cap, where a message arrives as the oldest leaves", async () => {
    const dto = longInbox(500);
    inbox(dto);
    await renderRail("threads");
    const msgs = await screen.findByTestId("ibx-msgs");
    await waitFor(() => expect(atEnd(msgs)).toBe(true));
    const next = more(dto, {});
    inbox({ ...next, messages: next.messages.slice(1) });
    await refresh();
    await waitFor(() =>
      expect(msgs.querySelector('[data-mid="alderco/default#501"]')).not.toBeNull(),
    );
    expect(atEnd(msgs)).toBe(true);
  });

  it.each([
    {
      name: "a day divider leads the viewport at 04:22",
      now: new Date(2026, 9, 10, 4, 22).getTime(),
      expectedKind: ".ibx-day",
      expectedTop: 40,
    },
    {
      name: "a message leads the viewport at noon",
      now: new Date(2026, 9, 10, 12, 0).getTime(),
      expectedKind: "[data-mid]",
      expectedTop: 0,
    },
  ])(
    "keeps a scrolled-up reader's message in place when the oldest is trimmed (no CSS anchoring): $name",
    async ({ now, expectedKind, expectedTop }) => {
      const dto = longInbox(500, now);
      inbox(dto);
      await renderRail("threads");
      const msgs = await screen.findByTestId("ibx-msgs");
      await waitFor(() => expect(atEnd(msgs)).toBe(true));
      act(() => setTop(msgs, 4000));
      expect(msgs.children[100]!.matches(expectedKind)).toBe(true);
      const topOf = (mid: string) =>
        msgs.querySelector(`[data-mid="${mid}"]`)!.getBoundingClientRect().top;
      const viewport = msgs.getBoundingClientRect();
      const visibleMessage = [...msgs.querySelectorAll("[data-mid]")].find((message) => {
        const rect = message.getBoundingClientRect();
        return rect.bottom > viewport.top && rect.top < viewport.bottom;
      });
      expect(visibleMessage).toBeDefined();
      const visibleRect = visibleMessage!.getBoundingClientRect();
      expect(visibleRect.bottom).toBeGreaterThan(viewport.top);
      expect(visibleRect.top).toBeLessThan(viewport.bottom);
      expect(visibleRect.top - viewport.top).toBe(expectedTop);
      const reading = visibleMessage!.getAttribute("data-mid");
      expect(reading).not.toBeNull();
      const at = topOf(reading!);
      const next = more(dto, {});
      inbox({ ...next, messages: next.messages.slice(1) });
      await refresh();
      await waitFor(() =>
        expect(msgs.querySelector('[data-mid="alderco/default#501"]')).not.toBeNull(),
      );
      expect(msgs.querySelector('[data-mid="alderco/default#1"]')).toBeNull();
      expect(topOf(reading!)).toBe(at);
      expect(await screen.findByRole("button", { name: /Jump to latest/ })).toHaveTextContent(
        "1 new",
      );
    },
  );

  it("shows what you just sent even when you had scrolled up", async () => {
    const dto = longInbox(30);
    inbox(dto);
    await renderRail("threads");
    const msgs = await screen.findByTestId("ibx-msgs");
    await waitFor(() => expect(atEnd(msgs)).toBe(true));
    act(() => setTop(msgs, 0));
    inbox(more(dto, { from: FX.t9, to: FX.child }));
    await refresh();
    await waitFor(() => expect(atEnd(msgs)).toBe(true));
    expect(screen.queryByRole("button", { name: /Jump to latest/ })).toBeNull();
  });
});
