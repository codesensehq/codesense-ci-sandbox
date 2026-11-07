// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { agentsAvatarIndex, humanHue, inboxSenderIdentity } from "./inbox-sender-identity";
import type { AgentRow } from "@/tauri/commands";

const TEAM = "acme/api:backend";
/** An agents-list row: a team agent of `acme/api`'s `org/team`, a personal one or a starter. */
const agentRow = (key: string, avatar: string | null): AgentRow => {
  const scope = key.startsWith("builtin/")
    ? "builtin"
    : key.startsWith("_personal/")
      ? "personal"
      : "team";
  const team = scope === "team" ? key.split("/")[2] : "";
  return { key, avatar, scope, org: scope === "team" ? "acme/api" : "", team } as AgentRow;
};
const index = (rows: [string, string | null][]) =>
  agentsAvatarIndex(
    rows.map(([k, a]) => agentRow(k, a)),
    TEAM,
  );

describe("inbox-sender-identity", () => {
  describe("inboxSenderIdentity", () => {
    it("classifies the literal 'human' as a human with a mark face", () => {
      // Single-word name: initialsOf returns the first two letters uppercased.
      const id = inboxSenderIdentity("human", "Alice");
      expect(id.kind).toBe("human");
      if (id.kind !== "human") throw new Error("narrowing");
      expect(id.face.kind).toBe("mark");
      if (id.face.kind !== "mark") throw new Error("narrowing");
      expect(id.face.text).toBe("AL");
      expect(id.face.color).toMatch(/^oklch\(/);
    });

    it("takes initials of two words for a human sender", () => {
      const id = inboxSenderIdentity("human", "Alice Smith");
      expect(id.kind).toBe("human");
      if (id.kind !== "human") throw new Error("narrowing");
      if (id.face.kind !== "mark") throw new Error("narrowing");
      expect(id.face.text).toBe("AS");
    });

    it("prefers the viewer's identity-menu display name over the stored 'human' fromName", () => {
      // Backend persists `fromName = "human"` for human-authored messages; the
      // identity-menu display name is the author's. The resolver must key the
      // mark on the viewer's name so the author sees their own initials, not
      // "HU" dealt from the literal id.
      const id = inboxSenderIdentity("human", "human", undefined, {
        displayName: "Ada Lovelace",
      });
      expect(id.kind).toBe("human");
      if (id.kind !== "human") throw new Error("narrowing");
      if (id.face.kind !== "mark") throw new Error("narrowing");
      expect(id.face.text).toBe("AL");
    });

    it("falls back to the stored fromName when the viewer has no display name yet", () => {
      // Sign-in flash: `useCurrentUser()` returns null before the local profile
      // loads. The resolver still returns a stable mark using whatever name the
      // row carries, instead of crashing or showing "?".
      const id = inboxSenderIdentity("human", "Alice", undefined, { displayName: null });
      expect(id.kind).toBe("human");
      if (id.kind !== "human") throw new Error("narrowing");
      if (id.face.kind !== "mark") throw new Error("narrowing");
      expect(id.face.text).toBe("AL");
    });

    it("renders a chosen pack face when the human identity carries one", () => {
      const id = inboxSenderIdentity("human", "human", undefined, {
        displayName: "Sam",
        face: "builtin-funny:sysadmin-goblin",
      });
      expect(id.kind).toBe("human");
      if (id.kind !== "human") throw new Error("narrowing");
      expect(id.face.kind).toBe("image");
      if (id.face.kind !== "image") throw new Error("narrowing");
      expect(id.face.ref).toBe("builtin-funny:sysadmin-goblin");
    });

    it("renders a data URL identity avatar directly", () => {
      const dataUrl = "data:image/png;base64,abcd";
      const id = inboxSenderIdentity("human", "human", undefined, {
        displayName: "Sam",
        face: dataUrl,
      });
      expect(id.kind).toBe("human");
      if (id.kind !== "human") throw new Error("narrowing");
      expect(id.face.kind).toBe("image");
      if (id.face.kind !== "image") throw new Error("narrowing");
      expect(id.face.src).toBe(dataUrl);
      expect(id.face.ref).toBe(dataUrl);
    });

    it("falls back to initials when a pack face isn't available here", () => {
      const id = inboxSenderIdentity("human", "human", undefined, {
        displayName: "Sam",
        face: "unknown-pack:nope",
      });
      expect(id.kind).toBe("human");
      if (id.kind !== "human") throw new Error("narrowing");
      expect(id.face.kind).toBe("mark");
    });

    it("gives an agent named controller a dealt face and the worker piece, like any agent", () => {
      const id = inboxSenderIdentity("controller", "Controller");
      expect(id.kind).toBe("agent");
      if (id.kind !== "agent") throw new Error("narrowing");
      expect(id.face.kind).toBe("image");
      expect(id.role.piece).toBe("worker");
    });

    it("gives any other agent id a dealt pool face and the worker piece", () => {
      const id = inboxSenderIdentity("code-reviewer", "Code reviewer");
      expect(id.kind).toBe("agent");
      if (id.kind !== "agent") throw new Error("narrowing");
      // Any agent without a declared avatar gets a stably-dealt image face from
      // the built-in pool. The exact ref depends on the hash of the agent key —
      // we only assert it is an image face and the piece is worker (no tree info).
      expect(id.face.kind).toBe("image");
      expect(id.role.piece).toBe("worker");
    });

    it("falls back to the agent id when the display name is empty", () => {
      const id = inboxSenderIdentity("session-xyz", "");
      expect(id.kind).toBe("agent");
      if (id.kind !== "agent") throw new Error("narrowing");
      // Stable face, no crash on empty fromName.
      expect(id.face).toBeDefined();
    });

    it("uses the configured avatar when the agents list carries one", () => {
      // An agent whose definition declared a specific face in the Agents tab
      // should render that face on inbox rows, not the deterministic pool-dealt
      // one — this is the fidelity gap PR #1862 explicitly deferred. Pick a face
      // the resolver's shipped `BUILTIN_FACE_SOURCE` actually knows so the
      // configured-ref path flows end-to-end instead of silently falling back.
      const avatars = index([["acme/api/backend/code-reviewer", "builtin-funny:sysadmin-goblin"]]);
      const id = inboxSenderIdentity("code-reviewer", "Code reviewer", avatars);
      expect(id.kind).toBe("agent");
      if (id.kind !== "agent") throw new Error("narrowing");
      expect(id.face.kind).toBe("image");
      if (id.face.kind !== "image") throw new Error("narrowing");
      // The resolved face carries the ref the agent definition chose.
      expect(id.face.ref).toBe("builtin-funny:sysadmin-goblin");
    });

    it("shows a team agent's own face, never a face-less same-id agent listed after it", () => {
      const avatars = index([
        ["acme/api/backend/manager", "builtin-funny:sysadmin-goblin"],
        ["_personal/1/personal/manager", null],
      ]);
      const id = inboxSenderIdentity("manager", "Ricky", avatars);
      if (id.kind !== "agent" || id.face.kind !== "image") throw new Error("narrowing");
      expect(id.face.ref).toBe("builtin-funny:sysadmin-goblin");
      expect(id.role).toEqual({ piece: "worker", label: "Worker" });
    });

    it("falls back to the pool when the agents list has no entry for the sender", () => {
      // A sender that isn't in the agents list (unknown id, legacy session-addressed
      // message, a copy that didn't make it to the list yet) still gets a stable
      // face from the pool. The ref the configured avatar DOES carry here belongs
      // to a different agent — this one dealing against its own key must land on
      // whatever the pool rule picks, not the other agent's face.
      const avatars = index([["acme/api/backend/other-agent", "builtin-funny:sysadmin-goblin"]]);
      const id = inboxSenderIdentity("code-reviewer", "Code reviewer", avatars);
      expect(id.kind).toBe("agent");
      if (id.kind !== "agent") throw new Error("narrowing");
      expect(id.face.kind).toBe("image");
      if (id.face.kind !== "image") throw new Error("narrowing");
      // Does NOT inherit the configured avatar of a different agent.
      expect(id.face.ref).not.toBe("builtin-funny:sysadmin-goblin");
    });

    it("falls back to the pool when the agents list carries a null avatar", () => {
      // An agent without a configured face still has a row in the agents list
      // (avatar = null). The resolver should pool-deal rather than crash.
      const avatars = index([["acme/api/backend/code-reviewer", null]]);
      const id = inboxSenderIdentity("code-reviewer", "Code reviewer", avatars);
      expect(id.kind).toBe("agent");
      if (id.kind !== "agent") throw new Error("narrowing");
      expect(id.face.kind).toBe("image");
    });

    it("falls back to the pool when the agents list carries an unknown pack", () => {
      // The configured avatar is only honoured if the pack is on this machine.
      // A `pack:id` the resolver's `BUILTIN_FACE_SOURCE` doesn't know (a team pack
      // that hasn't synced, say) still deals a stable pool face so the row never
      // renders as a broken image.
      const avatars = index([["acme/api/backend/code-reviewer", "team-only-pack:some-face"]]);
      const id = inboxSenderIdentity("code-reviewer", "Code reviewer", avatars);
      expect(id.kind).toBe("agent");
      if (id.kind !== "agent") throw new Error("narrowing");
      expect(id.face.kind).toBe("image");
    });
  });

  describe("agentsAvatarIndex", () => {
    it("maps each sender id to the face of the agent it names in this team", () => {
      const map = index([
        // Your personal copy with no face…
        ["_personal/1/acme/api/_scratch/code-reviewer", null],
        // …then the team's agent with a face: the team's is the one the id names.
        ["acme/api/backend/code-reviewer", "builtin-funny:yaml-wizard"],
        // Another team's agent of the same id never lends its face.
        ["acme/api/frontend/code-reviewer", "builtin-funny:tie-cat"],
        ["acme/api/frontend/designer", "builtin-funny:tie-cat"],
        ["builtin/claude", null],
      ]);
      expect(map.get("code-reviewer")).toBe("builtin-funny:yaml-wizard");
      expect(map.get("claude")).toBeNull();
      expect(map.get("designer")).toBeNull();
      expect(map.has("missing")).toBe(false);
    });
  });

  describe("humanHue", () => {
    it("returns the same hue for the same name", () => {
      expect(humanHue("alice")).toBe(humanHue("alice"));
    });
    it("distinguishes different names", () => {
      expect(humanHue("alice")).not.toBe(humanHue("bob"));
    });
    it("stays in 0..359", () => {
      const h = humanHue("alice");
      expect(h).toBeGreaterThanOrEqual(0);
      expect(h).toBeLessThan(360);
    });
  });
});
