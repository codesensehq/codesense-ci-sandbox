// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it } from "vitest";

import { applyTriage, useInboxTriageStore } from "./inbox-triage-store";

function reset() {
  useInboxTriageStore.setState({ dismissed: {}, pinned: {} });
}

describe("inbox-triage-store — housekeeping (Dismiss/Snooze, Reprioritize)", () => {
  beforeEach(reset);

  it("dismiss hides an item from the lane; undo restores it", () => {
    useInboxTriageStore.getState().dismiss("needs-you:1");
    expect(useInboxTriageStore.getState().isDismissed("needs-you:1")).toBe(true);
    useInboxTriageStore.getState().restore("needs-you:1");
    expect(useInboxTriageStore.getState().isDismissed("needs-you:1")).toBe(false);
  });

  it("reprioritize pins an item to the top of its lane", () => {
    useInboxTriageStore.getState().pin("needs-you:2");
    expect(useInboxTriageStore.getState().isPinned("needs-you:2")).toBe(true);
  });

  it("applyTriage filters dismissed and sorts pinned first, stably otherwise", () => {
    useInboxTriageStore.setState({
      dismissed: { "k:2": true },
      pinned: { "k:3": true },
    });
    const keys = ["k:1", "k:2", "k:3", "k:4"];
    const out = applyTriage(keys, (k) => k, useInboxTriageStore.getState());
    // k:2 is dismissed → gone; k:3 pinned → first; the rest keep their order.
    expect(out).toEqual(["k:3", "k:1", "k:4"]);
  });
});
