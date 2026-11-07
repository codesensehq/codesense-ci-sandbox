// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import type { Org } from "@/shared/types/domain";
import { useOrgStore } from "@/shared/stores/org-store";

/** SS-FU-24: the active `Org` object resolved from `activeOrgSlug`, or `null`
 *  when no org is active or the slug isn't in the loaded `orgs` list.
 *
 *  Centralizes the `orgs.find((o) => o.slug === activeOrgSlug)` lookup that was
 *  duplicated across ~10 call sites, so consumers that need the active org's
 *  fields (e.g. `org_root`) read it one way and can't drift. New adopters
 *  should prefer this over an inline find. */
export function useActiveOrg(): Org | null {
  const orgs = useOrgStore((s) => s.orgs);
  const activeOrgSlug = useOrgStore((s) => s.activeOrgSlug);

  return useMemo(
    () => (activeOrgSlug ? (orgs.find((o) => o.slug === activeOrgSlug) ?? null) : null),
    [orgs, activeOrgSlug],
  );
}
