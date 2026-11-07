// SPDX-License-Identifier: MIT
/**
 * Evicted displays are collectable, not just unbooked: after eviction and a
 * forced GC, nothing still reaches the evicted xterm or its WebGL addon. The
 * committed leak faults (`CS_TEST_MUTATION`, or both by default as negative
 * controls) must keep them alive.
 */
import { setFlagsFromString } from "node:v8";
import { runInNewContext } from "node:vm";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { WebglAddon } from "@xterm/addon-webgl";

import { FakeEventBus, FakeTerminalServer } from "./__test-utils__/fake-terminal-server";
import { createTerminalManager, type TerminalManagerDeps } from "./terminal-manager";

const fake = vi.hoisted(() => ({
  bus: null as unknown as import("./__test-utils__/fake-terminal-server").FakeEventBus,
  server: null as unknown as import("./__test-utils__/fake-terminal-server").FakeTerminalServer,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, handler: (event: { payload: unknown }) => void) =>
    fake.bus.listen(name, handler),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: async () => {} }));
vi.mock(
  "@/features/viewer/lib/cli-workspace/transport/terminal-session-source",
  async (importOriginal) => ({
    ...(await importOriginal<
      typeof import("@/features/viewer/lib/cli-workspace/transport/terminal-session-source")
    >()),
    reserveTerminalStream: () => fake.server.reserve(),
    subscribeTerminal: (session: string, stream?: number) => fake.server.subscribe(session, stream),
    unsubscribeTerminal: (stream: number) => fake.server.unsubscribe(stream),
    writePty: async () => {},
    resizePty: (session: string, cols: number, rows: number) =>
      fake.server.resize(session, cols, rows),
  }),
);

// ES2021 globals the project's `lib` does not declare.
declare class WeakRef<T extends object> {
  constructor(target: T);
  deref(): T | undefined;
}
declare class FinalizationRegistry<T> {
  constructor(cleanup: (held: T) => void);
  register(target: object, held: T): void;
}

setFlagsFromString("--expose-gc");
const gc = runInNewContext("gc") as () => void;

const N = 4;
const macrotask = () => new Promise((resolve) => setTimeout(resolve, 0));

/** Build N displays (each shown, with a WebGL lease, then hidden), hold weak
 *  references to each xterm and addon, evict them all, and collect. How many
 *  of the held objects survived. */
async function survivorsAfterEviction(mutation?: TerminalManagerDeps["leakMutation"]) {
  const addons: WebglAddon[] = [];
  const m = createTerminalManager({
    leakMutation: mutation,
    createWebgl: () => {
      const addon = {
        activate: () => {},
        dispose: () => {},
        onContextLoss: () => ({ dispose: () => {} }),
      } as unknown as WebglAddon;
      addons.push(addon);
      return addon;
    },
  });
  const refs: WeakRef<object>[] = [];
  let finalized = 0;
  const registry = new FinalizationRegistry(() => finalized++);
  const ids = Array.from({ length: N }, (_, i) => `s${i}`);
  await (async () => {
    for (const id of ids) {
      fake.server.open(id);
      await m.ensure(id, { fontSizePx: 13 });
      const slot = document.createElement("div");
      document.body.appendChild(slot);
      m.attach(id, slot, { focus: false });
      for (let n = 0; n < 50; n++) fake.server.produce(id, `line ${n}\r\n`);
      await m.debugDump(id);
      await vi.waitFor(() => expect(addons.length).toBeGreaterThan(refs.length / 2));
      const held: object[] = [m.terminal(id)!, addons[addons.length - 1]];
      m.detach(id, slot);
      slot.remove();
      for (const object of held) {
        refs.push(new WeakRef(object));
        registry.register(object, null);
      }
    }
    for (const id of ids) expect(m.evict(id)).toBe(true);
  })();
  addons.length = 0;
  const heapBefore = process.memoryUsage().heapUsed;
  // No `deref()` until the end: a deref keeps its target alive for the rest
  // of the job, through any gc() in it.
  for (let i = 0; i < 3; i++) {
    gc();
    gc();
    await macrotask();
  }
  const heapAfter = process.memoryUsage().heapUsed;
  console.info(`evicted ${N} displays: heapUsed delta after gc ${heapAfter - heapBefore} B`);
  return {
    alive: refs.filter((r) => r.deref() !== undefined).length,
    finalized,
    held: refs.length,
  };
}

beforeEach(() => {
  fake.bus = new FakeEventBus();
  fake.server = new FakeTerminalServer(fake.bus);
});

afterEach(() => {
  document.body.innerHTML = "";
});

describe("evicted displays are released", () => {
  const forced = process.env.CS_TEST_MUTATION as TerminalManagerDeps["leakMutation"] | undefined;

  it("evicted_displays_are_collectable", async () => {
    const { alive, finalized, held } = await survivorsAfterEviction(forced);
    expect(held).toBe(N * 2);
    expect(alive).toBe(0);
    expect(finalized).toBe(held);
  }, 30_000);

  it.skipIf(forced !== undefined).each(["leak_evicted", "keep_lease"] as const)(
    "a leak fault keeps evicted displays alive: %s",
    async (mutation) => {
      const { alive } = await survivorsAfterEviction(mutation);
      expect(alive).toBeGreaterThan(0);
    },
    30_000,
  );
});
