// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import { useActiveOrg } from "@/shared/stores/use-active-org";
import { buildPeopleByLogin, type PeopleByLogin } from "@/shared/lib/people-by-login";

/** SW-ID-1: the memoized login → Person index for the ACTIVE org's roster —
 *  the seam SW-ID-5 (assignee picker) and SW-ID-6 (avatars/filter) resolve
 *  `assigned_to` through, so they need no linear scan over `org.people`.
 *  Same memoized-index shape as the `personByGithubId` `useMemo` in
 *  teams-card.tsx, adapted for a nullable active org: `useActiveOrg()` is
 *  `null` when no org is active, so `people` is `undefined` and the index is
 *  empty (every lookup → `null` → initials fallback). Rebuilds only when the
 *  `people` reference changes (the store replaces `org` immutably). */
export function usePeopleByLogin(): PeopleByLogin {
  const people = useActiveOrg()?.people;
  return useMemo(() => buildPeopleByLogin(people ?? []), [people]);
}
