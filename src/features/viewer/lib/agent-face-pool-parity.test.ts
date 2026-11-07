// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import cases from "../../../../src-tauri/src/avatars/pool-parity.json";
import { inboxSenderIdentity } from "@/features/weft/lib/inbox-sender-identity";
import { BUILTIN_FACE_SOURCE } from "@/shared/lib/avatar-packs";
import { resolveAgentFace } from "./agent-list-view";

// The hosted phone Inbox deals a face to an agent with none in Rust
// (`avatars/pool.rs`). This is the same fixture, asserted against the app's own resolvers,
// so the two can never drift: the agents list for a session (`viaInbox` absent) and the
// Inbox's own sender identity for a sender with no session (`viaInbox`).
type Case = {
  agentKey?: string;
  displayName?: string;
  handle: string;
  viaInbox?: boolean;
  expect: string;
};

describe("the face dealt to an agent with no face of its own", () => {
  it.each((cases as Case[]).filter((c) => !c.viaInbox))(
    "agents list: $agentKey / $displayName / $handle",
    (c) => {
      const row = {
        avatar: null,
        agentRevision: c.agentKey !== undefined ? `${c.agentKey}@1` : null,
        handle: c.handle,
        displayName: c.displayName,
        rowNames: undefined,
      } as unknown as Parameters<typeof resolveAgentFace>[0];
      const face = resolveAgentFace(row, { name: c.displayName ?? c.handle });
      expect(face).toMatchObject({ kind: "image", ref: c.expect });
    },
  );

  it.each((cases as Case[]).filter((c) => c.viaInbox))(
    "inbox sender: $displayName / $handle",
    (c) => {
      const identity = inboxSenderIdentity(c.handle, c.displayName ?? "");
      expect(identity.face).toMatchObject({ kind: "image", ref: c.expect });
    },
  );

  it("deals from the built-in funny pack", () => {
    expect(BUILTIN_FACE_SOURCE.pool.every((f) => f.ref.startsWith("builtin-funny:"))).toBe(true);
  });
});
