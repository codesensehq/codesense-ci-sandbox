// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import vm from "node:vm";
import { viewerScript } from "../../../../tests/support/page-shim";
import { frameFixture } from "../../../../tests/support/fixtures/frame";
import { HostMenuContext } from "@/features/shell/host-menu-context";
import { beforeEach, afterEach, describe, it, expect, vi } from "vitest";
import { act, render, waitFor, cleanup } from "@testing-library/react";
const { ipc } = vi.hoisted(() => ({ ipc: vi.fn() }));
vi.mock("@tauri-apps/api/core", async (original) => ({
  ...(await original<typeof import("@tauri-apps/api/core")>()),
  invoke: (command: string, args: unknown) => ipc(command, args),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}), emit: vi.fn() }));
import { HtmlViewer } from "./html-viewer";
import { FRAME_PRESENTATION_UNAVAILABLE } from "../lib/frame-presentation";
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
    vi.fn(async () => ({ status: 200 })),
  );
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});
async function mount() {
  render(<HtmlViewer filePath="/ctx/content/board.html" teamContentPath="/ctx/content" />);
  await waitFor(() => expect(document.querySelector("iframe")?.src).toContain(record.origin));
  return document.querySelector("iframe")!;
}
describe("frame identity at the actual HtmlViewer bridge seam", () => {
  it("uses a distinct origin, sends no secret in its URL, and releases on teardown", async () => {
    const frame = await mount();
    expect(new URL(frame.src).origin).toBe(record.origin);
    expect(frame.src).not.toMatch(/secret|[?&]cap=/);
    cleanup();
    await waitFor(() => expect(ipc).toHaveBeenCalledWith("release_frame", { frameId: "fresh" }));
  });
  it("denies a token presented by the wrong origin before dispatch", async () => {
    const frame = await mount();
    const postMessage = vi.fn();
    const source = { postMessage } as unknown as Window;
    Object.defineProperty(frame, "contentWindow", { value: source, configurable: true });
    window.dispatchEvent(
      new MessageEvent("message", {
        source,
        origin: "http://f-other.localhost:9321",
        data: { type: "codesense:invoke", id: 1, command: "prefs", token: record.token },
      }),
    );
    await waitFor(() => expect(postMessage).toHaveBeenCalled());
    expect(postMessage.mock.calls[0][0]).toMatchObject({
      ok: false,
      error: expect.stringMatching(/^denied:/),
    });
    expect(postMessage.mock.calls[0][1]).toBe(record.origin);
  });
  it("authenticates LiveHub subscribe before admission and derives its view from the resolved grant", async () => {
    const base = ipc.getMockImplementation()!;
    const grant = { ...record.grant, scope: "team", team: { org: "org", team: "team" } };
    ipc.mockImplementation(async (command, args) => {
      if (command === "resolve_frame_grant") return grant;
      if (command === "live_hub_view_key") return "team:org/team";
      if (command === "verb_dispatch") return { status: "admit", callId: "subscribe-call" };
      if (command === "live_hub_subscribe")
        return { topic: "sessions", viewKey: "team:org/team", rev: 0, snapshot: [] };
      return base(command, args);
    });
    const frame = await mount();
    const postMessage = vi.fn();
    const source = { postMessage } as unknown as Window;
    Object.defineProperty(frame, "contentWindow", { value: source, configurable: true });
    const send = (origin: string, token: string) =>
      window.dispatchEvent(
        new MessageEvent("message", {
          source,
          origin,
          data: {
            type: "codesense:invoke",
            id: 12,
            command: "subscribe",
            token,
            args: { topic: "sessions", viewKey: "all" },
          },
        }),
      );
    for (const [origin, token] of [
      ["http://f-other.localhost:9321", record.token],
      [record.origin, "other-frame"],
    ]) {
      send(origin, token);
      await waitFor(() => expect(postMessage).toHaveBeenCalled());
      expect(postMessage.mock.calls.at(-1)?.[0]).toMatchObject({
        ok: false,
        error: expect.stringMatching(/^denied:/),
      });
      expect(
        ipc.mock.calls.some(([cmd]) => cmd === "verb_dispatch" || cmd === "live_hub_subscribe"),
      ).toBe(false);
      postMessage.mockClear();
    }
    expect(ipc).toHaveBeenCalledWith("live_hub_view_key", {
      origin: record.origin,
      token: record.token,
    });
    send(record.origin, record.token);
    await waitFor(() =>
      expect(ipc).toHaveBeenCalledWith(
        "live_hub_subscribe",
        expect.objectContaining({ viewKey: "team:org/team" }),
      ),
    );
    expect(ipc).toHaveBeenCalledWith(
      "verb_dispatch",
      expect.objectContaining({ caller: { ...grant, kind: record.kind }, verb: "subscribe" }),
    );
    await waitFor(() => expect(postMessage.mock.calls.at(-1)?.[0]).toMatchObject({ ok: true }));
    expect(postMessage.mock.calls.at(-1)?.[1]).toBe(record.origin);
  });
});

