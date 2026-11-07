// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, waitFor } from "@/test-utils";

// A tiny event bus standing in for the Tauri event channel: `listen(name, cb)` registers
// a handler and returns an unlisten; a test `emit`s to drive the hook.
const handlers = new Map<string, Set<(e: { payload: unknown }) => void>>();
const unlisten = vi.fn();
function emit(name: string, payload: unknown) {
  for (const cb of handlers.get(name) ?? []) cb({ payload });
}
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: (e: { payload: unknown }) => void) => {
    const set = handlers.get(name) ?? new Set();
    set.add(cb);
    handlers.set(name, set);
    return Promise.resolve(() => {
      set.delete(cb);
      unlisten();
    });
  },
}));

import { useWvEndorsementEvents } from "./use-wv-endorsement-events";

function Host(props: Parameters<typeof useWvEndorsementEvents>[0]) {
  useWvEndorsementEvents(props);
  return null;
}

describe("SW-WV-F5 · useWvEndorsementEvents (AC-7)", () => {
  beforeEach(() => {
    handlers.clear();
    unlisten.mockClear();
  });

  it("forwards a posted event to onPosted", async () => {
    const onPosted = vi.fn();
    render(<Host onPosted={onPosted} onRatified={vi.fn()} onRevoked={vi.fn()} />);
    await waitFor(() => expect(handlers.get("mcp:wv-assertion-posted")?.size).toBe(1));
    emit("mcp:wv-assertion-posted", {
      id: 1,
      sessionId: "s",
      claim: "c",
      channel: "slack",
      disposition: "pending",
    });
    expect(onPosted).toHaveBeenCalledWith(expect.objectContaining({ id: 1, claim: "c" }));
  });

  it("forwards ratified/revoked ids to their handlers", async () => {
    const onRatified = vi.fn();
    const onRevoked = vi.fn();
    render(<Host onPosted={vi.fn()} onRatified={onRatified} onRevoked={onRevoked} />);
    await waitFor(() => expect(handlers.get("mcp:wv-assertion-ratified")?.size).toBe(1));
    emit("mcp:wv-assertion-ratified", { id: 3, disposition: "ratified" });
    emit("mcp:wv-assertion-revoked", { id: 4, disposition: "revoked" });
    expect(onRatified).toHaveBeenCalledWith(3);
    expect(onRevoked).toHaveBeenCalledWith(4);
  });

  it("removes its listeners on unmount", async () => {
    const { unmount } = render(
      <Host onPosted={vi.fn()} onRatified={vi.fn()} onRevoked={vi.fn()} />,
    );
    await waitFor(() => expect(handlers.get("mcp:wv-assertion-posted")?.size).toBe(1));
    unmount();
    await waitFor(() => expect(unlisten).toHaveBeenCalled());
    expect(handlers.get("mcp:wv-assertion-posted")?.size ?? 0).toBe(0);
  });
});
