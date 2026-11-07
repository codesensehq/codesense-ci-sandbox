// SPDX-License-Identifier: MIT
import { logger } from "@/shared/lib/logger";

/**
 * The agents a person pinned to the top of the agents pane. Pins are per person, kept per team
 * with the other viewer settings; they are never shared with the team, so no agent becomes "the
 * team's special agent".
 *
 * A pin holds one session, by its team and handle (one handle can run in two teams): the pinned
 * row stays on top while that session runs, and the pin goes when the session ends.
 */
export interface AgentPin {
  kind: "session";
  /** The team the session runs in: the team whose list keeps the pin. */
  teamKey: string;
  handle: string;
  /** The session's id, so a pin whose tab is still restoring is not dropped early. */
  sessionId: string | null;
  name: string;
}

/** A pin's identity: its session's team and handle ({@link sessionPinId}). What Unpin removes,
 *  and what a row of that team and handle is matched by. */
export function agentPinId(pin: AgentPin): string {
  return sessionPinId(pin.teamKey, pin.handle);
}

/** The pin id of one team's session, by its handle. */
export function sessionPinId(teamKey: string, handle: string): string {
  return `session:${agentRowKey(teamKey, handle)}`;
}

/** One agent's key across teams: its team and its handle, since one handle can run in two teams.
 *  Every set or map of the pane's agents is keyed by it. A team key can hold a `:`, so the two are
 *  joined as a JSON pair, which no two different pairs share. */
export function agentRowKey(teamKey: string, handle: string): string {
  return JSON.stringify([teamKey, handle]);
}

/** The pins a saved settings file holds, keeping only well-formed ones. A pin that names no team
 *  is dropped, and so is a second pin of one session: the first of each id and of each session id
 *  is kept. */
export function sanitizeAgentPins(raw: unknown): AgentPin[] {
  return dedupePins(Array.isArray(raw) ? raw : [], null, new Set(), new Set());
}

/** Every team's saved pins ({@link sanitizeAgentPins} for each, keeping only the pins of the
 *  team whose list holds them), with a session pinned in two teams' lists kept only in the
 *  first. A team left with no pins has no entry. */
export function sanitizeAgentPinMap(raw: unknown): Record<string, AgentPin[]> {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return {};
  const ids = new Set<string>();
  const sessionIds = new Set<string>();
  const map: Record<string, AgentPin[]> = {};
  for (const [teamKey, list] of Object.entries(raw as Record<string, unknown>)) {
    const pins = dedupePins(Array.isArray(list) ? list : [], teamKey, ids, sessionIds);
    if (pins.length > 0) map[teamKey] = pins;
  }
  return map;
}

function dedupePins(
  raw: readonly unknown[],
  listTeam: string | null,
  ids: Set<string>,
  sessionIds: Set<string>,
): AgentPin[] {
  const pins: AgentPin[] = [];
  for (const item of raw) {
    const pin = toPin(item, listTeam);
    if (!pin) continue;
    const id = agentPinId(pin);
    if (ids.has(id) || (pin.sessionId !== null && sessionIds.has(pin.sessionId))) {
      logger.debug(
        "agents",
        `dropped a second saved pin of ${pin.teamKey} ${pin.handle} (session ${pin.sessionId ?? "none"})`,
      );
      continue;
    }
    ids.add(id);
    if (pin.sessionId !== null) sessionIds.add(pin.sessionId);
    pins.push(pin);
  }
  return pins;
}

/** One saved pin, or null when it is not one. In a team's list (`listTeam`) only that team's
 *  pins are: pins are only ever added to their own team's list, and a pin the list and its team
 *  disagree on would be kept by one team and matched as another's. */
function toPin(item: unknown, listTeam: string | null): AgentPin | null {
  if (!item || typeof item !== "object") return null;
  const o = item as Record<string, unknown>;
  if (o.kind !== "session" || typeof o.handle !== "string" || !o.handle) return null;
  if (typeof o.teamKey !== "string" || !o.teamKey) return null;
  if (listTeam !== null && o.teamKey !== listTeam) return null;
  return {
    kind: "session",
    teamKey: o.teamKey,
    handle: o.handle,
    sessionId: typeof o.sessionId === "string" ? o.sessionId : null,
    name: typeof o.name === "string" ? o.name : "",
  };
}