it("rejects a hosted message child's menu when its parent element is missing", async () => {
  const child = frameFixture(
    {
      parentFrame: "parent",
      documentPath: "/ctx/content/message.html",
      contentRoot: "/ctx/content",
      slot: "message",
      message: { org: "org", team: "team", messageId: 7 },
    },
    9321,
  );
  const base = ipc.getMockImplementation()!;
  ipc.mockImplementation(async (command, args) => {
    if (command === "resolve_frame_grant") return child.grant;
    if (command === "verb_dispatch") return { status: "admit", callId: "menu-call" };
    return base(command, args);
  });
  const show = vi.fn().mockResolvedValue({ closed: true });
  await act(async () => {
    render(
      <HostMenuContext.Provider value={show}>
        <HtmlViewer
          filePath={child.documentPath}
          teamContentPath={child.contentRoot}
          hostedFrame={child}
          parentElement={null}
        />
      </HostMenuContext.Provider>,
    );
  });
  expect(document.querySelector("iframe")).toBeNull();
  const postMessage = vi.fn();
  window.dispatchEvent(
    new MessageEvent("message", {
      source: { postMessage } as unknown as Window,
      origin: child.origin,
      data: {
        type: "codesense:invoke",
        id: 33,
        command: "showMenu",
        token: child.token,
        args: {
          anchorRect: { x: 0, y: 0, width: 20, height: 20 },
          items: [{ id: "inspect", label: "Inspect" }],
        },
      },
    }),
  );
  await waitFor(() =>
    expect(postMessage).toHaveBeenCalledWith(
      {
        type: "codesense:invoke-result",
        id: 33,
        ok: false,
        error: FRAME_PRESENTATION_UNAVAILABLE,
      },
      child.origin,
    ),
  );
  expect(ipc).toHaveBeenCalledWith(
    "verb_dispatch",
    expect.objectContaining({ caller: { ...child.grant, kind: child.kind }, verb: "showMenu" }),
  );
  expect(show).not.toHaveBeenCalled();
  expect(document.querySelector('[role="menu"]')).toBeNull();
});

