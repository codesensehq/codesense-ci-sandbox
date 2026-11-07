// SPDX-License-Identifier: MIT
import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef } from "react";

import {
  invalidateLandingPages,
  openTeamLandingPage,
} from "@/features/viewer/lib/team-landing-page";
import { logger } from "@/shared/lib/logger";
import { createListenerGroup } from "@/shared/lib/tauri-listeners";
import { buildTeamKey } from "@/shared/lib/team-key";
import { useActiveOrg } from "@/shared/stores/use-active-org";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  CONTEXT_FILE_CHANGE_EVENT,
  ORG_REGISTRY_CHANGED_EVENT,
  type ContextFileChangePayload,
} from "@/tauri/events";

/**
 * Open the active team's landing page whenever it has nothing open (see
 * `team-landing-page.ts`): on entering the team, and when its last tab closes.
 */
export function useTeamLandingPage(): void {
  const org = useActiveOrg();
  const teamName = useOrgStore((s) => s.activeTeamName);
  const orgLoaded = useOrgStore((s) => s.stateLoaded);
  const restored = useViewerStore((s) => s.persistedStateRestored);
  const team = org?.teams.find((t) => t.name === teamName) ?? null;
  const slug = org?.slug;
  const orgRoot = org?.org_root;
  const name = team?.name;
  const landingPage = team?.landing_page ?? null;
  // The team last entered: a re-run for the same team (an org refresh, a
  // `landing_page` edit) is not an entry, so a dismissed page stays dismissed.
  const enteredRef = useRef<string | null>(null);

  // A changed content file may add, remove or rename a landing page; a registry
  // edit (a starter page set or cleared, from the UI or an agent) may change
  // which page a team declares.
  useEffect(() => {
    const group = createListenerGroup();
    group.track(
      listen<ContextFileChangePayload>(CONTEXT_FILE_CHANGE_EVENT, (event) =>
        invalidateLandingPages(event.payload?.file_path),
      ),
    );
    group.track(listen(ORG_REGISTRY_CHANGED_EVENT, () => invalidateLandingPages()));
    return () => group.dispose();
  }, []);

  useEffect(() => {
    if (!orgLoaded || !restored || !slug || !orgRoot || !name) return;
    const teamKey = buildTeamKey(slug, name);
    const open = (opts: { justClosed?: readonly string[]; fresh?: boolean }) =>
      openTeamLandingPage({
        org: { slug, org_root: orgRoot },
        team: { name, landing_page: landingPage },
        ...opts,
      }).catch((e) => logger.warn("viewer", `team landing page failed: ${e}`));

    // (1) Entering the team (switch, launch, restart restore).
    if (enteredRef.current !== teamKey) {
      enteredRef.current = teamKey;
      void open({ fresh: true });
    }

    // (2) The team's last tab closed while the user is in it. A team teardown
    // (`closeTeam` drops it from `openedTeams` in the same update) is not a close.
    return useViewerStore.subscribe((s, prev) => {
      if (s.openTabs === prev.openTabs) return;
      const before = prev.openTabs[teamKey] ?? [];
      const after = s.openTabs[teamKey] ?? [];
      if (before.length === 0 || after.length > 0) return;
      if (prev.openedTeams.includes(teamKey) && !s.openedTeams.includes(teamKey)) return;
      void open({ justClosed: before.map((t) => t.path) });
    });
  }, [orgLoaded, restored, slug, orgRoot, name, landingPage]);

  // Leaving every team (no active team) makes the next one an entry again.
  useEffect(() => {
    if (!name) enteredRef.current = null;
  }, [name]);
}
