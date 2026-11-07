// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));

import {
  isCommandUnavailable,
  loadInboxRollup,
  loadSessionInbox,
  sendAsHuman,
  wireRecipient,
} from "./api";
import { FX } from "./fixtures";

beforeEach(() => invoke.mockReset());

describe("inbox api", () => {
  it("reads a session's inbox from the backend when the command exists", async () => {
    invoke.mockResolvedValue({ handle: "h", roles: [], messages: [] });
    await expect(loadSessionInbox("h")).resolves.toEqual({
      dto: { handle: "h", roles: [], messages: [] },
      source: "live",
    });
    expect(invoke).toHaveBeenCalledWith("list_session_inbox", { handle: "h" });
  });

  it("falls back to the demo data only when the command is unknown", async () => {
    invoke.mockImplementation(() => Promise.reject("Command list_session_inbox not found"));
    const { dto, source } = await loadSessionInbox(FX.t9);
    expect(source).toBe("fixture");
    expect(dto.messages.length).toBeGreaterThan(0);

    // Only the inbox reads fail; anything else the app asks in the background still answers.
    invoke.mockImplementation((cmd?: string) =>
      cmd?.startsWith("list_")
        ? Promise.reject(new Error("database is locked"))
        : Promise.resolve(undefined),
    );
    const failures = await Promise.allSettled([loadSessionInbox(FX.t9), loadInboxRollup()]);
    expect(failures.map((f) => (f.status === "rejected" ? String(f.reason) : f.status))).toEqual([
      "Error: database is locked",
      "Error: database is locked",
    ]);
  });

  it("recognises an unknown command by name, never another command's error", () => {
    expect(isCommandUnavailable("Command list_inbox_rollup not found", "list_inbox_rollup")).toBe(
      true,
    );
    expect(isCommandUnavailable("Command other not found", "list_inbox_rollup")).toBe(false);
  });

  it("addresses a role as #role, a session as @handle and the human as human", () => {
    expect(wireRecipient("#reviewer")).toBe("#reviewer");
    expect(wireRecipient(FX.child)).toBe(`@${FX.child}`);
    expect(wireRecipient("human")).toBe("human");
  });

  it("sends a reply with the message it answers", async () => {
    invoke.mockImplementation(() => Promise.resolve({ status: "sent", messageId: 3 }));
    await sendAsHuman({ org: "o", team: "t" }, FX.child, "hi", 9);
    expect(invoke).toHaveBeenCalledWith("send_inbox_as_human", {
      team: { org: "o", team: "t" },
      to: `@${FX.child}`,
      text: "hi",
      contentType: "markdown",
      onClick: null,
      attachFile: null,
      replyTo: null,
      summary: null,
      inReplyTo: 9,
    });
  });
});
