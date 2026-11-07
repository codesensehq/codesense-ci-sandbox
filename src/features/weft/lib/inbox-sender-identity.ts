// SPDX-License-Identifier: MIT
/**
 * SW-BSI-3 avatar fidelity — resolve an inbox message sender to the exact shape
 * {@link AgentAvatar}/{@link AgentFaceView} consume (`AgentFace` + `AgentRole`).
 *
 * We don't have an `AgentSessionRow` for an inbox sender — just `from` (an agent
 * id, a session id, or the literal "human") and `fromName` (a display name).
 * We synthesize the minimum the resolvers need: `handle` from `from`,
 * `displayName` from `fromName`, no `avatar` and no `agentRevision` (both
 * optional on the Pick shape the resolver accepts).
 *
 * An agent id falls through to a stably-dealt pool face
 * (same face every time for the same id) and the "worker" piece (we have no
 * launch tree here, so we can't say "manager"; the Agents tab's richer renderer
 * wins there — this is the inbox's own compact lens).
 *
 * Humans never wear a chess piece — we return a face-only identity for them.
 * When the caller passes `humanIdentity`, the human sender's mark face is keyed
 * to the viewer's own display name (the identity-menu one), not the stored
 * literal `"human"` in `fromName`. The person who authored the message sees
 * their initials on their own messages instead of a generic "H" or "HU".
 */

import { findAgent } from "@/features/agents/lib/agent-names";
import type { AgentRow } from "@/tauri/commands";
import {
  initialsOf,
  resolveAgentFace,
  resolveAgentRole,
  type AgentFace,
  type AgentRole,
} from "@/features/viewer/lib/agent-list-view";
import { BUILTIN_FACE_SOURCE, parseFaceRef, type FaceSource } from "@/shared/lib/avatar-packs";

/** The literal `from` value the G-MSG mailbox uses for a human sender. Mirrors
 *  `src-tauri/src/weft/inbox.rs` `HUMAN`. */
const HUMAN_FROM = "human";

/** Stable 12-way hue for a human sender's mark face, same hashing as the Phase 2
 *  helper so the colour doesn't shift when a reader goes from list → thread. */
export function humanHue(name: string): number {
  let h = 5381;
  for (let i = 0; i < name.length; i++) {
    h = ((h << 5) + h + name.charCodeAt(i)) | 0;
  }
  return Math.abs(h) % 360;
}

/** The resolved sender identity: a face always, and a role only for agents. */
export type InboxSenderIdentity =
  | { kind: "human"; face: AgentFace }
  | { kind: "agent"; face: AgentFace; role: AgentRole };

/**
 * The face of the agent each sender id names in `teamKey`'s inbox, by id. An inbox message's
 * `from` is an agent's bare id; it names the agent a launch of that id in the team finds (the
 * team's own, then your personal one, then the starter, by full key), so a same-named agent of
 * another team never lends its face. Agents without a face of their own map to `null`, so the sender is dealt a stable
 * face.
 */
export function agentsAvatarIndex(
  rows: readonly AgentRow[],
  teamKey: string | null,
): Map<string, string | null> {
  const map = new Map<string, string | null>();
  for (const row of rows) {
    const id = row.key.slice(row.key.lastIndexOf("/") + 1);
    if (map.has(id)) continue;
    map.set(id, findAgent(id, rows, teamKey)?.avatar ?? null);
  }
  return map;
}

/** The viewer's own identity, used so a human-sent message wears the viewer's
 *  initials and hue (or chosen face) instead of the stored literal "human".
 *  `displayName` is the identity-menu display name (the local profile's).
 *  `face` is the viewer's edited face: a `pack:id` the pack index resolves, or
 *  a `data:` URL the renderer draws directly; null falls back to initials. */
export interface HumanIdentity {
  displayName: string | null;
  face?: string | null;
}

/** Map an inbox message's `from` + `fromName` to the face/role shape the agent
 *  identity renderer takes. HUMAN → mark face with initials in a stable oklch
 *  hue, keyed to the viewer's own display name (`humanIdentity.displayName`)
 *  when supplied — so a human-authored message shows the author's initials,
 *  not a generic "H" dealt from the literal `"human"` id. Any other `from`
 *  resolves through the agent-list-view face/role resolvers. When `agentsAvatars` carries the sender's
 *  agent id, the configured face wins over the pool-dealt one; absent (unknown
 *  id, or no agents list threaded through) keeps the deterministic pool
 *  fallback. */
export function inboxSenderIdentity(
  from: string,
  fromName: string,
  agentsAvatars?: Map<string, string | null>,
  humanIdentity?: HumanIdentity,
  faces: FaceSource = BUILTIN_FACE_SOURCE,
): InboxSenderIdentity {
  if (from === HUMAN_FROM) {
    const humanName = humanIdentity?.displayName?.trim() || fromName.trim() || from.trim() || "?";
    const chosen = humanIdentity?.face?.trim() || null;
    if (chosen) {
      if (chosen.startsWith("data:")) {
        return { kind: "human", face: { kind: "image", ref: chosen, src: chosen } };
      }
      if (parseFaceRef(chosen)) {
        const src = faces.resolve(chosen);
        if (src) return { kind: "human", face: { kind: "image", ref: chosen, src } };
      }
    }
    return {
      kind: "human",
      face: {
        kind: "mark",
        text: initialsOf(humanName),
        color: `oklch(60% 0.11 ${humanHue(humanName)})`,
      },
    };
  }
  const name = fromName.trim() || from.trim() || "?";
  const configuredAvatar = agentsAvatars?.get(from) ?? null;
  const face = resolveAgentFace(
    {
      avatar: configuredAvatar,
      agentRevision: null,
      handle: from,
      displayName: name,
    },
    { name },
  );
  const role = resolveAgentRole({ role: null }, { launched: 0 });
  return { kind: "agent", face, role };
}