it.each([false, true])(
  "keeps page calls held until the host key advances (lookup fails=%s)",
  async (fails) => {
    const { useOrgStore } = await import("@/shared/stores/org-store");
    const { liveHub } = await import("../lib/live-hub");
    const previous = useOrgStore.getState();
    const dock = frameFixture(
      { documentPath: "/ctx/content/dock.html", contentRoot: "/ctx/content", slot: "dock" },
      9321,
    );
    dock.grant = { ...dock.grant, scope: "team", team: { org: "org", team: "a" } };
    let current = dock;
    let finishKey!: (key: string) => void;
    let failKey!: (error: Error) => void;
    const base = ipc.getMockImplementation()!;
    ipc.mockImplementation(async (command, args) => {
      if (command === "mint_frame") return dock;
      if (command === "resolve_frame_grant") return current.grant;
      if (command === "rebind_frame_team") {
        current = {
          ...dock,
          token: "new-team-token",
          grant: { ...dock.grant, team: { org: "org", team: "b" } },
        };
        return current;
      }
      if (command === "live_hub_view_key") {
        if ((args as { token: string }).token === "new-team-token")
          return new Promise<string>((resolve, reject) => {
            finishKey = resolve;
            failKey = reject;
          });
        return "team:org/a";
      }
      if (command === "verb_dispatch") return { status: "admit", callId: "held-call" };
      if (command === "live_hub_subscribe") {
        const a = args as { topic: string; viewKey: string };
        return { topic: a.topic, viewKey: a.viewKey, rev: 0, snapshot: [{ id: a.viewKey }] };
      }
      return base(command, args);
    });
    const problem = vi.fn();
    try {
      act(() => useOrgStore.setState({ activeOrgSlug: "org", activeTeamName: "a" }));
      render(
        <HtmlViewer
          filePath={dock.documentPath}
          teamContentPath={dock.contentRoot}
          pane={{
            contentRoot: dock.contentRoot,
            panesDir: "/ctx/content",
            fileName: "dock.html",
            slotId: "dock",
          }}
          onDocProblem={problem}
        />,
      );
      await waitFor(() => expect(document.querySelector("iframe")?.src).toContain(dock.origin));
      await waitFor(() =>
        expect(ipc).toHaveBeenCalledWith(
          "live_hub_view_key",
          expect.objectContaining({ token: dock.token }),
        ),
      );
      const frame = document.querySelector("iframe")!;
      let token = dock.token;
      const source = { postMessage: vi.fn() };
      Object.defineProperty(frame, "contentWindow", { value: source, configurable: true });
      const send = (data: Record<string, unknown>) =>
        window.dispatchEvent(
          new MessageEvent("message", {
            source: source as unknown as Window,
            origin: dock.origin,
            data: { token, ...data },
          }),
        );
      source.postMessage.mockImplementation((message) => {
        if (message.type === "codesense:rebind-pending")
          queueMicrotask(() => send({ type: "codesense:rebind-ack" }));
        if (message.type === "codesense:rebind") {
          token = message.token;
          send({
            type: "codesense:invoke",
            id: 45,
            command: "subscribe",
            args: { topic: "agentList", order: "status" },
          });
        }
      });
      await act(async () => {
        send({ type: "codesense:ready" });
      });
      act(() => useOrgStore.setState({ activeTeamName: "b" }));
      await waitFor(() => expect(finishKey).toBeTypeOf("function"));
      const releasedEarly = source.postMessage.mock.calls.some(
        ([message]) => message.type === "codesense:rebind",
      );
      await act(async () => {
        if (fails) failKey(new Error("key lookup failed"));
        else finishKey("team:org/b");
      });
      if (fails) {
        await waitFor(() => expect(problem).toHaveBeenCalled());
        expect(
          source.postMessage.mock.calls.some(([message]) => message.type === "codesense:rebind"),
        ).toBe(false);
        expect(ipc.mock.calls.some(([command]) => command === "live_hub_subscribe")).toBe(false);
      } else {
        await waitFor(() =>
          expect(source.postMessage).toHaveBeenCalledWith(
            expect.objectContaining({
              type: "codesense:invoke-result",
              id: 45,
              ok: true,
              data: expect.objectContaining({ viewKey: "team:org/b|status" }),
            }),
            dock.origin,
          ),
        );
        expect(
          ipc.mock.calls
            .filter(([command]) => command === "live_hub_subscribe")
            .map(([, args]) => (args as { viewKey: string }).viewKey),
        ).toEqual(["team:org/b|status"]);
      }
      expect(releasedEarly).toBe(false);
    } finally {
      cleanup();
      liveHub.dispose();
      act(() =>
        useOrgStore.setState({
          activeOrgSlug: previous.activeOrgSlug,
          activeTeamName: previous.activeTeamName,
        }),
      );
    }
  },
);

