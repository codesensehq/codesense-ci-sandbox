// SPDX-License-Identifier: MIT
import { LicenseLink } from "./LicenseLink";

import type { LicenseView } from "./api";
import { PUBLIC_DOWNLOAD_URL } from "./channel";
import { COPY } from "./copy";

const ATTENTION_TEXT = {
  not_writable: COPY.updateNotWritable,
  failing: COPY.updateFailing,
  install_failed: COPY.updateInstallFailed,
} as const;

/**
 * SW-PB-13: the line that says the next build has to be installed by hand. Shown when the app
 * can't replace itself, when downloads keep failing, when this build gets no updates at all,
 * and when a staged build can't be installed by Restart to update (SW-PB-23). SV-PBF-278: it
 * points to the public download, never to the beta-tester pages. Nothing while none of those
 * holds.
 */
export function UpdateByHand({ view, testIdPrefix }: { view: LicenseView; testIdPrefix: string }) {
  const reason = view.updates_off
    ? { text: COPY.updatesOff, id: "updates-off" }
    : view.update_attention &&
        view.update_attention !== "records_unwritable" &&
        (!view.update || view.update_attention === "install_failed")
      ? { text: ATTENTION_TEXT[view.update_attention], id: "update-by-hand" }
      : view.update && view.restart_ready === false
        ? // SW-PB-23: a staged build the app can't install itself now.
          { text: COPY.updateCantInstall, id: "update-by-hand" }
        : null;
  if (!reason) return null;
  return (
    <p className="text-sm text-muted-foreground" data-testid={`${testIdPrefix}-${reason.id}`}>
      {reason.text}{" "}
      <LicenseLink
        href={PUBLIC_DOWNLOAD_URL}
        label={COPY.downloadLatest}
        testId={`${testIdPrefix}-update-download`}
      />
      .
    </p>
  );
}
