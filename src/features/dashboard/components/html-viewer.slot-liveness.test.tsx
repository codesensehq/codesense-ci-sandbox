// SPDX-License-Identifier: MIT
import { beforeEach, afterEach, it, expect, vi } from "vitest";
import { render, waitFor, cleanup, act, fireEvent, screen } from "@testing-library/react";
const { ipc } = vi.hoisted(() => ({ ipc: vi.fn() }));
vi.mock("@tauri-apps/api/core", async (original) => ({
  ...(await original<typeof import("@tauri-apps/api/core")>()),
  invoke: (command: string, args: unknown) => ipc(command, args),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}), emit: vi.fn() }));
vi.mock("@/shared/lib/logger", () => ({
  logger: { error: vi.fn(), warn: vi.fn(), info: vi.fn(), debug: vi.fn(), trace: vi.fn() },
}));
import { SlotDashboardHost } from "@/features/shell/slots/slot-dashboard-host";
vi.mock("@/shared/lib/viewer-perf-config", () => ({ VIEWER_PERF: { maxBridgedFrames: 1 } }));
import { HtmlViewer } from "./html-viewer";
import { runRenderedDocumentRequest } from "@/shared/hooks/use-rendered-document-bridge";
import { liveHub } from "@/features/dashboard/lib/live-hub";
import { useOrgStore } from "@/shared/stores/org-store";
import { logger } from "@/shared/lib/logger";
const record = {
  kind: "private",
  documentPath: "/ctx/content/board.html",
  frameId: "fresh",
  origin: "http://f-fresh.localhost:9321",
  token: "frame-secret",
  grant: {
    frameId: "fresh",
    principal: "human",
    dashboard: "/ctx/content/board.html",
    slot: "centre",
    team: null,
    scope: "open",
  },
};
beforeEach(() => {
  ipc.mockImplementation(async (command: string) => {
    if (command === "get_http_api_port") return 9321;
    if (command === "local_profile_status") return { active: { principal: "human" } };
    if (command === "get_proxy_capability") return "process-secret";
    if (command === "mint_frame") return record;
    if (command === "resolve_frame_grant") return record.grant;
    if (command === "live_hub_view_key") return "all";
    return null;
  });
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({ ok: true, status: 200 })),
  );
});
afterEach(() => {
  cleanup();
  liveHub.dispose();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});
it("counts an admitted hidden page subscribe before its deferred snapshot, and never counts a denied one", async () => {
  const base = ipc.getMockImplementation()!;
  let admitted = false;
  ipc.mockImplementation(async (command, args) => {
    if (command === "verb_dispatch")
      return admitted
        ? { status: "admit", callId: "slot-subscribe" }
        : { status: "deny", reason: "not-allowed" };
    if (command === "live_hub_subscribe")
      return { topic: "sessions", viewKey: "all", rev: 0, snapshot: [] };
    return base(command, args);
  });
  const onDocLive = vi.fn();
  const { rerender } = render(
    <HtmlViewer
      filePath="/ctx/content/board.html"
      teamContentPath="/ctx/content"
      visible={false}
      onDocLive={onDocLive}
    />,
  );
  await waitFor(() => expect(document.querySelector("iframe")?.src).toContain(record.origin));
  const postMessage = vi.fn();
  const source = { postMessage } as unknown as Window;
  Object.defineProperty(document.querySelector("iframe")!, "contentWindow", {
    value: source,
    configurable: true,
  });
  const send = (id: number) =>
    window.dispatchEvent(
      new MessageEvent("message", {
        source,
        origin: record.origin,
        data: {
          type: "codesense:invoke",
          id,
          command: "subscribe",
          token: record.token,
          args: { topic: "sessions" },
        },
      }),
    );
  send(1);
  await waitFor(() => expect(postMessage).toHaveBeenCalled());
  expect(onDocLive).not.toHaveBeenCalled();
  expect(postMessage.mock.calls[0][0]).toMatchObject({ ok: false });
  admitted = true;
  postMessage.mockClear();
  send(2);
  await waitFor(() => expect(onDocLive).toHaveBeenCalledOnce());
  expect(postMessage).not.toHaveBeenCalled();
  rerender(
    <HtmlViewer
      filePath="/ctx/content/board.html"
      teamContentPath="/ctx/content"
      visible
      onDocLive={onDocLive}
    />,
  );
  await waitFor(() => expect(postMessage).toHaveBeenCalled());
  expect(postMessage.mock.calls.at(-1)?.[0]).toMatchObject({ ok: true });
});

