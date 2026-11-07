// SPDX-License-Identifier: MIT
vi.mock("@/features/dashboard/lib/verb-dispatch", async () =>
  (await import("@/test/verb-dispatch-mock")).openVerbDispatchMock(),
);
vi.mock("@/shared/lib/local-profile-session", () => ({ rendererPrincipal: async () => "42" }));
import { FrameMessageEvent } from "../../../../tests/support/fixtures/frame";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

import { verbDispatch, verbSettle } from "@/features/dashboard/lib/verb-dispatch";
import { liveHub } from "@/features/dashboard/lib/live-hub";
import { useOrgStore } from "@/shared/stores/org-store";

import { InboxMessageContent } from "./inbox-message-content";
import {
  __resetMessageContentRootsForTest,
  messageTabPath,
  parseMessageTabPath,
} from "./inbox-message-dashboard";

const {
  mockListInboxMessages,
  mockBridgeDocContext,
  mockSendRoleMessage,
  mockListPanes,
  mockOpenManagementTab,
  listeners,
} = vi.hoisted(() => ({
  mockListInboxMessages: vi.fn(),
  mockBridgeDocContext: vi.fn(),
  mockSendRoleMessage: vi.fn(),
  mockListPanes: vi.fn(),
  mockOpenManagementTab: vi.fn(),
  listeners: new Map<string, Array<(e: { payload: unknown }) => void>>(),
}));

vi.mock("@/tauri/commands", async () => ({
  ...(await import("../../../../tests/support/fixtures/frame")).frameCommands(),
  getHttpApiPort: () => Promise.resolve(9123),
  listPanes: (org: string, team: string) => mockListPanes(org, team),
  liveHubViewKey: async () => "team:acme/web",
  liveHubSubscribe: async (topic: string, viewKey: string) => ({
    topic,
    viewKey,
    rev: 0,
    snapshot: [],
  }),
  liveHubUnsubscribe: async () => {},
  agentReadInbox: (...args: unknown[]) => mockListInboxMessages(...args),
  bridgeDocContext: (page: string) => mockBridgeDocContext(page),
  sendRoleMessage: (...args: unknown[]) => mockSendRoleMessage(...args),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: (e: { payload: unknown }) => void) => {
    listeners.set(name, [...(listeners.get(name) ?? []), cb]);
    return Promise.resolve(() => {
      listeners.set(
        name,
        (listeners.get(name) ?? []).filter((c) => c !== cb),
      );
    });
  },
}));

vi.mock("@/shared/lib/open-management-tab", () => ({
  openManagementTab: (path: string) => mockOpenManagementTab(path),
}));

// The launch controller is not under test here (its own suites cover it).
vi.mock("@/features/dashboard/components/dashboard-launch-controller", () => ({
  useRegisterDashboardLaunchController: () => null,
}));

const mockFetch = vi.fn();
globalThis.fetch = mockFetch;

const TEAM = { org: "acme", team: "web" };
const CONTENT_ROOT = "/ctx/acme/web/content";
const b64url = (s: string) => btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");

beforeEach(async () => {
  await liveHub.dispose();
  vi.clearAllMocks();
  listeners.clear();
  __resetMessageContentRootsForTest();
  mockFetch.mockResolvedValue({ status: 200, ok: true });
  mockListPanes.mockResolvedValue({ contentRoot: CONTENT_ROOT, chain: [], panes: [] });
  mockListInboxMessages.mockResolvedValue({ messages: [], humanUnread: 0 });
  mockSendRoleMessage.mockResolvedValue({ status: "sent", messageId: 99, deliveryId: 1 });
  mockBridgeDocContext.mockResolvedValue({
    org: "acme",
    team: "web",
    layer: "team",
    contentRoot: CONTENT_ROOT,
  });
  // The team on screen is NOT the message's team: bridge calls must still act in
  // the message's own team.
  useOrgStore.setState({ activeOrgSlug: "other-org", activeTeamName: "other-team" });
});

