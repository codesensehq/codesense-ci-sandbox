// SPDX-License-Identifier: MIT
import { expect, it, vi, beforeEach } from "vitest";
import { renderHook, waitFor, act } from "@testing-library/react";
import type { HubSnapshot } from "@/features/dashboard/lib/live-hub";
const hub = vi.hoisted(() => ({ watch: vi.fn(), snapshot: vi.fn(), stop: vi.fn() }));
vi.mock("@/features/dashboard/lib/live-hub", () => ({ liveHub: hub }));
import { useInboxProjectionStore } from "./use-inbox-projection";
import { useInboxSessions, useInboxSessionsStore } from "./use-inbox-sessions";
beforeEach(() => {
  vi.clearAllMocks();
  useInboxSessionsStore.setState({ sessions: null });
  useInboxProjectionStore.setState({ counts: [] });
  hub.watch.mockReturnValue(hub.stop);
});
it("shares one projection subscription across native inbox surfaces", async () => {
  let update!: (s: HubSnapshot) => void;
  hub.watch.mockImplementation((_t, _v, cb) => {
    if (_t === "sessions") update = cb;
    return hub.stop;
  });
  const a = renderHook(() => useInboxSessions());
  const b = renderHook(() => useInboxSessions());
  expect(hub.watch).toHaveBeenCalledTimes(2);
  act(() =>
    update({
      topic: "sessions",
      viewKey: "all",
      rev: 1,
      snapshot: [{ id: "a", handle: "a", state: "needs-you" }],
    }),
  );
  await waitFor(() => expect(a.result.current?.[0].state).toBe("needs-you"));
  expect(b.result.current).toEqual(a.result.current);
  a.unmount();
  expect(hub.stop).not.toHaveBeenCalled();
  b.unmount();
  expect(hub.stop).toHaveBeenCalledTimes(2);
});
it("manual load reads the shared snapshot", async () => {
  hub.snapshot.mockResolvedValue({
    topic: "sessions",
    viewKey: "all",
    rev: 2,
    snapshot: [{ id: "b", handle: "b" }],
  });
  await useInboxSessionsStore.getState().load();
  expect(useInboxSessionsStore.getState().sessions?.[0].handle).toBe("b");
});

it("updates native rail sessions from each shared inbox revision", async () => {
  const updates = new Map<string, (value: HubSnapshot) => void>();
  hub.watch.mockImplementation((topic, _view, callback) => {
    updates.set(topic, callback);
    return hub.stop;
  });
  const rail = renderHook(() => useInboxSessions());
  await act(async () => {});
  const team = { org: "acme", team: "a" };
  act(() =>
    updates.get("sessions")!({
      topic: "sessions",
      viewKey: "all",
      rev: 1,
      snapshot: [{ id: "a", handle: "a", team }],
    }),
  );
  const publish = (rev: number, unread: number) =>
    updates.get("inbox")!({
      topic: "inbox",
      viewKey: "all",
      rev,
      snapshot: [
        {
          id: "counts:a",
          rowType: "counts",
          team,
          humanUnread: unread,
          recipients: { a: unread },
          needsYouBySender: {},
          sessions: {
            a: {
              unread,
              groupUnread: unread,
              groupReadId: rev,
              mailNotice: unread ? "pending" : null,
            },
          },
        },
      ],
    });
  act(() => publish(1, 2));
  expect(rail.result.current?.[0]).toMatchObject({
    unread: 2,
    groupUnread: 2,
    mailNotice: "pending",
  });
  act(() => publish(2, 0));
  expect(rail.result.current?.[0]).toMatchObject({
    unread: 0,
    groupUnread: 0,
    groupReadId: 2,
    mailNotice: null,
  });
  expect(hub.watch.mock.calls.filter(([topic]) => topic === "inbox")).toHaveLength(1);
  rail.unmount();
});