const pane = {
  id: "inbox",
  title: "Triage",
  scope: "team" as const,
  team: "dev",
  surface: "inbox",
  path: record.documentPath,
  panesDir: "/ctx/content",
  panelId: "pane:acme:dev/inbox",
};
function host(visible = true) {
  return (
    <SlotDashboardHost
      slotId="right-pane"
      entry={pane}
      contentRoot="/ctx/content"
      visible={visible}
    />
  );
}
function announce(frame: HTMLIFrameElement, type: string) {
  window.dispatchEvent(
    new MessageEvent("message", {
      source: frame.contentWindow,
      origin: new URL(frame.src).origin,
      data: { type, token: record.token },
    }),
  );
}
it("file-nav readiness delivers the latest team context, theme and visibility after early posts are lost", async () => {
  const view = (teamKey: string) => (
    <HtmlViewer
      filePath={record.documentPath}
      teamContentPath="/ctx/content"
      hostTeamKey={teamKey}
      pane={{
        contentRoot: "/ctx/content",
        panesDir: "/ctx/content",
        fileName: "board.html",
        slotId: "file-nav",
      }}
      visible
    />
  );
  const { rerender } = render(view("acme:dev"));
  await waitFor(() => expect(document.querySelector("iframe")?.src).toContain(record.origin));
  const frame = document.querySelector("iframe")!;
  const postMessage = vi.fn();
  Object.defineProperty(frame, "contentWindow", { value: { postMessage }, configurable: true });
  // The first real handshake may arrive after the host has already changed teams.
  rerender(view("acme:other"));
  expect(
    postMessage.mock.calls.some(([message]) => message.type === "codesense:slot-context"),
  ).toBe(false);
  postMessage.mockClear();
  act(() => announce(frame, "codesense:ready"));
  expect(postMessage).toHaveBeenCalledWith(
    expect.objectContaining({
      type: "codesense:slot-context",
      context: expect.objectContaining({
        slotId: "file-nav",
        team: { org: "acme", team: "other" },
      }),
    }),
    record.origin,
  );
  expect(postMessage).toHaveBeenCalledWith(
    expect.objectContaining({ type: "codesense:theme" }),
    record.origin,
  );
  expect(postMessage).toHaveBeenCalledWith(
    expect.objectContaining({ type: "codesense:visibility" }),
    record.origin,
  );
});
const filesKey = (base: string) =>
  `${base}|files:${JSON.stringify(["team", null, null, null, null, "/content", "human"])}`;
const filesReply = (id: number, base: string) =>
  expect.objectContaining({
    type: "codesense:invoke-result",
    id,
    ok: true,
    data: expect.objectContaining({
      topic: "files",
      viewKey: filesKey(base),
      snapshot: [expect.objectContaining({ name: `${base}.md` })],
    }),
  });
