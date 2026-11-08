// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
import { renderHook } from "@testing-library/react";
import { createElement } from "react";
import { VerbConfirmationContext } from "./verb-confirmation-context";
import { pasteToActiveTerminal } from "@/shared/lib/send-to-terminal";
import { frameFixture } from "../../../../tests/support/fixtures/frame";
import { useActionGate } from "./use-action-gate";
import { runBridgeCommand } from "./html-bridge";
import {
  agentReadInbox,
  bridgeDocContext,
  resolveFrameGrant,
  listAgentUpdates,
  getHumanStatus,
  sendPromptToAgent,
} from "@/tauri/commands";
import { verbAnswer, verbDispatch } from "../lib/verb-dispatch";

vi.mock("../lib/verb-dispatch", async () =>
  (await import("@/test/verb-dispatch-mock")).openVerbDispatchMock(),
);
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  resolveFrameGrant: vi.fn(),
  bridgeDocContext: vi.fn(),
  agentReadInbox: vi.fn(),
  agentReadSessions: vi.fn(async () => []),
  listAgentUpdates: vi.fn(async () => []),
  getHumanStatus: vi.fn(async () => ({})),
  sendPromptToAgent: vi.fn(),
}));
vi.mock("@/shared/lib/send-to-terminal", async (original) => ({
  ...(await original<typeof import("@/shared/lib/send-to-terminal")>()),
  pasteToActiveTerminal: vi.fn(async () => ({ status: "typed", message: null })),
}));
const source = { org: "acme", team: "parent" };
const target = { org: "acme", team: "child" };
const frame = frameFixture({
  documentPath: "/ctx/parent/panes/inbox.html",
  contentRoot: "/ctx/child/content",
  slot: "right-pane",
});
frame.kind = "team";
frame.grant = { ...frame.grant, scope: "team", team: source };
const identity = { origin: frame.origin, token: frame.token };
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(resolveFrameGrant).mockResolvedValue(frame.grant);
  vi.mocked(bridgeDocContext).mockResolvedValue({
    ...target,
    layer: "team",
    contentRoot: frame.contentRoot,
  });
  vi.mocked(agentReadInbox).mockResolvedValue({ messages: [], humanUnread: 0 } as never);
  vi.mocked(verbDispatch).mockResolvedValue({ status: "admit", callId: "call" });
});

it("admits argument-free inherited inbox calls against their resolved target team", async () => {
  vi.mocked(verbDispatch).mockImplementation(async (_caller, _verb, args) =>
    (args as typeof target)?.team === target.team
      ? { status: "deny", reason: "denied: grant" }
      : { status: "admit", callId: "call" },
  );
  const { result } = renderHook(() =>
    useActionGate(frame.documentPath, { frame, docPath: `${frame.contentRoot}/inbox.html` }),
  );
  await expect(result.current.runGatedAction("inbox", {}, identity)).rejects.toThrow(
    "denied: grant",
  );
  expect(verbDispatch).toHaveBeenCalledWith(
    expect.objectContaining({ team: source }),
    "inbox",
    target,
  );
  expect(agentReadInbox).not.toHaveBeenCalled();
});

it("executes the same resolved team that native admission checked", async () => {
  const { result } = renderHook(() =>
    useActionGate(frame.documentPath, { frame, docPath: `${frame.contentRoot}/inbox.html` }),
  );
  await result.current.runGatedAction("inbox", {}, identity);
  expect(verbDispatch).toHaveBeenCalledWith(expect.anything(), "inbox", target);
  expect(agentReadInbox).toHaveBeenCalledWith(target, expect.anything());
  expect(bridgeDocContext).toHaveBeenCalledTimes(1);
});

it("defaults an unmaterialized inbox handler to the grant's bound team", async () => {
  await runBridgeCommand(
    "inbox",
    {},
    {
      frameGrant: frame.grant,
      docPath: `${frame.contentRoot}/inbox.html`,
    },
  );
  expect(agentReadInbox).toHaveBeenCalledWith(source, expect.anything());
});

