// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it } from "vitest";

import {
  draftStoreKey,
  inboxReplyRequestId,
  useInboxDraftStore,
  type InboxDraftKey,
} from "./inbox-draft-store";

/**
 * SW-BSI-2 — draft store invariants, each anchored to a UI contract case:
 *  - UI-01 / UI-11: a draft lives at `(user, team, conversationRoot)`; the revision
 *    is per-key and monotonic.
 *  - UI-03 / UI-11: identical text in two threads are two independent drafts; neither
 *    affects the other.
 *  - UI-04 / UI-11: Send captures the revision at click time; a successful ack clears
 *    ONLY the exact revision — a newer edit in-flight is preserved.
 *  - UI-11 Round-4 seam: edit to identical text still creates a new revision; a late
 *    ack to the ORIGINAL revision cannot clear the newer draft.
 */

const KEY_A: InboxDraftKey = {
  userId: "1234567",
  teamKey: "alderco:default",
  conversationRootId: 101,
};
const KEY_B: InboxDraftKey = {
  userId: "1234567",
  teamKey: "alderco:default",
  conversationRootId: 202,
};
const KEY_OTHER_USER: InboxDraftKey = {
  userId: "7000000",
  teamKey: "alderco:default",
  conversationRootId: 101,
};
const KEY_OTHER_TEAM: InboxDraftKey = {
  userId: "1234567",
  teamKey: "acme:main",
  conversationRootId: 101,
};

beforeEach(() => {
  useInboxDraftStore.getState()._reset();
});

describe("inbox-draft-store", () => {
  it("UI-01: set + get returns the text; revision starts at 1", () => {
    const store = useInboxDraftStore.getState();
    const rev = store.setDraft(KEY_A, "hello");
    expect(rev).toBe(1);
    expect(store.getDraft(KEY_A)).toEqual({ text: "hello", revision: 1 });
    expect(useInboxDraftStore.getState().getDraft(KEY_A)).toEqual({
      text: "hello",
      revision: 1,
    });
  });

  it("UI-01: every edit bumps the revision", () => {
    const store = useInboxDraftStore.getState();
    expect(store.setDraft(KEY_A, "a")).toBe(1);
    expect(store.setDraft(KEY_A, "ab")).toBe(2);
    expect(store.setDraft(KEY_A, "abc")).toBe(3);
    expect(store.getDraft(KEY_A)?.text).toBe("abc");
    expect(store.getDraft(KEY_A)?.revision).toBe(3);
  });

  it("UI-11 Round-4: edit to identical text still creates a new revision", () => {
    const store = useInboxDraftStore.getState();
    expect(store.setDraft(KEY_A, "same")).toBe(1);
    expect(store.setDraft(KEY_A, "same")).toBe(2);
    expect(store.getDraft(KEY_A)?.revision).toBe(2);
  });

  it("UI-03/UI-11: identical text in two threads are two independent drafts", () => {
    const store = useInboxDraftStore.getState();
    store.setDraft(KEY_A, "identical");
    store.setDraft(KEY_B, "identical");
    // Clearing A's revision does NOT touch B — no text-equality merging.
    expect(store.clearIfRevision(KEY_A, 1)).toBe(true);
    expect(store.getDraft(KEY_A)).toBeNull();
    expect(store.getDraft(KEY_B)?.text).toBe("identical");
  });

  it("A → B → A: switching selection preserves A's draft", () => {
    const store = useInboxDraftStore.getState();
    store.setDraft(KEY_A, "draft-a");
    store.setDraft(KEY_B, "draft-b");
    // A's draft is still there after writing to B.
    expect(store.getDraft(KEY_A)?.text).toBe("draft-a");
    // Writing to A again doesn't affect B.
    store.setDraft(KEY_A, "draft-a-edited");
    expect(store.getDraft(KEY_B)?.text).toBe("draft-b");
  });

  it("UI-11: clearIfRevision preserves a newer in-flight edit", () => {
    const store = useInboxDraftStore.getState();
    store.setDraft(KEY_A, "r1"); // revision 1
    store.setDraft(KEY_A, "r2"); // revision 2 — edited after send captured
    // Send captured revision 1; the late ack arrives.
    expect(store.clearIfRevision(KEY_A, 1)).toBe(false);
    // The newer edit survived.
    expect(store.getDraft(KEY_A)).toEqual({ text: "r2", revision: 2 });
  });

  it("UI-04: clearIfRevision at the matching revision clears exactly that draft", () => {
    const store = useInboxDraftStore.getState();
    store.setDraft(KEY_A, "a");
    store.setDraft(KEY_B, "b");
    expect(store.clearIfRevision(KEY_A, 1)).toBe(true);
    expect(store.getDraft(KEY_A)).toBeNull();
    expect(store.getDraft(KEY_B)?.text).toBe("b");
  });

  it("key isolation across users and teams", () => {
    const store = useInboxDraftStore.getState();
    store.setDraft(KEY_A, "mine");
    store.setDraft(KEY_OTHER_USER, "theirs");
    store.setDraft(KEY_OTHER_TEAM, "elsewhere");
    expect(store.getDraft(KEY_A)?.text).toBe("mine");
    expect(store.getDraft(KEY_OTHER_USER)?.text).toBe("theirs");
    expect(store.getDraft(KEY_OTHER_TEAM)?.text).toBe("elsewhere");
    // All three storeKeys are distinct.
    const keys = new Set([
      draftStoreKey(KEY_A),
      draftStoreKey(KEY_OTHER_USER),
      draftStoreKey(KEY_OTHER_TEAM),
    ]);
    expect(keys.size).toBe(3);
  });

  it("inboxReplyRequestId is deterministic for (key, revision)", () => {
    const a1 = inboxReplyRequestId(KEY_A, 1);
    const a1_again = inboxReplyRequestId(KEY_A, 1);
    const a2 = inboxReplyRequestId(KEY_A, 2);
    const b1 = inboxReplyRequestId(KEY_B, 1);
    expect(a1).toBe(a1_again);
    expect(a1).not.toBe(a2);
    expect(a1).not.toBe(b1);
  });

  it("clearIfRevision on an absent draft is a no-op false", () => {
    const store = useInboxDraftStore.getState();
    expect(store.clearIfRevision(KEY_A, 1)).toBe(false);
    expect(store.getDraft(KEY_A)).toBeNull();
  });
});