it.each([false, true])(
  "keeps plain, status and inbox live through token rotation (retired first paint=%s)",
  async (retirePlain) => {
    const { useOrgStore } = await import("@/shared/stores/org-store");
    const { liveHub } = await import("../lib/live-hub");
    const { listen } = await import("@tauri-apps/api/event");
    const previous = useOrgStore.getState();
    const events = new Map<string, (event: { payload: unknown }) => void>();
    vi.mocked(listen).mockImplementation(async (name, callback) => {
      events.set(name, callback as (event: { payload: unknown }) => void);
      return () => {
        events.delete(name);
      };
    });
    const dock = frameFixture(
      { documentPath: "/ctx/content/dock.html", contentRoot: "/ctx/content", slot: "dock" },
      9321,
    );
    dock.grant = { ...dock.grant, scope: "team", team: { org: "org", team: "a" } };
    let current = dock;
    const base = ipc.getMockImplementation()!;
    ipc.mockImplementation(async (command, args) => {
      if (command === "mint_frame") return dock;
      if (command === "resolve_frame_grant") return current.grant;
      if (command === "rebind_frame_team") {
        current = {
          ...dock,
          token: "new-team-token",
          grant: { ...dock.grant, team: { org: "org", team: "b" } },
        };
        return current;
      }
      if (command === "live_hub_view_key")
        return current.grant.team?.team === "b" ? "team:org/b" : "team:org/a";
      if (command === "verb_dispatch") return { status: "admit", callId: "existing-subscriber" };
      if (command === "live_hub_subscribe") {
        const a = args as { topic: string; viewKey: string };
        return {
          topic: a.topic,
          viewKey: a.viewKey,
          rev: 0,
          snapshot: [{ id: "row", label: a.viewKey }],
        };
      }
      return base(command, args);
    });
    try {
      act(() => useOrgStore.setState({ activeOrgSlug: "org", activeTeamName: "a" }));
      render(
        <HtmlViewer
          filePath={dock.documentPath}
          teamContentPath={dock.contentRoot}
          pane={{
            contentRoot: dock.contentRoot,
            panesDir: "/ctx/content",
            fileName: "dock.html",
            slotId: "dock",
          }}
        />,
      );
      await waitFor(() => expect(document.querySelector("iframe")?.src).toContain(dock.origin));
      await waitFor(() => expect(ipc).toHaveBeenCalledWith("live_hub_view_key", expect.anything()));
      const listeners: Array<(event: unknown) => void> = [];
      const source = {
        __codesenseProxy: { token: dock.token },
        __codesenseFirstPaint: {
          topic: "agentList",
          viewKey: "team:org/a",
          rev: 0,
          snapshot: [{ id: "row", label: "team:org/a" }],
        },
        parent: {
          postMessage: (data: unknown) =>
            queueMicrotask(() =>
              window.dispatchEvent(
                new MessageEvent("message", {
                  source: source as unknown as Window,
                  origin: dock.origin,
                  data,
                }),
              ),
            ),
        },
        addEventListener: (type: string, callback: (event: unknown) => void) => {
          if (type === "message") listeners.push(callback);
        },
        postMessage: vi.fn((data: unknown) =>
          queueMicrotask(() =>
            listeners.forEach((callback) =>
              callback({ source: source.parent, origin: "tauri://localhost", data }),
            ),
          ),
        ),
      };
      Object.defineProperty(document.querySelector("iframe")!, "contentWindow", {
        value: source,
        configurable: true,
      });
      vm.runInNewContext(
        viewerScript("LINK_INTERCEPTOR_JS") +
          "\n" +
          readFileSync("src-tauri/src/http/routes/live_hub.js", "utf8"),
        {
          window: source,
          document: document.implementation.createHTMLDocument(),
          queueMicrotask,
          setTimeout,
          clearTimeout,
          console,
        },
      );
      const api = (
        source as unknown as {
          codesense: {
            subscribe: (
              topic: string,
              options: { order?: "status"; onChange: ReturnType<typeof vi.fn> },
            ) => Promise<unknown>;
            unsubscribe: (
              topic: string,
              callback: ReturnType<typeof vi.fn>,
              options?: { order?: "status" },
            ) => void;
          };
        }
      ).codesense;
      const plain = vi.fn(),
        status = vi.fn(),
        inbox = vi.fn();
      await act(async () => {
        await Promise.all([
          api.subscribe("agentList", { onChange: plain }),
          api.subscribe("agentList", { order: "status", onChange: status }),
          api.subscribe("inbox", { onChange: inbox }),
        ]);
      });
      expect([plain, status, inbox].map((cb) => cb.mock.lastCall?.[0].viewKey)).toEqual([
        "team:org/a",
        "team:org/a|status",
        "team:org/a",
      ]);
      if (retirePlain) {
        api.unsubscribe("agentList", plain, {});
        plain.mockClear();
      }
      act(() => useOrgStore.setState({ activeTeamName: "b" }));
      await waitFor(() =>
        expect([status, inbox].map((cb) => cb.mock.lastCall?.[0].viewKey)).toEqual([
          "team:org/b|status",
          "team:org/b",
        ]),
      );
      if (retirePlain) {
        await act(async () => {
          const pending = api.subscribe("agentList", { onChange: plain });
          // A later Team/Manager selection must never synchronously paint the retired seed.
          expect(plain.mock.calls.every(([value]) => value.viewKey === "team:org/b")).toBe(true);
          await pending;
        });
      }
      expect([plain, status, inbox].map((cb) => cb.mock.lastCall?.[0].viewKey)).toEqual([
        "team:org/b",
        "team:org/b|status",
        "team:org/b",
      ]);
      if (retirePlain)
        expect(plain.mock.calls.every(([value]) => value.viewKey === "team:org/b")).toBe(true);
      expect(source.__codesenseProxy.token).toBe("new-team-token");
      const release = source.postMessage.mock.calls.findIndex(
        ([data]) => (data as { type: string }).type === "codesense:rebind",
      );
      const resync = source.postMessage.mock.calls.findIndex(
        ([data]) => (data as { type: string }).type === "codesense:resync",
      );
      expect(release).toBeGreaterThanOrEqual(0);
      expect(resync).toBeGreaterThan(release);
      await act(async () => {
        for (const [topic, key] of [
          ["agentList", "team:org/b"],
          ["agentList", "team:org/b|status"],
          ["inbox", "team:org/b"],
        ]) {
          const options = ipc.mock.calls.find(
            ([command, args]) =>
              command === "live_hub_subscribe" && args.topic === topic && args.viewKey === key,
          )?.[1];
          const delta = {
            topic,
            viewKey: key,
            rev: 1,
            upserts: [{ id: "row", label: "live " + key }],
            removes: [],
          };
          if (options?.subscriptionId)
            events.get("codesense:subscription-delta")!({
              payload: { subscriptionId: options.subscriptionId, baseRev: 0, delta },
            });
          else events.get("codesense:delta")!({ payload: delta });
        }
      });
      await waitFor(() =>
        expect([plain, status, inbox].map((cb) => cb.mock.lastCall?.[0].snapshot[0].label)).toEqual(
          ["live team:org/b", "live team:org/b|status", "live team:org/b"],
        ),
      );
    } finally {
      cleanup();
      liveHub.dispose();
      vi.mocked(listen).mockImplementation(async () => () => {});
      act(() =>
        useOrgStore.setState({
          activeOrgSlug: previous.activeOrgSlug,
          activeTeamName: previous.activeTeamName,
        }),
      );
    }
  },
);
