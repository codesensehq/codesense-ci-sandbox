// SPDX-License-Identifier: MIT
/** "1 agent run without a terminal will stop." / "N agent runs without a terminal will stop." */
function runsStop(runs: number): string {
  return `${runs} agent ${runs === 1 ? "run" : "runs"} without a terminal will stop.`;
}

export const COPY = {
  checkNow: "Check now",
  browserFailed: "Couldn't open your browser. Copy this URL and open it in your browser.",
  copyUrl: "Copy",
  copiedUrl: "Copied",
  copyFailed: "Couldn't copy. Select the URL and copy it.",
  /** SV-PBF-278: Settings → Updates. */
  updatesTitle: "Updates",
  updatesIdle: "CodeSense checks for a new version daily and installs it when you quit.",
  /** SV-PBF-278: Check now started a background check. */
  checkStarted: "Checking. A new version downloads in the background.",
  checkFailed: "Could not check. Try again.",
  /** SW-PB-13 AC-5: a build the background updater downloaded and verified. */
  updateReady: (version: string) =>
    `Update ready (${version}) — it installs when you quit CodeSense`,
  releaseNotes: "Release notes",
  hideReleaseNotes: "Hide release notes",
  updateNotWritable:
    "A new build is out, but CodeSense can't replace itself where it's installed without an administrator.",
  updateFailing: "CodeSense couldn't download the new build.",
  /** SW-PB-23: installing the offered build at a restart failed. */
  updateInstallFailed: "CodeSense couldn't install the new build.",
  /** SV-PBF-278: the by-hand line's link to the public download. */
  downloadLatest: "Download the latest CodeSense from codesense.ai",
  updatesOff: "Updates are off in this build.",
  /** SW-PB-23 AC-1: install the staged build and relaunch, the user's own action. */
  restartToUpdate: "Restart to update",
  restarting: "Restarting...",
  restartFailed: "Couldn't restart. Try again, or quit CodeSense to install the update.",
  /** The click was accepted but CodeSense was still open half a minute later. */
  restartTimedOut: "CodeSense didn't restart. Try again, or quit CodeSense to install the update.",
  /** The staged build can't be installed by the app itself (Restart to update isn't offered). */
  updateCantInstall: "CodeSense can't install this update by itself.",
  /** Restart to update while agent runs without a terminal are going: the user decides. */
  confirmRestartRuns: (runs: number) => `${runsStop(runs)} Restart anyway?`,
  /** The count couldn't be read: ask anyway. */
  confirmRestartRunsUnknown: "Agent runs without a terminal may stop. Restart anyway?",
  /** SW-PB-23: the update records can't be saved; only the user's restart works. `button`
   *  names the restart button shown with the note, if one is. */
  recordsUnwritable: (button: string | null) =>
    `CodeSense can't save its update records, so it won't restart itself for a required update.${button ? ` ${button} still works.` : ""}`,
  restartAnyway: "Restart anyway",
  cancel: "Cancel",
} as const;