const filesSnapshot = (args: any) => ({
  topic: args.topic,
  viewKey: args.viewKey,
  rev: 1,
  snapshot: [{ id: "row", kind: "file", name: `${args.viewKey.split("|")[0]}.md` }],
});
it("keeps a cold replacement file-nav frame delivering after a pane source save", async () => {
  vi.useFakeTimers();
  const base = ipc.getMockImplementation()!;
  const records = new Map<string, typeof record>();
  let minted = 0;
  const reads: Array<[string, string]> = [];
  ipc.mockImplementation(async (command: string, args: any) => {
    if (command === "mint_frame") {
      const id = `save-${++minted}`;
      const next = {
        ...record,
        frameId: id,
        token: `token-${id}`,
        origin: `http://f-${id}.localhost:9321`,
        grant: { ...record.grant, frameId: id },
      };
      records.set(next.token, next);
      return next;
    }
    if (command === "resolve_frame_grant") return records.get(args.token)?.grant;
    if (command === "live_hub_view_key") return "team:acme/dev";
    if (command === "file_surface_prepare_view") return filesKey("team:acme/dev");
    if (command === "verb_dispatch") return { status: "admit", callId: `call-${args.verb}` };
    if (command === "live_hub_subscribe") {
      reads.push([args.topic, args.viewKey]);
      return filesSnapshot(args);
    }
    return base(command, args);
  });
  const view = (modified: string) => (
    <SlotDashboardHost
      slotId="file-nav"
      entry={{ ...pane, modified }}
      contentRoot="/ctx/content"
      visible
    />
  );
  let mounted!: ReturnType<typeof render>;
  await act(async () => {
    mounted = render(view("2026-10-23T00:00:00Z"));
  });
  expect(document.querySelectorAll("iframe")).toHaveLength(1);
  const first = document.querySelector("iframe")!;
  const sendSubscribe = (frame: HTMLIFrameElement, token: string, id: number) =>
    window.dispatchEvent(
      new MessageEvent("message", {
        source: frame.contentWindow,
        origin: new URL(frame.src).origin,
        data: {
          type: "codesense:invoke",
          id,
          token,
          command: "subscribe",
          args: { topic: "files" },
        },
      }),
    );
  await act(async () => {
    mounted.rerender(view("2026-10-23T00:01:00Z"));
  });
  // Source saves debounce for 150 ms and wait up to 300 ms for outgoing writes.
  // This cold fixture has no flush responder. Drive both deadlines explicitly
  // rather than racing them against waitFor's wall-clock timeout.
  await act(async () => {
    await vi.advanceTimersByTimeAsync(300);
  });
  expect(document.querySelectorAll("iframe")).toHaveLength(2);
  const candidate = [...document.querySelectorAll("iframe")].find((frame) => frame !== first)!;
  const candidatePost = vi.fn();
  Object.defineProperty(candidate, "contentWindow", {
    value: { postMessage: candidatePost },
    configurable: true,
  });
  expect(records.size).toBe(2);
  const candidateToken = [...records.keys()][1];
  act(() =>
    window.dispatchEvent(
      new MessageEvent("message", {
        source: candidate.contentWindow,
        origin: new URL(candidate.src).origin,
        data: { type: "codesense:live", token: candidateToken },
      }),
    ),
  );
  await act(async () => sendSubscribe(candidate, candidateToken, 2));
  expect(reads).toHaveLength(1);
  expect(reads.map(([topic]) => topic)).toEqual(["files"]);
  expect(candidatePost).toHaveBeenCalledWith(
    filesReply(2, "team:acme/dev"),
    new URL(candidate.src).origin,
  );
});
it("delivers a hidden cold file-nav subscription when the pane is shown", async () => {
  const base = ipc.getMockImplementation()!;
  const reads: string[] = [];
  ipc.mockImplementation(async (command: string, args: any) => {
    if (command === "live_hub_view_key") return "team:acme/dev";
    if (command === "file_surface_prepare_view") return filesKey("team:acme/dev");
    if (command === "verb_dispatch") return { status: "admit", callId: "hidden-file-subscribe" };
    if (command === "live_hub_subscribe") {
      reads.push(args.topic);
      return filesSnapshot(args);
    }
    return base(command, args);
  });
  const mounted = render(
    <SlotDashboardHost slotId="file-nav" entry={pane} contentRoot="/ctx/content" visible={false} />,
  );
  await waitFor(() => expect(document.querySelector("iframe")?.src).toContain(record.origin));
  const frame = document.querySelector("iframe")!;
  const post = vi.fn();
  Object.defineProperty(frame, "contentWindow", {
    value: { postMessage: post },
    configurable: true,
  });
  act(() =>
    window.dispatchEvent(
      new MessageEvent("message", {
        source: frame.contentWindow,
        origin: record.origin,
        data: {
          type: "codesense:invoke",
          id: 1,
          token: record.token,
          command: "subscribe",
          args: { topic: "files" },
        },
      }),
    ),
  );
  await waitFor(() => expect(ipc).toHaveBeenCalledWith("verb_dispatch", expect.anything()));
  await act(async () => {});
  expect(reads).toEqual([]);

  mounted.rerender(
    <SlotDashboardHost slotId="file-nav" entry={pane} contentRoot="/ctx/content" visible />,
  );
  await waitFor(() => expect(reads).toEqual(["files"]));
  await waitFor(() =>
    expect(post).toHaveBeenCalledWith(filesReply(1, "team:acme/dev"), record.origin),
  );
});
it("delivers the first files listing after a visible file-nav frame switches teams", async () => {
  const base = ipc.getMockImplementation()!;
  const reads: string[] = [];
  let current = {
    ...record,
    kind: "team",
    grant: { ...record.grant, team: { org: "acme", team: "a" } },
  };
  ipc.mockImplementation(async (command: string, args: any) => {
    if (command === "mint_frame") return current;
    if (command === "resolve_frame_grant") return current.grant;
    if (command === "live_hub_view_key") return `team:acme/${current.grant.team.team}`;
    if (command === "rebind_frame_team") {
      current = {
        ...current,
        token: "team-b-token",
        grant: { ...current.grant, team: { org: args.org, team: args.team } },
      };
      return current;
    }
    if (command === "file_surface_prepare_view")
      return filesKey(`team:acme/${current.grant.team.team}`);
    if (command === "verb_dispatch") return { status: "admit", callId: "team-files" };
    if (command === "live_hub_subscribe") {
      reads.push(args.viewKey);
      return filesSnapshot(args);
    }
    return base(command, args);
  });
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "a" });
  render(
    <HtmlViewer
      filePath={record.documentPath}
      teamContentPath="/ctx/content"
      hostTeamKey="acme:a"
      pane={{
        contentRoot: "/ctx/content",
        panesDir: "/ctx/content",
        fileName: "board.html",
        slotId: "file-nav",
      }}
      visible
    />,
  );
  await waitFor(() => expect(document.querySelector("iframe")?.src).toContain(record.origin));
  const frame = document.querySelector("iframe")!;
  const post = vi.fn((message: { type: string; token?: string }) => {
    if (message.type === "codesense:rebind-pending")
      queueMicrotask(() =>
        window.dispatchEvent(
          new MessageEvent("message", {
            source: frame.contentWindow,
            origin: record.origin,
            data: { type: "codesense:rebind-ack", token: message.token },
          }),
        ),
      );
  });
  Object.defineProperty(frame, "contentWindow", {
    value: { postMessage: post },
    configurable: true,
  });
  const send = (data: Record<string, unknown>) =>
    window.dispatchEvent(
      new MessageEvent("message", {
        source: frame.contentWindow,
        origin: record.origin,
        data: { ...data, token: current.token },
      }),
    );
  act(() =>
    send({ type: "codesense:invoke", id: 1, command: "subscribe", args: { topic: "files" } }),
  );
  await waitFor(() => expect(reads).toEqual([filesKey("team:acme/a")]));
  act(() => send({ type: "codesense:ready" }));
  act(() => useOrgStore.setState({ activeTeamName: "b" }));
  await waitFor(() => expect(current.grant.team.team).toBe("b"));
  act(() =>
    send({ type: "codesense:invoke", id: 2, command: "subscribe", args: { topic: "files" } }),
  );
  await waitFor(() => expect(reads).toEqual([filesKey("team:acme/a"), filesKey("team:acme/b")]));
  await waitFor(() =>
    expect(post).toHaveBeenCalledWith(filesReply(2, "team:acme/b"), record.origin),
  );
  expect(post).toHaveBeenCalledWith(filesReply(1, "team:acme/a"), record.origin);
});
it("keeps one startup deadline across the shim and iframe load", async () => {
  vi.useFakeTimers();
  await act(async () => {
    render(host());
  });
  expect(document.querySelector("iframe")?.src).toContain(record.origin);
  const frame = document.querySelector("iframe")!;
  act(() => announce(frame, "codesense:ready"));
  act(() => vi.advanceTimersByTime(4000));
  expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
  fireEvent.load(frame);
  for (let i = 0; i < 5; i++) {
    act(() => vi.advanceTimersByTime(5000));
    act(() => announce(frame, "codesense:pong"));
  }
  act(() => vi.advanceTimersByTime(999));
  expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
  act(() => vi.advanceTimersByTime(1));
  expect(screen.getByTestId("slot-fallback-strip")).toBeInTheDocument();
});
it("a budget-evicted live frame must complete a fresh startup after re-mint", async () => {
  const base = ipc.getMockImplementation()!;
  let minted = 0;
  ipc.mockImplementation(async (command, args) =>
    command === "mint_frame"
      ? { ...record, kind: "team", frameId: `evict-${++minted}` }
      : base(command, args),
  );
  const main = render(host(false));
  await waitFor(() => expect(main.container.querySelector("iframe")?.src).toContain(record.origin));
  const first = main.container.querySelector("iframe")!;
  fireEvent.load(first);
  act(() => announce(first, "codesense:live"));
  const other = render(
    <HtmlViewer filePath="/ctx/content/other.html" teamContentPath="/ctx/content" visible />,
  );
  await waitFor(() =>
    expect(main.container.querySelector("iframe")?.getAttribute("src")).toBeNull(),
  );
  other.unmount();
  vi.useFakeTimers();
  await act(async () => {
    main.rerender(host(true));
  });
  expect(main.container.querySelector("iframe")?.src).toContain(record.origin);
  expect(minted).toBe(3);
  const next = main.container.querySelector("iframe")!;
  expect(next).not.toBe(first);
  expect(next.style.visibility).not.toBe("hidden");
  fireEvent.load(next);
  // Unreachable while it starts, but not left retired: its own readiness opens it.
  act(() => announce(next, "codesense:ready"));
  act(() => announce(next, "codesense:theme-applied"));
  expect(next.inert).not.toBe(true);
  act(() => vi.advanceTimersByTime(30000));
  expect(screen.getByTestId("slot-fallback-strip")).toBeInTheDocument();
});