it.each(["sessions", "subscribe"])(
  "lets native admission decide %s for a team-safe message",
  async (verb) => {
    const message = frameFixture({
      documentPath: "inbox-message://acme/parent/1",
      contentRoot: "/ctx/parent/content",
      slot: "message",
      message: { ...source, messageId: 1 },
    });
    message.grant = { ...message.grant, team: source, scope: "team" };
    vi.mocked(resolveFrameGrant).mockResolvedValue(message.grant);
    const subscribe = vi.fn(async () => ({ snapshot: [] }));
    const { result } = renderHook(() =>
      useActionGate(message.documentPath, {
        frame: message,
        liveFrame: async () => ({ subscribe }) as never,
      }),
    );
    await result.current.runGatedAction(
      verb,
      { topic: "sessions" },
      {
        origin: message.origin,
        token: message.token,
      },
    );
    expect(verbDispatch).toHaveBeenCalledWith(expect.anything(), verb, { topic: "sessions" });
    expect(subscribe).toHaveBeenCalledWith(
      "sessions",
      ...(verb === "subscribe" ? [undefined, undefined] : []),
    );
  },
);

it.each(["updates", "humanStatus"])(
  "confines inherited %s reads to the admitted team",
  async (verb) => {
    const handler = vi.mocked(verb === "updates" ? listAgentUpdates : getHumanStatus);
    vi.mocked(verbDispatch).mockImplementation(async (_caller, _verb, args) =>
      (args as typeof target)?.team === target.team
        ? { status: "deny", reason: "denied: grant" }
        : { status: "admit", callId: "call" },
    );
    const { result } = renderHook(() =>
      useActionGate(frame.documentPath, {
        frame,
        docPath: `${frame.contentRoot}/inbox.html`,
      }),
    );
    await expect(result.current.runGatedAction(verb, {}, identity)).rejects.toThrow(
      "denied: grant",
    );
    expect(handler).not.toHaveBeenCalled();
    expect(verbDispatch).toHaveBeenCalledWith(
      expect.objectContaining({ team: source }),
      verb,
      target,
    );
    vi.mocked(verbDispatch).mockResolvedValue({ status: "admit", callId: "call" });
    await result.current.runGatedAction(verb, {}, identity);
    expect(handler.mock.calls.at(-1)?.[0]).toEqual(target);
    await runBridgeCommand(
      verb,
      {},
      { frameGrant: frame.grant, docPath: `${frame.contentRoot}/inbox.html` },
    );
    expect(handler.mock.calls.at(-1)?.[0]).toEqual(source);
  },
);

it("a granted page sendPrompt reports the shared native spend refusal", async () => {
  const reason = "agent worker, local day: spend $1.0000, cap $1.0000";
  vi.mocked(sendPromptToAgent).mockRejectedValue(new Error(reason));
  const { result } = renderHook(() =>
    useActionGate(frame.documentPath, { frame, docPath: `${frame.contentRoot}/board.html` }),
  );
  const outcome = await result.current.runGatedAction(
    "sendPrompt",
    { handle: "worker", text: "continue" },
    identity,
  );
  expect(verbDispatch).toHaveBeenCalledWith(
    expect.anything(),
    "sendPrompt",
    expect.objectContaining({ handle: "worker", text: "continue" }),
  );
  expect(sendPromptToAgent).toHaveBeenCalledWith("worker", null, "continue", undefined);
  expect(outcome).toMatchObject({ submitted: false, message: reason });
});

it.each([
  ["cancel", { status: "cancelled" }, 0],
  ["once", { status: "confirmed" }, 1],
] as const)(
  "an agent-authored page's terminal paste waits for the person's confirmation (%s)",
  async (answer, outcome, pasted) => {
    vi.mocked(verbDispatch).mockResolvedValue({ status: "pending", confirmId: "c1" } as never);
    vi.mocked(verbAnswer).mockResolvedValue({
      callId: "call",
      args: { handle: "active", text: "ls", submit: false },
    } as never);
    const confirm = vi.fn().mockResolvedValue(answer);
    const { result } = renderHook(
      () =>
        useActionGate(frame.documentPath, { frame, docPath: `${frame.contentRoot}/board.html` }),
      {
        wrapper: ({ children }) =>
          createElement(VerbConfirmationContext.Provider, { value: confirm }, children),
      },
    );
    await expect(
      result.current.runGatedAction(
        "sendPrompt",
        { handle: "active", text: "ls", submit: false },
        identity,
      ),
    ).resolves.toMatchObject(outcome);
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ verb: "sendPrompt" }));
    expect(pasteToActiveTerminal).toHaveBeenCalledTimes(pasted);
  },
);
