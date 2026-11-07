// SPDX-License-Identifier: MIT
import { afterEach, expect, it, vi } from "vitest";
import { warmScopes } from "@/shared/lib/warm-scopes";
import {
  registerFrame,
  flushFrame,
  flushFrameWrites,
  setFrameFrozen,
  flushAll,
  acknowledgeFlush,
  retireAfterFlush,
  markFrameLive,
  postDashboardCommand,
} from "./frame-lifecycle";
const cleanups: Array<() => void> = [];
afterEach(() => {
  cleanups.splice(0).forEach((off) => off());
  vi.useRealTimers();
  vi.restoreAllMocks();
});
function frame(id = "a") {
  const post = vi.fn();
  const value = { frameId: id, dashboard: id, post, hide: vi.fn() };
  cleanups.push(registerFrame(value));
  return value;
}
it("waits for ack before teardown and deduplicates concurrent close paths", async () => {
  vi.useFakeTimers();
  const f = frame();
  const a = flushFrame(f.frameId);
  const b = flushFrame(f.frameId);
  expect(f.post.mock.calls.map(([m]) => m.type)).toEqual(["codesense:flush"]);
  acknowledgeFlush(f.frameId, f.post.mock.calls[0][0].id);
  await Promise.all([a, b]);
  expect(f.post.mock.calls.map(([m]) => m.type)).toEqual(["codesense:flush", "codesense:teardown"]);
});
it("times out at 300ms, logs once, and retires only after flushing", async () => {
  vi.useFakeTimers();
  const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
  const f = frame();
  const remove = vi.fn();
  const pending = retireAfterFlush((v) => v.frameId === f.frameId, remove);
  expect(f.hide).toHaveBeenCalledOnce();
  await vi.advanceTimersByTimeAsync(299);
  expect(remove).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(1);
  await pending;
  await flushFrame(f.frameId);
  expect(remove).toHaveBeenCalledOnce();
  expect(warn).toHaveBeenCalledExactlyOnceWith("frame flush timed out: a");
});
it("flushes five frames in parallel under a shared quit deadline, including an already pending flush", async () => {
  vi.useFakeTimers();
  const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
  const frames = Array.from({ length: 5 }, (_, i) => frame(String(i)));
  void flushFrame("0", 1000);
  const done = vi.fn();
  const pending = flushAll(500).then(done);
  for (const f of frames.slice(1)) acknowledgeFlush(f.frameId, f.post.mock.calls[0][0].id);
  await vi.advanceTimersByTimeAsync(499);
  expect(done).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(1);
  await pending;
  expect(done).toHaveBeenCalledOnce();
  expect(warn).toHaveBeenCalledOnce();
  expect(frames.every((f) => f.post.mock.calls.at(-1)?.[0].type === "codesense:teardown")).toBe(
    true,
  );
});

it("keeps a starting pane command past ten seconds and delivers it before the thirty-second deadline", () => {
  vi.useFakeTimers();
  const f = {
    frameId: "slow-pane",
    dashboard: "dashboard:slow",
    slotId: "dock",
    post: vi.fn(),
    hide: vi.fn(),
  };
  cleanups.push(registerFrame(f));
  postDashboardCommand(f.dashboard, "dock", {
    id: "search",
    label: "Search",
    dashboardTitle: "Slow",
  });
  vi.advanceTimersByTime(15000);
  markFrameLive(f.frameId);
  expect(f.post).toHaveBeenCalledWith(
    expect.objectContaining({ type: "codesense:command", id: "search" }),
  );
});

it("routes commands to the promoted frame while the outgoing frame flushes", async () => {
  const old = {
    frameId: "outgoing",
    dashboard: "dashboard:recover",
    slotId: "dock",
    post: vi.fn(),
    hide: vi.fn(),
  };
  const next = { ...old, frameId: "incoming", post: vi.fn(), hide: vi.fn() };
  cleanups.push(registerFrame(old), registerFrame(next));
  markFrameLive(old.frameId);
  markFrameLive(next.frameId);
  const pending = retireAfterFlush(
    (f) => f.frameId === old.frameId,
    () => {},
  );
  postDashboardCommand(old.dashboard, "dock", {
    id: "search",
    label: "Search",
    dashboardTitle: "Recover",
  });
  expect(next.post).toHaveBeenCalledWith(expect.objectContaining({ type: "codesense:command" }));
  expect(old.post).not.toHaveBeenCalledWith(expect.objectContaining({ type: "codesense:command" }));
  acknowledgeFlush(old.frameId, old.post.mock.calls[0][0].id);
  await pending;
});