it("forwards a pane's resolved live badge declaration to the shim and hidden hub", async () => {
  const base = ipc.getMockImplementation()!;
  ipc.mockImplementation(async (command, args) => {
    if (command === "verb_dispatch") return { status: "admit", callId: "live-badge" };
    if (command === "live_hub_subscribe")
      return { topic: "inbox", viewKey: "all", rev: 0, snapshot: [] };
    return base(command, args);
  });
  render(
    <SlotDashboardHost
      slotId="right-pane"
      entry={{ ...pane, declaration: { slots: ["right-pane"], capabilities: [], badge: "live" } }}
      contentRoot="/ctx/content"
      visible={false}
    />,
  );
  await waitFor(() => expect(document.querySelector("iframe")?.src).toContain(record.origin));
  const source = { postMessage: vi.fn() } as unknown as Window;
  Object.defineProperty(document.querySelector("iframe")!, "contentWindow", {
    value: source,
    configurable: true,
  });
  act(() => announce(document.querySelector("iframe")!, "codesense:ready"));
  expect(source.postMessage).toHaveBeenCalledWith(
    expect.objectContaining({
      type: "codesense:slot-context",
      declaration: { badge: "live" },
      token: record.token,
    }),
    record.origin,
  );
  act(() =>
    window.dispatchEvent(
      new MessageEvent("message", {
        source,
        origin: record.origin,
        data: {
          type: "codesense:invoke",
          id: 42,
          command: "subscribe",
          token: record.token,
          args: { topic: "inbox" },
        },
      }),
    ),
  );
  await waitFor(() =>
    expect(source.postMessage).toHaveBeenCalledWith(
      expect.objectContaining({ id: 42, ok: true }),
      record.origin,
    ),
  );
  expect(
    ipc.mock.calls.find(([command]) => command === "live_hub_subscribe")?.[1].subscriptionId,
  ).toContain(pane.panelId);
});

