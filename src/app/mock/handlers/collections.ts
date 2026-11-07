// SPDX-License-Identifier: MIT
import type { HandlerRegistry } from "../mock-router";

/**
 * Team management + team-switch. Create / rename / archive / delete all route
 * through the one `commit_registry_edit_local` command; the org store applies
 * the change optimistically once it resolves, so the mock only has to succeed
 * for team CRUD to visibly work and persist for the session. Selecting a team
 * is pure front-end state; its activation side-effect (`set_active_team_for_wave`)
 * is inert here and just resolves cleanly.
 */
export function teamHandlers(): HandlerRegistry {
  return {
    commit_registry_edit_local: (args) => {
      const edit = args.edit as { type?: string };
      return { commit_message: `mock: ${edit?.type ?? "registry edit"}` };
    },
    set_active_team_for_wave: () => undefined,
  };
}