afterEach(() => {
  cleanup();
});

function renderHtmlMessage(text = "<p id=o></p><script>codesense.sessions()</script>") {
  return render(
    <InboxMessageContent
      text={text}
      contentType="html"
      live={{ team: TEAM, messageId: 42, sender: "agent-7" }}
    />,
  );
}

/** The mounted message frame, with a stub window standing in for its document. */
async function frameWithWindow() {
  const iframe = await waitFor(() => {
    const el = document.querySelector("iframe");
    expect(el?.getAttribute("src")).toBeTruthy();
    return el!;
  });
  const postMessage = vi.fn();
  const fakeWindow = { postMessage } as unknown as Window;
  Object.defineProperty(iframe, "contentWindow", { value: fakeWindow, configurable: true });
  return { iframe, fakeWindow, postMessage };
}

function fromFrame(source: Window, data: Record<string, unknown>) {
  window.dispatchEvent(new FrameMessageEvent("message", { data, source }));
}

describe("html message to the human", () => {
  it("mounts the live dashboard frame, not sanitized markup", async () => {
    renderHtmlMessage();
    const { iframe } = await frameWithWindow();

    const body = screen.getByTestId("inbox-message-body");
    expect(body).toHaveAttribute("data-content-type", "html");
    expect(body).toHaveAttribute("data-live", "true");
    // The body is served by the message route (same shim + capability as a file),
    // never inlined into the inbox DOM.
    // Pane frame URLs carry the startup theme as `?theme=`.
    const src = new URL(iframe.getAttribute("src")!);
    expect(`${src.origin}${src.pathname}`).toBe(
      `http://f-test.localhost:9123/message/${b64url("acme")}/${b64url("web")}/42`,
    );
    expect(src.searchParams.get("theme")).toBeTruthy();
    expect(iframe.getAttribute("sandbox")).toBe(
      "allow-scripts allow-same-origin allow-forms allow-popups allow-modals allow-downloads",
    );
    expect(body.innerHTML).not.toContain("codesense.sessions()");
    expect(mockListPanes).toHaveBeenCalledWith("acme", "web");
  });

  it("a bridge call from the frame resolves through the message's team", async () => {
    mockListInboxMessages.mockResolvedValue({ messages: [{ messageId: 1 }], humanUnread: 3 });
    renderHtmlMessage();
    const { fakeWindow, postMessage } = await frameWithWindow();

    fromFrame(fakeWindow, { type: "codesense:invoke", id: 5, command: "inbox", args: {} });

    await waitFor(() =>
      expect(postMessage).toHaveBeenCalledWith(
        {
          type: "codesense:invoke-result",
          id: 5,
          ok: true,
          data: { messages: [{ messageId: 1 }], humanUnread: 3 },
        },
        "http://f-test.localhost:9123",
      ),
    );
    // The message's team, not the team on screen.
    expect(mockListInboxMessages).toHaveBeenCalledWith(TEAM, {});
  });

  it("an action from the frame goes through the action gate in the message's team", async () => {
    renderHtmlMessage();
    const { fakeWindow, postMessage } = await frameWithWindow();

    fromFrame(fakeWindow, {
      type: "codesense:invoke",
      id: 6,
      command: "postMessage",
      args: { to: "reviewer", text: "approve" },
    });

    await waitFor(() =>
      expect(postMessage).toHaveBeenCalledWith(
        expect.objectContaining({ type: "codesense:invoke-result", id: 6, ok: true }),
        "http://f-test.localhost:9123",
      ),
    );
    const [team, sender, role, text] = mockSendRoleMessage.mock.calls[0];
    expect(team).toEqual(TEAM);
    // The frame's page anchors in the message's team content root.
    expect(sender).toBe(`${CONTENT_ROOT}/inbox-message-42.html`);
    expect([role, text]).toEqual(["reviewer", "approve"]);
    expect(verbDispatch).toHaveBeenCalledWith(
      expect.objectContaining({
        scope: "open",
        dashboard: "inbox-message://acme/web/42",
      }),
      "postMessage",
      expect.anything(),
    );
    expect(verbSettle).toHaveBeenCalledWith("call", true, "admitted");
  });

  it("context() answers where the message's team lives", async () => {
    renderHtmlMessage();
    const { fakeWindow, postMessage } = await frameWithWindow();

    fromFrame(fakeWindow, { type: "codesense:invoke", id: 7, command: "context", args: null });

    await waitFor(() =>
      expect(postMessage).toHaveBeenCalledWith(
        expect.objectContaining({ id: 7, ok: true }),
        "http://f-test.localhost:9123",
      ),
    );
    expect(mockBridgeDocContext).toHaveBeenCalledWith(`${CONTENT_ROOT}/inbox-message-42.html`);
  });

  it("hears the same pushed inbox and agent-activity events a dashboard does", async () => {
    renderHtmlMessage();
    const { postMessage, fakeWindow } = await frameWithWindow();
    fromFrame(fakeWindow, { type: "codesense:ready" });

    listeners.get("agent-inbox-changed")?.forEach((cb) => cb({ payload: null }));
    listeners.get("mcp:agent-message")?.forEach((cb) => cb({ payload: null }));

    expect(postMessage).toHaveBeenCalledWith(
      { type: "codesense:inbox-changed", token: "frame-token" },
      "http://f-test.localhost:9123",
    );
    expect(postMessage).toHaveBeenCalledWith(
      { type: "codesense:agent-activity-changed", token: "frame-token" },
      "http://f-test.localhost:9123",
    );
  });

  it("sizes the inline frame to the height the doc reports", async () => {
    renderHtmlMessage();
    const { fakeWindow } = await frameWithWindow();
    const holder = screen.getByTestId("inbox-message-frame");
    expect(holder.style.height).toBe("160px");

    fromFrame(fakeWindow, { type: "codesense:content-size", height: 312.4 });
    await waitFor(() => expect(holder.style.height).toBe("313px"));

    fromFrame(fakeWindow, { type: "codesense:content-size", height: 99999 });
    await waitFor(() => expect(holder.style.height).toBe("720px"));
  });

  it("Open in tab opens the message as its own tab", async () => {
    renderHtmlMessage();
    await frameWithWindow();
    fireEvent.click(screen.getByTestId("inbox-message-open-tab"));
    expect(mockOpenManagementTab).toHaveBeenCalledWith(messageTabPath(TEAM, 42, "agent-7"));
  });
});

describe("html message between agents", () => {
  // What the sanitizer keeps is covered under jsdom in inbox-message-content.test.tsx.
  it("stays sanitized static markup with no frame", () => {
    render(<InboxMessageContent text="<p>ok</p><script>alert(1)</script>" contentType="html" />);
    const body = screen.getByTestId("inbox-message-body");
    expect(body).not.toHaveAttribute("data-live");
    expect(document.querySelector("iframe")).toBeNull();
    expect(mockListPanes).not.toHaveBeenCalled();
  });
});

describe("message tab path", () => {
  it("round-trips team, id and sender, including awkward characters", () => {
    const team = { org: "acme co", team: "web/ui" };
    const path = messageTabPath(team, 42, "agent #7");
    expect(path.startsWith("virtual://inbox-message/")).toBe(true);
    expect(parseMessageTabPath(path)).toEqual({ team, messageId: 42, sender: "agent #7" });
  });

  it("rejects a malformed path", () => {
    expect(parseMessageTabPath("virtual://inbox-message/acme/web")).toBeNull();
    expect(parseMessageTabPath("virtual://inbox-message/acme/web/x/s")).toBeNull();
    expect(parseMessageTabPath("virtual://other/acme/web/1/s")).toBeNull();
  });
});