it("holds a candidate intent until its frame is presented", async () => {
  const delivered = vi.fn();
  const intent = { focusList: true };
  const view = render(
    <HtmlViewer
      filePath={record.documentPath}
      teamContentPath="/ctx/content"
      intent={intent}
      onIntentDelivered={delivered}
      presented={false}
    />,
  );
  await waitFor(() => expect(document.querySelector("iframe")?.src).toContain(record.origin));
  const frame = document.querySelector("iframe")!;
  const postMessage = vi.fn();
  Object.defineProperty(frame, "contentWindow", { value: { postMessage }, configurable: true });
  act(() => announce(frame, "codesense:ready"));
  expect(delivered).not.toHaveBeenCalled();
  expect(postMessage.mock.calls.some(([message]) => message.type === "codesense:intent")).toBe(
    false,
  );
  view.rerender(
    <HtmlViewer
      filePath={record.documentPath}
      teamContentPath="/ctx/content"
      intent={intent}
      onIntentDelivered={delivered}
      presented
    />,
  );
  await waitFor(() => expect(delivered).toHaveBeenCalledOnce());
  expect(
    postMessage.mock.calls.filter(([message]) => message.type === "codesense:intent"),
  ).toHaveLength(1);
});

it("captures only the visible recovery document and retains outgoing edits until flush acknowledgement", async () => {
  const base = ipc.getMockImplementation()!;
  let minted = 0;
  ipc.mockImplementation(async (command, args) => {
    if (command === "mint_frame") {
      const id = `recovery-${++minted}`;
      return {
        ...record,
        frameId: id,
        origin: `http://f-${id}.localhost:9321`,
        grant: { ...record.grant, frameId: id },
      };
    }
    return base(command, args);
  });
  const renderHost = (refreshKey: number) => (
    <SlotDashboardHost
      slotId="dock"
      entry={pane}
      contentRoot="/ctx/content"
      visible
      refreshKey={refreshKey}
    />
  );
  const view = render(renderHost(1));
  await waitFor(() => expect(document.querySelector("iframe")?.src).toContain("recovery-1"));
  const outgoing = document.querySelector("iframe")!;
  const connect = (frame: HTMLIFrameElement, content: string) => {
    let flushes = 0;
    const post = vi.fn((message: any) => {
      if (
        message.type === "codesense:before-reload" ||
        (message.type === "codesense:flush" && ++flushes === 1)
      ) {
        window.dispatchEvent(
          new MessageEvent("message", {
            source,
            origin: new URL(frame.src).origin,
            data: {
              type:
                message.type === "codesense:flush"
                  ? "codesense:flush-ack"
                  : "codesense:before-reload-result",
              id: message.id,
              state: null,
              token: record.token,
            },
          }),
        );
      }
      if (message.type !== "codesense:snapshot-request") return;
      window.dispatchEvent(
        new MessageEvent("message", {
          source,
          origin: new URL(frame.src).origin,
          data: {
            type: "codesense:snapshot-result",
            token: record.token,
            id: message.id,
            ok: true,
            content,
            sourceUrl: frame.src,
            capturedAt: "2026-10-22T00:00:00Z",
            truncated: false,
          },
        }),
      );
    });
    const source = { postMessage: post } as unknown as Window;
    Object.defineProperty(frame, "contentWindow", { value: source, configurable: true });
    vi.spyOn(frame, "getClientRects").mockReturnValue([{}] as unknown as DOMRectList);
    return post;
  };
  const oldPost = connect(outgoing, "Retained useful content");
  act(() => {
    announce(outgoing, "codesense:ready");
    announce(outgoing, "codesense:theme-applied");
    announce(outgoing, "codesense:live");
  });
  outgoing.focus();
  view.rerender(renderHost(2));
  await waitFor(() => expect(document.querySelectorAll("iframe")).toHaveLength(2));
  await waitFor(() =>
    expect(document.querySelector<HTMLIFrameElement>("[data-pane-current] iframe")?.src).toContain(
      "recovery-2",
    ),
  );
  const candidate = document.querySelector<HTMLIFrameElement>("[data-pane-current] iframe")!;
  connect(candidate, "Replacement useful content");
  const read = async () => {
    const resolve = vi.fn().mockResolvedValue(undefined);
    await runRenderedDocumentRequest(
      { requestId: "recovery-capture", path: record.documentPath, format: "text" },
      resolve,
    );
    expect(resolve.mock.calls[0][2]).toBeNull();
    return resolve.mock.calls[0][1].content;
  };
  expect(await read()).toBe("Retained useful content");
  act(() => {
    announce(candidate, "codesense:ready");
    announce(candidate, "codesense:theme-applied");
    announce(candidate, "codesense:live");
  });
  expect(outgoing.isConnected).toBe(true);
  expect(oldPost.mock.calls.some(([message]) => message.type === "codesense:flush")).toBe(true);
  expect(await read()).toBe("Replacement useful content");
  expect(document.activeElement).toBe(candidate);
  await waitFor(() =>
    expect(oldPost.mock.calls.filter(([m]) => m.type === "codesense:flush")).toHaveLength(2),
  );
  const flush = oldPost.mock.calls
    .filter(([message]) => message.type === "codesense:flush")
    .at(-1)![0];
  await act(async () => {
    window.dispatchEvent(
      new MessageEvent("message", {
        source: outgoing.contentWindow,
        origin: new URL(outgoing.src).origin,
        data: { type: "codesense:flush-ack", id: flush.id, token: record.token },
      }),
    );
  });
  expect(outgoing.isConnected).toBe(false);
  expect(candidate.isConnected).toBe(true);
});

