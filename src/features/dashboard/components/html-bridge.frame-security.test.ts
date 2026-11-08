// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { renderHook } from "@testing-library/react";
import { frameFixture } from "../../../../tests/support/fixtures/frame";
import { resolveFrameCall, checkFrameGrant } from "./html-bridge";
import { useActionGate } from "./use-action-gate";
import { verbDispatch } from "../lib/verb-dispatch";
vi.mock("../lib/verb-dispatch", async () =>
  (await import("@/test/verb-dispatch-mock")).openVerbDispatchMock(),
);
import { agentReadInbox, bridgeResolveOpenTarget, resolveFrameGrant } from "@/tauri/commands";
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  resolveFrameGrant: vi.fn(),
  agentReadInbox: vi.fn(),
  bridgeResolveOpenTarget: vi.fn(),
}));
vi.mock("./action-audit", () => ({ logAction: vi.fn() }));
const frame = frameFixture({
  documentPath: "inbox-message://acme/a/7",
  contentRoot: "/ctx/acme/a/content",
  slot: "message",
  message: { org: "acme", team: "a", messageId: 7 },
});
frame.grant.team = { org: "acme", team: "a" };
const identity = { origin: frame.origin, token: frame.token };
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(resolveFrameGrant).mockResolvedValue({ ...frame.grant, scope: "open" });
});
describe("per-call frame grants", () => {
  it("requires the matching origin and re-resolves on every call, including after revocation", async () => {
    await expect(
      resolveFrameCall(frame, { ...identity, origin: "http://f-other.localhost:9123" }),
    ).rejects.toThrow("denied:");
    expect(resolveFrameGrant).not.toHaveBeenCalled();
    expect(await resolveFrameCall(frame, identity)).toEqual(frame.grant);
    vi.mocked(resolveFrameGrant).mockRejectedValue(new Error("denied: frame identity"));
    await expect(resolveFrameCall(frame, identity)).rejects.toThrow("denied:");
    expect(resolveFrameGrant).toHaveBeenCalledTimes(2);
  });
  it("leaves verb admission to Rust and confines team-safe message targets", async () => {
    await expect(checkFrameGrant(frame.grant, "sendAsHuman", {}, frame)).resolves.toBeUndefined();
    const safe = { ...frame.grant, scope: "team" as const };
    for (const [verb, args] of [["inbox", { org: "acme", team: "b" }]] as const) {
      await expect(checkFrameGrant(safe, verb, args, frame)).rejects.toThrow("denied:");
    }
    await expect(checkFrameGrant(safe, "inbox", {}, frame)).resolves.toBeUndefined();
    await expect(checkFrameGrant(safe, "sessions", {}, frame)).resolves.toBeUndefined();
    await expect(
      checkFrameGrant(safe, "subscribe", { topic: "sessions" }, frame),
    ).resolves.toBeUndefined();
    vi.mocked(bridgeResolveOpenTarget).mockResolvedValue({
      path: "/ctx/acme/b/content/file",
      org: "acme",
      team: "b",
      contentRoot: "/ctx/acme/b/content",
    });
    await expect(
      checkFrameGrant(safe, "openFile", { path: "/ctx/acme/b/content/file" }, frame),
    ).rejects.toThrow("denied:");
  });
  it("message mutations stay in their own thread", async () => {
    vi.mocked(agentReadInbox).mockResolvedValue({
      messages: [
        { messageId: 7, inReplyTo: null },
        { messageId: 8, inReplyTo: 7 },
        { messageId: 9, inReplyTo: null },
      ],
    } as Awaited<ReturnType<typeof agentReadInbox>>);
    vi.mocked(resolveFrameGrant).mockImplementation(async (_origin, _token, ids) => {
      if (ids?.includes(9)) throw new Error("denied: message thread");
      return { ...frame.grant, scope: "team" };
    });
    const safe = { ...frame.grant, scope: "team" as const };
    await expect(
      checkFrameGrant(safe, "archive", { messageIds: [7, 8] }, frame),
    ).resolves.toBeUndefined();
    await expect(checkFrameGrant(safe, "markRead", { messageIds: [9] }, frame)).rejects.toThrow(
      "denied: message thread",
    );
    await expect(
      checkFrameGrant(safe, "unarchive", { messageIds: [7, 8], restoreUnread: [8] }, frame),
    ).resolves.toBeUndefined();
    await expect(
      checkFrameGrant(safe, "unarchive", { messageIds: [9], restoreUnread: [9] }, frame),
    ).rejects.toThrow("denied: message thread");
  });
  it("allows mutations of a recent reply whose parent is past the inbox list cap", async () => {
    vi.mocked(agentReadInbox).mockResolvedValue({
      messages: [
        { messageId: 7, inReplyTo: 1 },
        ...Array.from({ length: 499 }, (_, i) => ({ messageId: i + 100, inReplyTo: null })),
      ],
    } as Awaited<ReturnType<typeof agentReadInbox>>);
    const safe = { ...frame.grant, scope: "team" as const };
    await expect(
      checkFrameGrant(safe, "archive", { messageIds: [7] }, frame),
    ).resolves.toBeUndefined();
    await expect(
      checkFrameGrant(safe, "markRead", { messageIds: [7] }, frame),
    ).resolves.toBeUndefined();
    expect(agentReadInbox).not.toHaveBeenCalled();
    expect(resolveFrameGrant).toHaveBeenCalledWith(frame.origin, frame.token, [7]);
  });
  it("resolves origin and token before native admission and forwards the exact grant", async () => {
    const sendToSession = vi.fn(async () => ({ delivered: true }));
    const controller = {
      sendToSession,
    } as unknown as import("./dashboard-launch").DashboardLaunchController;
    const { result } = renderHook(() =>
      useActionGate(frame.documentPath, {
        frame,
        controller,
        prefsKey: "forged-dashboard",
        message: { org: "forged-org", team: "forged-team", messageId: 9, sender: "forged-sender" },
      }),
    );
    vi.mocked(resolveFrameGrant).mockResolvedValue({ ...frame.grant, scope: "team" });
    vi.mocked(verbDispatch).mockResolvedValueOnce({
      status: "deny",
      reason: "denied: message frame",
    });
    await expect(result.current.runGatedAction("sendToSession", {}, identity)).rejects.toThrow(
      "denied: message frame",
    );
    expect(verbDispatch).toHaveBeenCalledTimes(1);
    vi.mocked(verbDispatch).mockClear();
    vi.mocked(resolveFrameGrant).mockResolvedValue(frame.grant);
    for (const badIdentity of [
      { ...identity, origin: "http://f-other.localhost:9123" },
      { ...identity, token: "wrong-token" },
    ]) {
      await expect(result.current.runGatedAction("sendToSession", {}, badIdentity)).rejects.toThrow(
        "denied: frame identity",
      );
    }
    expect(verbDispatch).not.toHaveBeenCalled();
    expect(sendToSession).not.toHaveBeenCalled();
    const args = { target: "S-1", text: "hello" };
    await result.current.runGatedAction("sendToSession", args, identity);
    expect(verbDispatch).toHaveBeenCalledExactlyOnceWith(
      { ...frame.grant, kind: frame.kind },
      "sendToSession",
      args,
    );
    expect(sendToSession).toHaveBeenCalledTimes(1);
    vi.mocked(resolveFrameGrant).mockRejectedValue(new Error("denied: frame identity"));
    await expect(result.current.runGatedAction("sendToSession", args, identity)).rejects.toThrow(
      "denied: frame identity",
    );
    expect(verbDispatch).toHaveBeenCalledTimes(1);
  });
});
