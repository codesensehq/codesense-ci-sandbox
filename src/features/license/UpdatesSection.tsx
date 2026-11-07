// SPDX-License-Identifier: MIT
import { useState } from "react";

import { Button } from "@/components/ui/button";

import { licenseCheckNow } from "./api";
import { isBetaChannel, readBuildChannel } from "./channel";
import { COPY } from "./copy";
import { RestartToUpdateButton } from "./RestartToUpdateButton";
import { UpdateByHand } from "./UpdateByHand";
import { useLicenseView } from "./useLicense";

/**
 * SV-PBF-278: Settings → Updates in the beta channels. The build the background updater staged,
 * its release notes, Restart to update, and the install-by-hand line. Nothing about a license.
 */
export function UpdatesSection() {
  if (!isBetaChannel(readBuildChannel())) return null;
  return <Updates />;
}

function Updates() {
  const { view } = useLicenseView();
  const [notesOpen, setNotesOpen] = useState(false);
  const [checkNote, setCheckNote] = useState<string | null>(null);
  if (!view) return null;
  // The check runs in the background; a build it finds arrives as a state change.
  const checkNow = () => {
    setCheckNote(null);
    licenseCheckNow().then(
      () => setCheckNote(COPY.checkStarted),
      () => setCheckNote(COPY.checkFailed),
    );
  };
  const update = view.update;
  return (
    <section className="space-y-2 py-2" data-testid="settings-updates-section">
      <h3 className="text-sm font-medium">{COPY.updatesTitle}</h3>
      {!update && !view.updates_off && (
        <p className="text-sm text-muted-foreground" data-testid="settings-updates-idle">
          {COPY.updatesIdle}
        </p>
      )}
      {update && (
        <div className="space-y-1 text-sm" data-testid="settings-updates-ready">
          <p>
            {COPY.updateReady(update.staged_version)}
            {update.notes.trim() !== "" && (
              <>
                {" "}
                <button
                  type="button"
                  className="underline underline-offset-2 hover:text-foreground"
                  aria-expanded={notesOpen}
                  onClick={() => setNotesOpen((open) => !open)}
                  data-testid="settings-updates-release-notes"
                >
                  {notesOpen ? COPY.hideReleaseNotes : COPY.releaseNotes}
                </button>
              </>
            )}
          </p>
          {notesOpen && (
            <p
              className="whitespace-pre-wrap text-muted-foreground"
              data-testid="settings-updates-release-notes-text"
            >
              {update.notes}
            </p>
          )}
          {view.restart_ready && <RestartToUpdateButton testId="settings-updates-restart" />}
        </div>
      )}
      {!update && !view.updates_off && (
        <div className="flex flex-wrap items-center gap-3">
          <Button variant="outline" onClick={checkNow} data-testid="settings-updates-check">
            {COPY.checkNow}
          </Button>
          {checkNote && (
            <span
              className="text-xs text-muted-foreground"
              data-testid="settings-updates-check-note"
            >
              {checkNote}
            </span>
          )}
        </div>
      )}
      <UpdateByHand view={view} testIdPrefix="settings-updates" />
      {view.update_attention === "records_unwritable" && (
        <p
          className="text-sm text-muted-foreground"
          data-testid="settings-updates-records-unwritable"
        >
          {COPY.recordsUnwritable(update && view.restart_ready ? COPY.restartToUpdate : null)}
        </p>
      )}
    </section>
  );
}