it.each(["script", "timeout"] as const)(
  "flushes pending edits before loading, then unfreezes the shown frame on candidate %s",
  async (reason) => {
    vi.useFakeTimers();
    const base = ipc.getMockImplementation()!;
    let minted = 0;
    ipc.mockImplementation(async (command, args) =>
      command === "mint_frame"
        ? {
            ...record,
            frameId: `failed-recovery-${++minted}`,
            origin: `http://f-failed-recovery-${minted}.localhost:9321`,
            grant: { ...record.grant, frameId: `failed-recovery-${minted}` },
          }
        : base(command, args),
    );
    const renderHost = (refreshKey: number) => (
      <SlotDashboardHost
        slotId="dock"
        entry={pane}
        contentRoot="/ctx/content"
        visible
        refreshKey={refreshKey}
      />
    );
    let view!: ReturnType<typeof render>;
    await act(async () => {
      view = render(renderHost(1));
    });
    const outgoing = document.querySelector("iframe")!;
    let persisted = "Saved text";
    const draft = document.createElement("textarea");
    draft.value = persisted;
    const post = vi.fn();
    Object.defineProperty(outgoing, "contentWindow", {
      value: { postMessage: post },
      configurable: true,
    });
    act(() => {
      announce(outgoing, "codesense:ready");
      announce(outgoing, "codesense:theme-applied");
      announce(outgoing, "codesense:live");
    });
    fireEvent.change(draft, { target: { value: "Saved text plus unsaved final edit" } });
    await act(async () => {
      view.rerender(renderHost(2));
    });
    expect(document.querySelectorAll("iframe")).toHaveLength(1);
    const flush = post.mock.calls.find(([message]) => message.type === "codesense:flush")?.[0];
    expect(flush).toBeDefined();
    await act(async () => {
      persisted = draft.value;
      window.dispatchEvent(
        new MessageEvent("message", {
          source: outgoing.contentWindow,
          origin: new URL(outgoing.src).origin,
          data: { type: "codesense:flush-ack", token: record.token, id: flush.id },
        }),
      );
      await vi.advanceTimersByTimeAsync(150);
    });
    const candidate = document.querySelectorAll("iframe")[1];
    expect(candidate).toBeDefined();
    expect(persisted).toBe("Saved text plus unsaved final edit");
    act(() => {
      if (reason === "timeout") vi.advanceTimersByTime(29850);
      else
        window.dispatchEvent(
          new MessageEvent("message", {
            source: candidate.contentWindow,
            origin: new URL(candidate.src).origin,
            data: {
              type: "codesense:doc-error",
              token: record.token,
              message: "candidate failed before autosave",
            },
          }),
        );
    });
    expect(screen.getByTestId("slot-fallback-strip")).toBeInTheDocument();
    expect(outgoing.isConnected).toBe(true);
    expect(outgoing.closest("[data-pane-retained]")).not.toHaveAttribute("inert");
    expect(persisted).toBe("Saved text plus unsaved final edit");
    expect(post.mock.calls.some(([message]) => message.type === "codesense:teardown")).toBe(false);
  },
);

