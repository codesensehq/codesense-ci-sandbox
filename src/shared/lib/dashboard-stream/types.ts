// SPDX-License-Identifier: MIT
/**
 * SW-DX-3 — the in-app agent status stream contract: session (agent) deltas,
 * delivered to `session`- or `story`-scoped subscribers. Doc changes are not
 * carried here — doc views query the context engine (`subscribeContext`).
 */

import type { Phase } from "../roles";

/**
 * A subscription's window:
 * - `session`— one agent session; receives its deltas.
 * - `story`  — SW-GW-3: one STORY; receives the deltas of whatever session that
 *   story's workspace is bound to RIGHT NOW, resolved trusted-side at delivery
 *   time — so a board follows a story it never launched, without ever handling
 *   a sessionId. The terminal `signal_done` delta reaches it the same way.
 */
export type StatusScope =
  | { kind: "session"; sessionId: string }
  | { kind: "story"; storyId: string };

/** An agent `signal_done` status — `needs-attention` is first-class (SW-DX-7). */
export type SessionStatus = "success" | "failed" | "needs-attention";

/**
 * A session (agent) delta. `session` = a `signal_done` flip; `session-message` =
 * a `post_agent_message`; `progress` = a `signal_progress` (SW-GW-4) mid-run
 * phase. Carries `sessionId` (not a doc id/path) — session→story binding is
 * SW-DX-7's job; DX-3 delivers it session-scoped.
 */
export type SessionStatusDelta =
  | {
      origin: "session";
      kind: "session";
      sessionId: string;
      status: SessionStatus;
      summary?: string;
    }
  | {
      origin: "session";
      kind: "session-message";
      sessionId: string;
      role: string;
      text: string;
    }
  | {
      // SW-GW-4: a mid-run progress signal. `phase` is a fixed lifecycle value
      // (see `../roles`); `step`/`total` drive an optional N-of-M bar; `note` is
      // untrusted free text (render via textContent). Optional fields are ABSENT
      // (not null) when unset — mirror of the Rust `to_json` omit-when-None.
      origin: "session";
      kind: "progress";
      sessionId: string;
      phase: Phase;
      step?: number;
      total?: number;
      note?: string;
    };

/** A delta on the stream. */
export type StatusDelta = SessionStatusDelta;

/** Freshness metadata delivered on every callback (AC-7, the never-lie badge). */
export type StatusMeta = { fresh: boolean };

/** A subscriber callback. Invoked only when `deltas.length > 0` (AC-6). */
export type StatusCallback = (deltas: StatusDelta[], meta: StatusMeta) => void;

/** Returned by `onStatus`; idempotent on double-call (AC-3). */
export type Unsubscribe = () => void;

/** The in-app subscription verb (session/story scopes). */
export type OnStatus = (scope: StatusScope, cb: StatusCallback) => Unsubscribe;