it("a command queued before mount inherits the mounted attempt deadline instead of its old backstop", () => {
  vi.useFakeTimers();
  const f = {
    frameId: "late-mount",
    dashboard: "dashboard:late",
    slotId: "dock",
    post: vi.fn(),
    hide: vi.fn(),
  };
  postDashboardCommand(f.dashboard, "dock", {
    id: "search",
    label: "Search",
    dashboardTitle: "Late",
  });
  vi.advanceTimersByTime(9000);
  cleanups.push(registerFrame(f));
  vi.advanceTimersByTime(22000);
  markFrameLive(f.frameId);
  markFrameLive(f.frameId);
  expect(
    f.post.mock.calls.filter(([message]) => message.type === "codesense:command"),
  ).toHaveLength(1);
});

it("flushes pending edits before reload without retiring the shown frame, then retires after swap", async () => {
  const f = frame("reload-editor");
  const ready = vi.fn();
  const writes = flushFrameWrites(f.frameId).then(ready);
  expect(f.post).toHaveBeenLastCalledWith(expect.objectContaining({ type: "codesense:flush" }));
  expect(ready).not.toHaveBeenCalled();
  acknowledgeFlush(f.frameId, f.post.mock.calls[0][0].id);
  await writes;
  expect(f.hide).not.toHaveBeenCalled();
  expect(f.post.mock.calls.map(([m]) => m.type)).toEqual(["codesense:flush"]);
  const retired = retireAfterFlush((v) => v.frameId === f.frameId, vi.fn());
  acknowledgeFlush(f.frameId, f.post.mock.calls.at(-1)![0].id);
  await retired;
  expect(f.post).toHaveBeenLastCalledWith({ type: "codesense:teardown" });
});

it("unmounting during the non-retiring flush settles it and tears down the document", async () => {
  vi.useFakeTimers();
  const post = vi.fn();
  const unregister = registerFrame({
    frameId: "reload-unmount",
    dashboard: "editor",
    post,
    hide: vi.fn(),
  });
  const pending = flushFrameWrites("reload-unmount");
  unregister();
  await pending;
  expect(post.mock.calls.map(([message]) => message.type)).toEqual([
    "codesense:flush",
    "codesense:teardown",
  ]);
  expect(vi.getTimerCount()).toBe(0);
});

it("freezes owned child frames and drains their writes before replacement reads, then unfreezes that group", async () => {
  vi.useFakeTimers();
  const element = document.createElement("iframe");
  const root = {
    frameId: "parent",
    dashboard: "pane",
    element,
    post: vi.fn(),
    freeze: vi.fn(),
    hide: vi.fn(),
  };
  const child = {
    frameId: "message",
    parentFrame: "parent",
    dashboard: "message",
    post: vi.fn(),
    freeze: vi.fn(),
    hide: vi.fn(),
  };
  const nested = {
    ...child,
    frameId: "nested",
    parentFrame: "message",
    post: vi.fn(),
    freeze: vi.fn(),
  };
  const unrelated = {
    ...child,
    frameId: "other-message",
    parentFrame: "other-pane",
    post: vi.fn(),
    freeze: vi.fn(),
  };
  for (const frame of [root, child, nested, unrelated]) cleanups.push(registerFrame(frame));
  const unfreeze = setFrameFrozen(element, true);
  for (const frame of [root, child, nested]) expect(frame.freeze).toHaveBeenCalledWith(true);
  expect(unrelated.freeze).not.toHaveBeenCalled();
  const read = vi.fn();
  const pending = flushFrameWrites(element, 300).then(read);
  for (const frame of [root, nested])
    acknowledgeFlush(frame.frameId, frame.post.mock.calls[0][0].id);
  await vi.advanceTimersByTimeAsync(100);
  expect(read).not.toHaveBeenCalled();
  acknowledgeFlush(child.frameId, child.post.mock.calls[0][0].id);
  await pending;
  expect(read).toHaveBeenCalledOnce();
  expect(unrelated.post).not.toHaveBeenCalled();
  unfreeze();
  for (const frame of [root, child, nested]) {
    expect(frame.freeze).toHaveBeenLastCalledWith(false);
    expect(frame.hide).not.toHaveBeenCalled();
    expect(frame.post.mock.calls.some(([m]) => m.type === "codesense:teardown")).toBe(false);
  }
});
it("a hidden team's late-ready page leaves a queued command for the page on screen", () => {
  warmScopes.forget("o:a");
  warmScopes.forget("o:b");
  warmScopes.activate("o:a");
  warmScopes.activate("o:b");
  const mk = (frameId: string, teamKey: string) => {
    const post = vi.fn();
    cleanups.push(
      registerFrame({ frameId, dashboard: "d", slotId: "dock", teamKey, post, hide: vi.fn() }),
    );
    return post;
  };
  const hidden = mk("fa", "o:a");
  const shown = mk("fb", "o:b");
  postDashboardCommand("d", "dock", { id: "find", label: "Find", dashboardTitle: "D" });
  markFrameLive("fa");
  expect(hidden).not.toHaveBeenCalled();
  markFrameLive("fb");
  expect(shown.mock.calls.map(([m]) => m.id)).toEqual(["find"]);
  warmScopes.forget("o:a");
  warmScopes.forget("o:b");
});