it("never delivers retirement messages from the pending iframe to a freshly minted document", async () => {
  const pending = ipc.getMockImplementation()!;
  let finishMint!: (value: typeof record) => void;
  ipc.mockImplementation((command, args) =>
    command === "mint_frame"
      ? new Promise((resolve) => {
          finishMint = resolve;
        })
      : pending(command, args),
  );
  render(host());
  await waitFor(() => expect(finishMint).toBeTypeOf("function"));
  await act(async () => {
    finishMint(record);
  });
  await waitFor(() => expect(document.querySelector("iframe")?.src).toContain(record.origin));
  const frame = document.querySelector("iframe")!;
  const postMessage = vi.fn();
  Object.defineProperty(frame, "contentWindow", { value: { postMessage }, configurable: true });
  act(() => announce(frame, "codesense:ready"));
  expect(postMessage.mock.calls.map(([message]) => message.type)).not.toContain(
    "codesense:teardown",
  );
  expect(postMessage.mock.calls.map(([message]) => message.type)).not.toContain("codesense:flush");
});

it("hands reload state only to the attempt's first document handshake", async () => {
  const snapshot = { draft: "first document" };
  render(
    <HtmlViewer
      filePath="/ctx/content/board.html"
      teamContentPath="/ctx/content"
      reloadState={Promise.resolve(snapshot)}
    />,
  );
  await waitFor(() => expect(document.querySelector("iframe")?.src).toContain(record.origin));
  const postMessage = vi.fn();
  const source = { postMessage } as unknown as Window;
  Object.defineProperty(document.querySelector("iframe")!, "contentWindow", {
    value: source,
    configurable: true,
  });
  const ready = () =>
    window.dispatchEvent(
      new MessageEvent("message", {
        source,
        origin: record.origin,
        data: { type: "codesense:ready", token: record.token },
      }),
    );
  const states = () =>
    postMessage.mock.calls
      .map(([m]) => m)
      .filter((m) => m.type === "codesense:reload-state")
      .map((m) => m.state);
  act(ready);
  await waitFor(() => expect(states()).toEqual([snapshot]));
  act(ready);
  await waitFor(() => expect(states()).toEqual([snapshot, null]));
});

