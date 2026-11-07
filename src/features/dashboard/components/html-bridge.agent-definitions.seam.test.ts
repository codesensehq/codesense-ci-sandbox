// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import vm from "node:vm";
import { afterEach, expect, it, vi } from "vitest";
import { frameFixture } from "../../../../tests/support/fixtures/frame";
import { LiveHub, liveHub, type HubDelta, type HubSnapshot } from "../lib/live-hub";
import { runBridgeCommand } from "./html-bridge";

const source = vi.hoisted(() => ({ current: undefined as HubSnapshot | undefined }));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  agentDefinitionsPrepare: vi.fn(async () => source.current!),
  agentReadSessions: vi.fn(async () => []),
}));
let hub: LiveHub | undefined;
afterEach(() => {
  hub?.dispose();
  vi.restoreAllMocks();
});

it.each([false, true])(
  "delivers a real unused-agent save through the SDK with hidden=%s",
  async (hidden) => {
    const packets = JSON.parse(
      readFileSync("tests/fixtures/agent-definitions-save.json", "utf8"),
    ) as {
      pageBefore: HubSnapshot;
      pageAfter: HubSnapshot;
      deltas: HubDelta[];
    };
    source.current = packets.pageBefore;
    let subscriptionId: string | undefined;
    const events = new Map<string, (event: { payload: unknown }) => void>();
    const read = vi.fn(
      async (
        _topic: string,
        _key: string,
        _since?: number,
        options?: { subscriptionId: string },
      ) => {
        if (_topic === "sessions") return { topic: _topic, viewKey: _key, rev: 0, snapshot: [] };
        subscriptionId = options?.subscriptionId;
        return source.current!;
      },
    );
    hub = new LiveHub({
      read,
      listen: async (event, callback) => {
        events.set(event, callback);
        return () => {};
      },
      readUi: async () => ({}),
      writeUi: async (_, state) => state,
    });
    vi.spyOn(liveHub, "seed").mockImplementation((view) => hub!.seed(view));
    const frame = frameFixture({ documentPath: "/pane.html", contentRoot: "/", slot: "dock" });
    const parent = {};
    const listeners: Array<(event: unknown) => void> = [];
    const posts: unknown[] = [];
    const deliver = (data: unknown) => {
      posts.push(data);
      listeners.forEach((cb) =>
        cb({
          source: parent,
          origin: "tauri://localhost",
          data: { token: frame.token, ...(data as object) },
        }),
      );
    };
    const live = hub.attach(frame.frameId, "team:acme/backend", deliver, true);
    let visibility: (shown: boolean) => void = () => {};
    const invoke = vi.fn((verb: string, args: unknown) =>
      runBridgeCommand(verb, args, { frame, liveFrame: async () => live }),
    );
    const window = {
      parent,
      __codesenseProxy: { token: frame.token },
      codesense: {
        invoke,
        agents: () => invoke("agents", {}),
        onVisibility: (cb: typeof visibility) => {
          visibility = cb;
          cb(true);
          return () => {};
        },
        onInboxChanged: () => () => {},
      },
      addEventListener: (_: string, cb: (event: unknown) => void) => listeners.push(cb),
    };
    vm.runInNewContext(readFileSync("src-tauri/src/http/routes/live_hub.js", "utf8"), {
      window,
      queueMicrotask,
    });
    const api = window.codesense as typeof window.codesense & {
      onAgentsChanged: (cb: () => void) => () => void;
    };
    const changed = vi.fn();
    const off = api.onAgentsChanged(changed);
    await vi.waitFor(() => expect(changed).toHaveBeenCalled());
    changed.mockClear();
    const readsBeforeChange = read.mock.calls.length;
    if (hidden) {
      visibility(false);
      live.visible(false);
    }
    source.current = packets.pageAfter;
    const delta = packets.deltas.find((d) => d.viewKey === packets.pageAfter.viewKey)!;
    events.get("codesense:subscription-delta")!({
      payload: { subscriptionId, baseRev: packets.pageBefore.rev, delta },
    });
    if (hidden) {
      expect(changed).not.toHaveBeenCalled();
      expect(read).toHaveBeenCalledTimes(readsBeforeChange);
      expect(posts.filter((p) => (p as { type?: string }).type === "codesense:stale")).toHaveLength(
        1,
      );
      live.visible(true);
      visibility(true);
    }
    await vi.waitFor(() => expect(changed).toHaveBeenCalled());
    const rows = (await api.agents()) as Array<Record<string, unknown>>;
    expect(rows.find((row) => row.key === "acme/api/backend/unused-live")?.name).toBe(
      "Saved without any session",
    );
    for (const row of rows)
      expect(Object.keys(row).sort()).toEqual([
        "avatar",
        "description",
        "key",
        "name",
        "runningNow",
        "runtime",
        "scope",
        "team",
      ]);
    expect(
      invoke.mock.calls.every(([verb]) =>
        ["agents", "sessions", "subscribe", "unsubscribe"].includes(verb),
      ),
    ).toBe(true);
    off();
  },
);
