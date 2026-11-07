// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { AttentionRowDto } from "@/tauri/commands";

import {
  INBOX_ACTION_LABEL,
  isAgentBearing,
  resolveInboxActions,
  type InboxActionKind,
} from "./inbox-actions";
import type { InboxItem, InboxLaneId } from "./inbox-model";

function item(
  overrides: Partial<AttentionRowDto>,
  lane: InboxLaneId,
  need: InboxItem["need"],
): InboxItem {
  const row: AttentionRowDto = {
    id: 1,
    summary: "Something needs you.",
    source: "post",
    ref: null,
    initiator: "human",
    ...overrides,
  };
  return { key: `${lane}:1`, row, need, lane };
}

describe("resolveInboxActions — need → the matching action (§2.3)", () => {
  it("maps an agent-bearing question to Reply", () => {
    const a = resolveInboxActions(item({ liveSessionId: "s" }, "needs-you", "awaiting-answer"));
    expect(a.need).toEqual(["reply"]);
  });

  it("offers NO Reply on a non-agent-bearing question (nothing/no run to reply to)", () => {
    // A human triage row with no agent/run behind it can't be replied to — and it would
    // have no Open-in-CLI door to fall back to, so it must not present a dead Reply.
    const a = resolveInboxActions(item({ initiator: "human" }, "needs-you", "awaiting-answer"));
    expect(a.need).toEqual([]);
    expect(a.primary).toBeNull();
  });

  it("maps a gate (needs-approval) to Approve / Reject", () => {
    const a = resolveInboxActions(item({}, "needs-you", "needs-approval"));
    expect(a.need).toEqual(["approve", "reject"]);
  });

  it("maps a validation gate to Approve / Reject", () => {
    const a = resolveInboxActions(item({}, "needs-you", "awaiting-validation"));
    expect(a.need).toEqual(["approve", "reject"]);
  });

  it("maps stuck / needs-eyes to Open in CLI (the primary), no extra need verb", () => {
    // An agent-bearing needs-attention item's action IS Open in CLI (take the wheel).
    const a = resolveInboxActions(
      item({ liveSessionId: "sess-1" }, "needs-you", "needs-attention"),
    );
    expect(a.primary).toBe("open-in-cli");
    expect(a.need).toEqual([]);
  });

  it("puts housekeeping (Reprioritize, Dismiss/Snooze) in ONE overflow, never inline", () => {
    const a = resolveInboxActions(item({}, "needs-you", "needs-approval"));
    expect(a.overflow).toEqual(["reprioritize", "dismiss"]);
    // Housekeeping is never promoted into the inline need verbs.
    expect(a.need).not.toContain("reprioritize");
    expect(a.need).not.toContain("dismiss");
  });
});

describe("Open in CLI — the always-visible primary door on every agent-bearing row", () => {
  it("is present when the row carries a live agent handle", () => {
    const a = resolveInboxActions(item({ liveSessionId: "sess-9" }, "running", null));
    expect(a.primary).toBe("open-in-cli");
  });

  it("is present when an agent posted the item (even with no live handle — exited/headless)", () => {
    const a = resolveInboxActions(item({ initiator: "agent" }, "needs-you", "awaiting-answer"));
    expect(a.primary).toBe("open-in-cli");
  });

  it("is absent on a plain human triage row with no agent", () => {
    const a = resolveInboxActions(item({ initiator: "human" }, "needs-you", "needs-approval"));
    expect(a.primary).toBeNull();
  });

  it("is present on a question row and coexists with Reply (agent-bearing)", () => {
    const a = resolveInboxActions(
      item({ initiator: "agent", liveSessionId: "sess-2" }, "needs-you", "awaiting-answer"),
    );
    expect(a.primary).toBe("open-in-cli");
    expect(a.need).toEqual(["reply"]);
  });
});

describe("isAgentBearing", () => {
  it("treats a non-empty live handle as agent-bearing", () => {
    expect(isAgentBearing(item({ liveSessionId: "sess-1" }, "running", null))).toBe(true);
  });
  it("treats a blank/absent handle from a human row as not agent-bearing", () => {
    expect(isAgentBearing(item({ liveSessionId: "   " }, "needs-you", "done"))).toBe(false);
    expect(isAgentBearing(item({}, "needs-you", "done"))).toBe(false);
  });
  it("treats an agent-posted row as agent-bearing even without a handle", () => {
    expect(isAgentBearing(item({ initiator: "agent" }, "done", "done"))).toBe(true);
  });
});

describe("fixed named vocabulary — constant labels, no morphing verb (§8.5, §8.12)", () => {
  it("labels Open in CLI with the exact constant string in every state", () => {
    expect(INBOX_ACTION_LABEL["open-in-cli"]).toBe("Open in CLI");
  });
  it("labels the question action exactly 'Reply' (never 'Answer and continue')", () => {
    expect(INBOX_ACTION_LABEL.reply).toBe("Reply");
  });
  it("draws every action from the fixed named set", () => {
    const vocab: InboxActionKind[] = [
      "open-in-cli",
      "reply",
      "approve",
      "reject",
      "reprioritize",
      "dismiss",
    ];
    for (const kind of vocab) {
      expect(INBOX_ACTION_LABEL[kind]).toBeTruthy();
    }
    // Not a control-plane / lease / tier word anywhere in the vocabulary.
    const all = Object.values(INBOX_ACTION_LABEL).join(" ").toLowerCase();
    expect(all).not.toMatch(/ratify|endors|steer|dispatch|subclass|t2/);
  });
});
