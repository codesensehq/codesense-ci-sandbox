// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import { AppButton } from "@/shared/components/app-button";
import { isCodesenseDocs, missingContextRepoDetail } from "@/shared/lib/codesense-docs";
import { useOrgStore } from "@/shared/stores/org-store";
import type { OrgSummary } from "@/shared/types/domain";
import { detachContextRepository, listContextAttachments } from "@/tauri/commands";

import { CONTEXT_ATTACHMENTS_REFRESH } from "../lib/context-attachment-events";
import { chooseFolder, locateContextRepo } from "../lib/context-repo-actions";
import {
  beginLocalSetup,
  endLocalSetup,
  useLocalSetupActivity,
} from "../lib/local-setup-completion";

/** Connected context repos whose folder is gone (moved, deleted, unmounted). Read only
 *  while `active` (the switcher is open), and kept fresh while it stays open. */
export function useMissingContextRepos(active: boolean): OrgSummary[] {
  const [missing, setMissing] = useState<OrgSummary[]>([]);
  useEffect(() => {
    if (!active) return;
    let live = true;
    const refresh = () => {
      // Best effort: a failed read only hides the missing-folder rows.
      Promise.resolve()
        .then(listContextAttachments)
        .then(
          // The pinned docs are re-placed on launch; they never need locating.
          (rows) =>
            live &&
            setMissing(rows.filter((row) => row.availability_error && !isCodesenseDocs(row.slug))),
          () => {},
        );
    };
    refresh();
    window.addEventListener(CONTEXT_ATTACHMENTS_REFRESH, refresh);
    const subscription = listen("context-attachments-changed", refresh).catch(() => () => {});
    return () => {
      live = false;
      window.removeEventListener(CONTEXT_ATTACHMENTS_REFRESH, refresh);
      void subscription.then((unlisten) => {
        try {
          unlisten();
        } catch {
          // The event bridge is already gone (window teardown).
        }
      });
    };
  }, [active]);
  return missing;
}

/** "`{name}` can't be found at `{path}`…" with [Locate folder…] [Disconnect]. */
export function MissingContextRepos({
  rows,
  onPickerSettled,
  itemRole,
}: {
  rows: readonly OrgSummary[];
  onPickerSettled?: () => void;
  /** `menuitem` when hosted in a menu, so its buttons are items of that menu. */
  itemRole?: "menuitem";
}) {
  const busy = useLocalSetupActivity((s) => s.busy);
  const [error, setError] = useState<string | null>(null);
  if (rows.length === 0) return null;

  async function locate(row: OrgSummary) {
    setError(null);
    const path = await chooseFolder(`Locate ${row.display_name}`).catch(() => null);
    onPickerSettled?.();
    if (!path) return;
    const outcome = await locateContextRepo(row.slug, path);
    if (outcome && !outcome.ok) {
      // The stale registration is already gone; say so, so the user knows to reconnect.
      setError(
        `${row.display_name} was disconnected, but that folder couldn't be connected: ${outcome.failure.message} Use Connect a context repo… once you find it.`,
      );
    }
  }

  async function disconnect(row: OrgSummary) {
    if (!beginLocalSetup()) return;
    setError(null);
    try {
      await detachContextRepository(row.slug);
      const store = useOrgStore.getState();
      store.setOrgs(store.orgs.filter((org) => org.slug !== row.slug));
      window.dispatchEvent(new Event(CONTEXT_ATTACHMENTS_REFRESH));
    } catch (e) {
      setError(String(e));
    } finally {
      endLocalSetup();
    }
  }

  return (
    <div className="flex flex-col gap-2 px-2 py-1.5" data-testid="missing-context-repos">
      {rows.map((row) => (
        <div
          key={row.slug}
          className="space-y-1.5"
          data-testid={`missing-context-repo-${row.slug}`}
        >
          <p className="text-xs text-muted-foreground">
            <span className="font-medium text-foreground">{row.display_name}</span>{" "}
            {missingContextRepoDetail(row.org_root)}
          </p>
          <div className="flex gap-1.5">
            <AppButton
              size="xs"
              variant="outline"
              role={itemRole}
              disabled={busy}
              onClick={() => void locate(row)}
            >
              Locate folder…
            </AppButton>
            <AppButton
              size="xs"
              variant="ghost"
              role={itemRole}
              disabled={busy}
              onClick={() => void disconnect(row)}
            >
              Disconnect
            </AppButton>
          </div>
        </div>
      ))}
      {error && (
        <p role="alert" className="text-xs text-destructive">
          {error}
        </p>
      )}
    </div>
  );
}
