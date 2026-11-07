// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { AgentPromptResult } from "@/tauri/commands";

import { sendToAgent } from "./send-to-agent";

const h = vi.hoisted(() => ({ send: vi.fn() }));
vi.mock("@/tauri/commands", () => ({ sendPromptToAgent: h.send }));

const result = (over: Partial<AgentPromptResult>): AgentPromptResult => ({
  status: "submitted",
  submitted: true,
  machine: null,
  code: null,
  message: null,
  ...over,
});

const LOCAL = { label: "local-barb", row: { handle: "barb#1", sessionId: "sess-barb" } };
const REMOTE = {
  label: "ac4-warm",
  row: { handle: "remote-tab-f0529fed", sessionId: "remote-shell-f0529fed" },
};

beforeEach(() => {
  h.send.mockReset();
});

describe("sendToAgent", () => {
  it("sends to a local agent by its handle and session", async () => {
    h.send.mockResolvedValue(result({}));
    await expect(sendToAgent(LOCAL, "do it")).resolves.toEqual({ kind: "sent" });
    expect(h.send).toHaveBeenCalledWith("barb#1", "sess-barb", "do it");
  });

  it("a local write the agent was not seen to start on still counts as sent", async () => {
    h.send.mockResolvedValue(result({ status: "typed", submitted: false }));
    await expect(sendToAgent(LOCAL, "do it")).resolves.toEqual({ kind: "sent" });
  });

  it("sends to a remote agent by its remote handle", async () => {
    h.send.mockResolvedValue(result({ machine: "codesense-test-1" }));
    await expect(sendToAgent(REMOTE, "go")).resolves.toEqual({ kind: "sent" });
    expect(h.send).toHaveBeenCalledWith("remote-tab-f0529fed", "remote-shell-f0529fed", "go");
  });

  it("names the machine when it is disconnected", async () => {
    h.send.mockResolvedValue(
      result({
        status: "refused",
        submitted: false,
        machine: "codesense-test-1",
        code: "staleOrigin",
        message: "codesense-test-1 is disconnected",
      }),
    );
    await expect(sendToAgent(REMOTE, "go")).resolves.toEqual({
      kind: "failed",
      message: "Couldn't send to ac4-warm: codesense-test-1 is disconnected. Nothing was sent.",
    });
  });

  it("an unknown delivery is uncertain, sent exactly once", async () => {
    h.send.mockResolvedValue(
      result({ status: "unknown", submitted: null, machine: "codesense-test-1" }),
    );
    await expect(sendToAgent(REMOTE, "go")).resolves.toEqual({
      kind: "uncertain",
      message:
        "Sent to ac4-warm on codesense-test-1, but couldn't confirm it arrived. Check its terminal before sending again.",
    });
    expect(h.send).toHaveBeenCalledTimes(1);
  });

  it("a remote refusal without words still names the machine", async () => {
    h.send.mockResolvedValue(
      result({ status: "notSent", submitted: false, machine: "codesense-test-1" }),
    );
    await expect(sendToAgent(REMOTE, "go")).resolves.toEqual({
      kind: "failed",
      message: "Couldn't send to ac4-warm: codesense-test-1 did not take it. Nothing was sent.",
    });
  });

  it("an ended tab is a failure naming the machine, without a doubled period", async () => {
    h.send.mockResolvedValue(
      result({
        status: "refused",
        submitted: false,
        machine: "codesense-test-1",
        code: "ended",
        message: "its tab on codesense-test-1 has ended.",
      }),
    );
    await expect(sendToAgent(REMOTE, "go")).resolves.toEqual({
      kind: "failed",
      message:
        "Couldn't send to ac4-warm: its tab on codesense-test-1 has ended. Nothing was sent.",
    });
  });

  it("an empty reason falls back to naming the machine", async () => {
    h.send.mockResolvedValue(
      result({ status: "refused", submitted: false, machine: "box", message: "  " }),
    );
    await expect(sendToAgent(REMOTE, "go")).resolves.toEqual({
      kind: "failed",
      message: "Couldn't send to ac4-warm: box did not take it. Nothing was sent.",
    });
  });

  it("a status that does not say 'not submitted' is uncertain, never a failure", async () => {
    h.send.mockResolvedValue(
      result({ status: "refused", submitted: null, machine: "box", message: "lost" }),
    );
    await expect(sendToAgent(REMOTE, "go")).resolves.toMatchObject({ kind: "uncertain" });
  });

  it("a rejected command reads as a failure with its reason", async () => {
    h.send.mockImplementation(() => Promise.reject(new Error("the agent has no running session")));
    await expect(sendToAgent(LOCAL, "go")).resolves.toEqual({
      kind: "failed",
      message: "Couldn't send to local-barb: the agent has no running session",
    });
  });
});
