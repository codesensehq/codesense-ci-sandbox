// SPDX-License-Identifier: MIT
import { liveHub } from "@/features/dashboard/lib/live-hub";
import { type SessionListRow } from "@/tauri/commands";
import type { HumanSendResult, InboxTeam } from "@/tauri/commands";

import { fixtureRollup, fixtureSessionInbox, fixtureSessions } from "./fixtures";

import {
  listInboxRollup,
  listSessionInbox,
  sendInboxAsHuman,
  type RollupDto,
  type SessionInboxDto,
} from "@/tauri/commands";

export {
  listInboxRollup,
  listSessionInbox,
  sendInboxAsHuman,
  type ConversationMessage,
  type RollupDto,
  type SessionInboxDto,
} from "@/tauri/commands";

/** A role-queue delivery's claim state, as the role inbox tracks it. */
export type RoleDeliveryClaimState = NonNullable<
  import("@/tauri/commands").ConversationMessage["delivery"]
>["state"];

/** One role queue as the rollup lists it. */
export type RoleSummary = RollupDto["roles"][number];

/** Where a read came from: the backend, or the dev-only demo data while the command is missing. */
export type InboxSource = "live" | "fixture";

/**
 * True when `err` says the backend has no such command (an app built before the inbox read
 * model landed) — or, in a dev build only, that there is no Tauri runtime at all (a plain
 * browser). Anything else is a real failure that must surface, never be papered over.
 */
export function isCommandUnavailable(err: unknown, command: string): boolean {
  const text = err instanceof Error ? err.message : String(err);
  if (text.includes(command) && /not found|unknown command|not mocked/i.test(text)) return true;
  return import.meta.env.DEV && /__TAURI_INTERNALS__|reading 'invoke'/.test(text);
}

/** Demo data is a development aid only: a production build never shows it. */
const fixtureAllowed = () => import.meta.env.DEV || import.meta.env.MODE === "test";

export async function loadSessionInbox(
  handle: string,
): Promise<{ dto: SessionInboxDto; source: InboxSource }> {
  try {
    return { dto: await listSessionInbox(handle), source: "live" };
  } catch (err) {
    if (fixtureAllowed() && isCommandUnavailable(err, "list_session_inbox")) {
      return { dto: fixtureSessionInbox(handle), source: "fixture" };
    }
    throw err;
  }
}

export async function loadInboxRollup(): Promise<{ dto: RollupDto; source: InboxSource }> {
  try {
    return { dto: await listInboxRollup(), source: "live" };
  } catch (err) {
    if (fixtureAllowed() && isCommandUnavailable(err, "list_inbox_rollup")) {
      return { dto: fixtureRollup(), source: "fixture" };
    }
    throw err;
  }
}

/** The session roster. Falls back to the demo roster exactly when the inbox reads would. */
export async function loadInboxSessions(): Promise<SessionListRow[]> {
  try {
    return (await liveHub.snapshot("sessions", "all")).snapshot as unknown as SessionListRow[];
  } catch (err) {
    if (fixtureAllowed() && isCommandUnavailable(err, "live_hub_subscribe"))
      return fixtureSessions();
    throw err;
  }
}

/** How a composer key is addressed on the wire: `#role`, `human`, else `@handle`. */
export function wireRecipient(to: string): string {
  if (to.startsWith("#") || to === "human" || to.startsWith("@")) return to;
  return `@${to}`;
}

/** Send as the human to a session, a role queue or yourself, optionally replying to a message. */
export function sendAsHuman(
  team: InboxTeam,
  to: string,
  text: string,
  inReplyTo: number | null = null,
): Promise<HumanSendResult> {
  return sendInboxAsHuman(
    team,
    wireRecipient(to),
    text,
    "markdown",
    null,
    null,
    null,
    null,
    inReplyTo,
  );
}
