// SPDX-License-Identifier: MIT
import { buildTeamKey } from "@/shared/lib/team-key";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

import { CODESENSE_DOCS_SLUG, CODESENSE_DOCS_TEAM } from "@/shared/lib/codesense-docs";

/**
 * Runs `decide` once the org and viewer stores have restored the persisted session, so the
 * launch decision (reopen the last context repo vs. first launch) never races the restore,
 * however long it takes: picking the docs before the last-opened repo is known would
 * overwrite it. Returns a cancel. (Both stores always settle: a failed restore still marks
 * itself loaded, and App's boot watchdog forces a stuck viewer restore.)
 */
export function afterPersistedSelectionRestored(decide: () => void): () => void {
  const loaded = () => useOrgStore.getState().stateLoaded && useViewerStore.getState().stateLoaded;
  if (loaded()) {
    decide();
    return () => {};
  }
  let done = false;
  const check = () => {
    if (done || !loaded()) return;
    done = true;
    stop();
    decide();
  };
  const unsubscribeOrg = useOrgStore.subscribe(check);
  const unsubscribeViewer = useViewerStore.subscribe(check);
  const stop = () => {
    unsubscribeOrg();
    unsubscribeViewer();
  };
  return () => {
    done = true;
    stop();
  };
}

/**
 * The very first launch: open the CodeSense docs repo's "Getting started" team with its
 * README as the active tab. No modal, no welcome screen.
 */
export function openCodesenseDocsReadme(readmePath: string): void {
  const org = useOrgStore.getState();
  if (org.activeOrgSlug !== CODESENSE_DOCS_SLUG || org.activeTeamName !== CODESENSE_DOCS_TEAM) {
    org.setActiveTeamIn(CODESENSE_DOCS_SLUG, CODESENSE_DOCS_TEAM);
  }
  const { activeOrgSlug, activeTeamName } = useOrgStore.getState();
  if (activeOrgSlug !== CODESENSE_DOCS_SLUG || activeTeamName !== CODESENSE_DOCS_TEAM) return;
  const teamKey = buildTeamKey(CODESENSE_DOCS_SLUG, CODESENSE_DOCS_TEAM);
  const viewer = useViewerStore.getState();
  // A landing, not the user's open: full-screen CLI stays on.
  viewer.openFile(teamKey, "", readmePath, { keepLayout: true });
}
