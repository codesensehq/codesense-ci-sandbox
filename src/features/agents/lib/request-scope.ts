// SPDX-License-Identifier: MIT
import { useOrgStore } from "@/shared/stores/org-store";

/** The team a request made now is addressed to: the one on screen. */
export function requestScope(): string | null {
  const { activeOrgSlug, activeTeamName } = useOrgStore.getState();
  return activeOrgSlug && activeTeamName ? `${activeOrgSlug}:${activeTeamName}` : null;
}
