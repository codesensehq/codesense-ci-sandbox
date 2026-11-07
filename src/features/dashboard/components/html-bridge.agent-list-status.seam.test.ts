// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import vm from "node:vm";
import { afterEach, expect, it, vi } from "vitest";
import { frameFixture } from "../../../../tests/support/fixtures/frame";
import { runBridgeCommand } from "./html-bridge";
import { LiveHub, type HubReply } from "../lib/live-hub";
const grantedKey = vi.hoisted(() => ({ value: "team:acme/a" }));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  liveHubViewKey: vi.fn(async () => grantedKey.value),
}));
let hub: LiveHub;
afterEach(() => hub?.dispose());

it("keeps both agent orders independent through the shim, bridge and host, including rebind and unsubscribe", async () => {
  grantedKey.value = "team:acme/a";
  const events = new Map<string, (event: { payload: unknown }) => void>();
  const read = vi.fn(
    async (topic: string, viewKey: string, sinceRev?: number): Promise<HubReply> => {
      const rev = viewKey.startsWith("team:acme/b") ? 1 : 0;
      return {
        topic,
        viewKey,
        rev,
        ...(sinceRev === rev
          ? { deltas: [] }
          : { snapshot: [{ id: "agent", order: 0, label: viewKey }] }),
      };
    },
  );
  const unsubscribe = vi.fn(async () => {});
  hub = new LiveHub({
    read,
    unsubscribe,
    listen: async (event, cb) => {
      events.set(event, cb);
      return () => {};
    },
    readUi: async () => ({}),
    writeUi: async (_, state) => state,
  });
  const frame = frameFixture({ documentPath: "/pane.html", contentRoot: "/", slot: "dock" });
  const listeners: Array<(event: unknown) => void> = [];
  const parent = {};
  const deliver = (data: unknown) =>
    listeners.forEach((cb) =>
      cb({
        source: parent,
        origin: "tauri://localhost",
        data: { token: frame.token, ...(data as object) },
      }),
    );
  const live = hub.attach(frame.frameId, grantedKey.value, deliver, true);
  const invoke = vi.fn((verb: string, args: unknown) =>
    runBridgeCommand(verb, args, { frame, liveFrame: async () => live }),
  );
  const window = {
    parent,
    __codesenseProxy: { token: frame.token },
    codesense: {
      invoke,
      onVisibility: (cb: (v: boolean) => void) => {
        cb(true);
        return () => {};
      },
      onInboxChanged: () => () => {},
      onAgentsChanged: () => () => {},
    },
    addEventListener: (_: string, cb: (event: unknown) => void) => listeners.push(cb),
  };
  vm.runInNewContext(readFileSync("src-tauri/src/http/routes/live_hub.js", "utf8"), {
    window,
    queueMicrotask,
  });
  const api = window.codesense as any;
  const plain = vi.fn(),
    status = vi.fn();
  await api.subscribe("agentList", { onChange: plain });
  await api.subscribe("agentList", { order: "status", onChange: status });
  expect(plain.mock.lastCall?.[0].viewKey).toBe("team:acme/a");
  expect(status.mock.lastCall?.[0].viewKey).toBe("team:acme/a|status");
  const push = async (key: string, rev: number, label: string) => {
    const options = read.mock.calls.find(([, view]) => view === key) as unknown[];
    const subscriptionId = (options?.[3] as { subscriptionId?: string })?.subscriptionId;
    const delta = {
      topic: "agentList",
      viewKey: key,
      rev,
      upserts: [{ id: "agent", order: 0, label }],
      removes: [],
    };
    if (subscriptionId)
      events.get("codesense:subscription-delta")!({
        payload: { subscriptionId, baseRev: rev - 1, delta },
      });
    else hub.receive(delta);
    await new Promise((resolve) => setTimeout(resolve, 10));
  };
  await push("team:acme/a", 1, "plain changed");
  await push("team:acme/a|status", 1, "status changed");
  expect(plain.mock.lastCall?.[0].snapshot[0].label).toBe("plain changed");
  expect(status.mock.lastCall?.[0].snapshot[0].label).toBe("status changed");
  // A new grant moves the derived status key and the plain view together.
  grantedKey.value = "team:acme/b";
  live.rekey(grantedKey.value);
  await vi.waitFor(() => {
    expect(plain.mock.lastCall?.[0].viewKey).toBe("team:acme/b");
    expect(status.mock.lastCall?.[0].viewKey).toBe("team:acme/b|status");
  });
  api.unsubscribe("agentList", status, { order: "status" });
  const count = status.mock.calls.length;
  await push("team:acme/b", 2, "still plain");
  expect(plain.mock.lastCall?.[0].snapshot[0].label).toBe("still plain");
  expect(status).toHaveBeenCalledTimes(count);
  expect(unsubscribe).toHaveBeenCalled();
  await expect(api.subscribe("agentList", { order: "name" })).rejects.toThrow("order");
});

it.each(["hidden", "bridge"])(
  "derives Status from the current attached frame after a %s wait",
  async (wait) => {
    grantedKey.value = "team:acme/a";
    const read = vi.fn(async (topic: string, viewKey: string) => ({
      topic,
      viewKey,
      rev: 0,
      snapshot: [{ id: viewKey }],
    }));
    hub = new LiveHub({
      read,
      listen: async () => () => {},
      readUi: async () => ({}),
      writeUi: async (_, state) => state,
    });
    const live = hub.attach("scope-wait", grantedKey.value, vi.fn(), wait !== "hidden");
    const frame = frameFixture({ documentPath: "/pane.html", contentRoot: "/", slot: "dock" });
    let release!: (frame: typeof live) => void;
    const pending = runBridgeCommand(
      "subscribe",
      { topic: "agentList", order: "status" },
      {
        frame,
        liveFrame: () =>
          wait === "bridge"
            ? new Promise((resolve) => {
                release = resolve;
              })
            : Promise.resolve(live),
      },
    );
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(read).not.toHaveBeenCalled();
    live.rekey("team:acme/b");
    live.visible(true);
    if (wait === "bridge") release(live);
    const result = (await pending) as HubReply;
    expect(result.viewKey).toBe("team:acme/b|status");
    expect(read.mock.calls.map(([, key]) => key)).toEqual(["team:acme/b|status"]);
  },
);