type LifecycleLog = {
  slot: string;
  panelId?: string;
  transition: string;
  attempt: number;
  isLive: boolean;
  probing: boolean;
  frozen: boolean;
  retainedAttempt: number | null;
  failureKind: string | null;
  visible: boolean;
  geometry: unknown;
};

function lifecycleLogs(): LifecycleLog[] {
  return vi
    .mocked(logger.info)
    .mock.calls.filter(([tag]) => tag === "slot-lifecycle")
    .map(([, msg]) => JSON.parse(msg) as LifecycleLog);
}

it("logs hidden/revealed lifecycle transitions without pane content, paths or tokens", async () => {
  const { rerender } = render(host());
  await waitFor(() => expect(document.querySelector("iframe")?.src).toContain(record.origin));
  const frame = document.querySelector("iframe")!;
  act(() => announce(frame, "codesense:ready"));
  act(() => announce(frame, "codesense:theme-applied"));
  act(() => announce(frame, "codesense:live"));

  vi.mocked(logger.info).mockClear();

  rerender(host(false));
  await waitFor(() => {
    expect(lifecycleLogs().some((log) => log.transition === "hidden")).toBe(true);
  });

  rerender(host(true));
  await waitFor(() => {
    expect(lifecycleLogs().some((log) => log.transition === "revealed")).toBe(true);
  });

  const logs = lifecycleLogs();
  expect(logs.length).toBeGreaterThanOrEqual(2);
  for (const log of logs) {
    expect(log.slot).toBe("right-pane");
    expect(log).toHaveProperty("transition");
    expect(log).toHaveProperty("geometry");
    expect(typeof log.visible).toBe("boolean");
  }

  const messages = vi
    .mocked(logger.info)
    .mock.calls.filter(([tag]) => tag === "slot-lifecycle")
    .map(([, msg]) => msg);
  for (const msg of messages) {
    expect(msg).not.toContain(record.token);
    expect(msg).not.toContain(record.origin);
    expect(msg).not.toContain(pane.path);
    expect(msg).not.toContain("Agent 0");
  }
});
