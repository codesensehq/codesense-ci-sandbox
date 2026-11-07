// SPDX-License-Identifier: MIT
import { toast } from "sonner";

import { locateAgentByAgentId } from "@/features/weft/lib/locate-agent";
import type { InboxTeam } from "@/tauri/commands";

import { sendAsHuman, type ConversationMessage, type RoleSummary } from "./api";
import { HUMAN, isRoleKey, machineLabel, participant, type Directory } from "./model";

/** Bring a session's terminal forward. */
export async function goToSession(handle: string): Promise<boolean> {
  try {
    const { located } = await locateAgentByAgentId(handle);
    // Not located: locateAgentByAgentId / showAgent already said why in a toast.
    return located;
  } catch (err) {
    // Callers fire and forget: a failed lookup says so instead of rejecting unhandled.
    toast(`Could not open that session: ${err instanceof Error ? err.message : String(err)}`);
    return false;
  }
}

/**
 * The team a message to `to` belongs in: where the latest message with it lives, else
 * `fallback`. A note to yourself goes to `fallback`: every message names the human on one side,
 * so the latest one says nothing about which team the note is for.
 */
export function teamFor(
  to: string,
  messages: readonly ConversationMessage[],
  fallback: InboxTeam | null,
): InboxTeam | null {
  if (to === HUMAN) return fallback;
  const role = isRoleKey(to) ? to.slice(1) : null;
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i]!;
    if (role ? m.toRole === role : m.from === to || m.to === to) return m.team;
  }
  return fallback;
}

/** Send as the human and say, in plain words, where the message went. */
export async function sendFromInbox(opts: {
  to: string;
  text: string;
  messages: readonly ConversationMessage[];
  fallbackTeam: InboxTeam | null;
  /** The team retained with an explicitly picked recipient. */
  team?: InboxTeam | null;
  dir: Directory;
  roles: readonly RoleSummary[];
  /** The message being answered; the reply goes to its team. */
  inReplyTo?: ConversationMessage | null;
  /** No toast once sent: the surface shows the message landing. What the surface does not show
   *  is still said: a message that waits for an offline machine, an idle agent woken to read it,
   *  and a role queue's note. */
  quiet?: boolean;
  /** Told why a send failed, for a surface that says so beside the message; no toast then, so
   *  a failure is reported once. `retryable` is false when the backend refused the message (the
   *  same send would be refused again), true when it could not be reached. */
  onFailed?: (why: string, retryable: boolean) => void;
}): Promise<boolean> {
  const { to, dir, roles, inReplyTo } = opts;
  // Blank lines around the message go; the first line's own indentation (code, a list) stays.
  const text = opts.text.replace(/^(?:[ \t]*\r?\n)+/, "").trimEnd();
  if (!text.trim()) return false;
  const team = inReplyTo?.team ?? opts.team ?? teamFor(to, opts.messages, opts.fallbackTeam);
  const fail = (why: string, retryable: boolean, toastText = `Could not send: ${why}`) => {
    if (opts.onFailed) opts.onFailed(why, retryable);
    else toast(toastText);
    return false;
  };
  if (!team) return fail("no team to send it in", false, "Open a team to send from the inbox.");
  let result;
  try {
    result = await sendAsHuman(team, to, text, inReplyTo?.id ?? null);
  } catch (failure) {
    return fail(failure instanceof Error ? failure.message : String(failure), true);
  }
  if (result.status !== "sent") return fail(result.message, false);
  const p = isRoleKey(to) ? null : participant(dir, to);
  if (opts.quiet && p && p.state !== "offline" && p.state !== "idle") return true;
  if (!p) {
    const r = roles.find((x) => x.role === to.slice(1));
    toast(
      r?.holder
        ? `Queued on ${to}. It stays there until ${participant(dir, r.holder).name} decides to claim it.`
        : `Queued on ${to}. No live holder yet; it stays queued until an agent claims it.`,
    );
  } else {
    toast(
      p.state === "idle"
        ? `Delivered. ${p.name} was idle and has been woken.`
        : p.state === "offline"
          ? `Queued for ${p.name}. It's delivered once ${machineLabel(p)} reconnects.`
          : `Sent to ${p.name}.`,
    );
  }
  return true;
}
