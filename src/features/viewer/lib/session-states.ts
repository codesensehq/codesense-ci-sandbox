// SPDX-License-Identifier: MIT
import { agentCliLabel } from "@/shared/lib/agent-clis";
import type { AgentSessionRow } from "@/tauri/commands";

import { HAND_STARTED_KIND } from "./hand-agents";

/**
 * What a session says about how CodeSense reaches it, from facts every CLI's session carries:
 * the roster kind, whether an inbox waiter is armed, and the release its launch recorded. No
 * rule here names a CLI.
 */

/** The words a session typed by hand wears on its row and tab. */
export const TYPED_BY_HAND = "Typed by hand";
/** What a session whose CLI release is outside the qualified range says. */
export const RELEASE_NOT_SUPPORTED = "Release not supported";
/** The tab's mail-off glyph: its name and its tooltip. */
export const TYPED_BY_HAND_TAB = `${TYPED_BY_HAND} · Mail waits; CodeSense can't wake it`;
/** The launch warning a row quotes when its note is missing. */
const RELEASE_NOTE_FALLBACK =
  "This CLI release isn't supported yet. Status, inbox wake and usage may not work.";

/** A tab a person typed a CLI into, which CodeSense can't wake: mail waits for it unannounced.
 *  One whose CLI armed a waiter says nothing extra. */
export function isTypedByHand(row: Pick<AgentSessionRow, "kind" | "inboxWaiter">): boolean {
  return row.kind === HAND_STARTED_KIND && row.inboxWaiter === false;
}

/** The launch warning of a session whose CLI release CodeSense is not qualified on, else null. */
export function releaseWarning(
  row: Pick<AgentSessionRow, "releaseQualified" | "releaseNote">,
): string | null {
  return row.releaseQualified === false ? (row.releaseNote ?? RELEASE_NOTE_FALLBACK) : null;
}

/** The notice line's words before its action: `Typed by hand · 2 unread, not woken`. */
export function typedByHandNotice(unread: number): string {
  return unread > 0
    ? `${TYPED_BY_HAND} · ${unread} unread, not woken`
    : `${TYPED_BY_HAND} · not woken`;
}

/** The row's tooltip for a session typed by hand: what it is, what its mail does, the action. */
export function typedByHandHint(cli: string | null, unread: number): string {
  const mail =
    unread >= 2
      ? `Its ${unread} unread messages are stored and readable here, but nothing wakes it.`
      : unread === 1
        ? "Its 1 unread message is stored and readable here, but nothing wakes it."
        : "Mail sent to it is stored and readable, but nothing wakes it.";
  return [
    `${TYPED_BY_HAND}, so CodeSense doesn't manage it.`,
    mail,
    `${startManagedLabel(cli)} opens a new session with inbox wake and activity; this tab and its mail stay as they are.`,
  ].join(" ");
}

/** The Start managed action's name, which says which CLI it starts. */
export function startManagedLabel(cli: string | null): string {
  return cli ? `Start managed ${agentCliLabel(cli)}` : "Start managed";
}

/** The Check version action's name. */
export function checkVersionLabel(cli: string | null): string {
  return cli ? `Check ${agentCliLabel(cli)} version` : "Check version";
}

/** The row's tooltip for an unsupported release: the launch warning verbatim, then what still
 *  works, and where Check version goes when the row offers it. */
export function releaseHint(warning: string, canCheck: boolean): string {
  return [
    warning,
    canCheck
      ? "Mail is stored and readable. Check version opens Settings → CLIs, which shows the installed version."
      : "Mail is stored and readable.",
  ].join("\n");
}
