// SPDX-License-Identifier: MIT
import { isBetaChannel, readBuildChannel } from "./channel";
import { COPY } from "./copy";
import { useLicenseView } from "./useLicense";

/**
 * SW-PB-13 AC-5: the quiet dot on the Settings entry while a staged update waits for the
 * quit. Renders nothing outside the beta channels, where there are no license commands.
 */
export function UpdateReadyDot() {
  if (!isBetaChannel(readBuildChannel())) return null;
  return <StagedDot />;
}

function StagedDot() {
  const { view } = useLicenseView();
  if (!view?.update) return null;
  return (
    <span
      role="img"
      aria-label={COPY.updateReady(view.update.staged_version)}
      title={COPY.updateReady(view.update.staged_version)}
      data-testid="settings-update-ready-dot"
      className="ml-1.5 inline-block size-1.5 shrink-0 rounded-full bg-primary"
    />
  );
}
