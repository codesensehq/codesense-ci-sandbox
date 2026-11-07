// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { renderHook } from "@testing-library/react";
const dispatch = vi.hoisted(() => vi.fn());
const settle = vi.hoisted(() => vi.fn());
const handler = vi.hoisted(() => vi.fn());
vi.mock("../lib/verb-dispatch", () => ({
  verbDispatch: dispatch,
  verbSettle: settle,
  verbAnswer: vi.fn(),
}));
vi.mock("./html-bridge", () => ({ runBridgeCommand: handler }));
vi.mock("@/shared/lib/local-profile-session", () => ({ rendererPrincipal: async () => "42" }));
vi.mock("@/tauri/commands", () => ({
  bridgeDocContext: async () => ({ org: "org", team: "team", layer: "team" }),
}));
import { useActionGate } from "./use-action-gate";
beforeEach(() => {
  vi.clearAllMocks();
  dispatch.mockResolvedValue({ status: "admit", callId: "call" });
  settle.mockResolvedValue(undefined);
  handler.mockResolvedValue({ ok: true });
});
describe("native admission seam", () => {
  it("gates a typed prompt before the bridge and preserves submit:false", async () => {
    const args = { handle: "agent#1", text: "draft", submit: false };
    const { result } = renderHook(() => useActionGate("/board.html"));
    await result.current.runGatedAction("sendPrompt", args);
    expect(dispatch).toHaveBeenCalledWith(
      expect.objectContaining({ dashboard: "/board.html" }),
      "sendPrompt",
      args,
    );
    expect(handler).toHaveBeenCalledWith("sendPrompt", args, expect.anything());
    expect(settle).toHaveBeenCalledWith("call", true, "admitted");
  });
  it.each(["denied: grant", "denied: send budget"])(
    "a prompt refused with %s never reaches the send command",
    async (reason) => {
      dispatch.mockResolvedValue({ status: "deny", reason });
      const { result } = renderHook(() => useActionGate("/board.html"));
      await expect(
        result.current.runGatedAction("sendPrompt", { handle: "agent#1", text: "hello" }),
      ).rejects.toThrow(reason);
      expect(handler).not.toHaveBeenCalled();
    },
  );
  it("forwards the host identity and settles the admitted handler", async () => {
    const { result } = renderHook(() => useActionGate("/board.html"));
    await expect(
      result.current.runGatedAction("sendToSession", { target: "agent", text: "hello" }),
    ).resolves.toEqual({ ok: true });
    expect(dispatch).toHaveBeenCalledWith(
      expect.objectContaining({ dashboard: "/board.html", principal: "42", scope: "team" }),
      "sendToSession",
      { target: "agent", text: "hello" },
    );
    expect(settle).toHaveBeenCalledWith("call", true, "admitted");
  });
  it.each([
    "denied: send budget",
    "denied: write budget",
    "denied: prefs budget",
    "denied: spawn budget",
    "denied: message frame",
    "busy",
  ])("a native %s never runs the handler", async (reason) => {
    dispatch.mockResolvedValue({ status: "deny", reason });
    const { result } = renderHook(() => useActionGate("/board.html"));
    await expect(result.current.runGatedAction("sendToSession", {})).rejects.toThrow(reason);
    expect(handler).not.toHaveBeenCalled();
    expect(settle).not.toHaveBeenCalled();
  });
  it("settles failure for the Rust write refund, preserving the handler error", async () => {
    handler.mockRejectedValue(new Error("denied: read-only"));
    const { result } = renderHook(() => useActionGate("/board.html"));
    await expect(result.current.runGatedAction("setStatus", {})).rejects.toThrow(
      "denied: read-only",
    );
    expect(settle).toHaveBeenCalledWith("call", false, "denied: read-only");
  });
  it("settles an all-failed batch as not landed", async () => {
    handler.mockResolvedValue([{ ok: false }]);
    const { result } = renderHook(() => useActionGate("/board.html"));
    await result.current.runGatedAction("setStatus", {});
    expect(settle).toHaveBeenCalledWith("call", false, "write not applied");
  });
  it("a new rendered file has a new frame identity", async () => {
    const hook = renderHook(({ path }) => useActionGate(path), { initialProps: { path: "/a" } });
    await hook.result.current.runGatedAction("sessions", {});
    const first = dispatch.mock.calls[0][0].frameId;
    hook.rerender({ path: "/b" });
    await hook.result.current.runGatedAction("sessions", {});
    expect(dispatch.mock.calls[1][0].frameId).not.toBe(first);
  });
});

it("requires the host confirmation for a built-in even under open policy", async () => {
  const { createElement } = await import("react");
  const { VerbConfirmationContext } = await import("./verb-confirmation-context");
  const confirm = vi.fn().mockResolvedValue("cancel");
  const { result } = renderHook(() => useActionGate("/board.html"), {
    wrapper: ({ children }) =>
      createElement(VerbConfirmationContext.Provider, { value: confirm }, children),
  });
  await expect(
    result.current.runGatedAction("stopSession", { handle: "agent" }, undefined, {
      confirmInHost: true,
    }),
  ).resolves.toEqual({ status: "cancelled" });
  expect(confirm).toHaveBeenCalledWith(
    expect.objectContaining({ verb: "stopSession", allowAlways: false }),
  );
  expect(handler).not.toHaveBeenCalled();
  expect(settle).toHaveBeenCalledWith("call", false, "cancelled");
});
