// SPDX-License-Identifier: MIT
import { toast } from "sonner";

import { openSettingsTab } from "@/features/settings/lib/open-settings-tab";
import { requestNewWorkspaceTerminal } from "@/features/viewer/lib/new-workspace-terminal";
import {
  checkVersionLabel,
  RELEASE_NOT_SUPPORTED,
  startManagedLabel,
  typedByHandNotice,
} from "@/features/viewer/lib/session-states";
import { isAgentRuntime } from "@/shared/lib/agent-runtimes";
import {
  selectHiddenTabLocation,
  selectOpenTabLocation,
  useViewerStore,
} from "@/shared/stores/viewer-store";
import { LOCAL_MACHINE_ID, sessionCwd } from "@/tauri/commands";

/** What Start managed says when the tab typed by hand is no longer in any workspace. */
const NO_TAB_FOR_START =
  "Couldn't find this tab's workspace. Open New terminal there to start it managed.";

/** Open New terminal on `sessionId`'s own machine with `cli` and the folder the tab is in now,
 *  for a new managed session. `reveal` first shows the tab's workspace, whose chooser mounts
 *  only while it is shown. The tab typed by hand, its process and its mail stay as they are. */
async function startManaged(sessionId: string, cli: string, reveal: () => void): Promise<void> {
  const viewer = useViewerStore.getState();
  const home =
    selectOpenTabLocation(viewer, sessionId) ?? selectHiddenTabLocation(viewer, sessionId);
  if (!home) {
    toast.error(NO_TAB_FOR_START);
    return;
  }
  reveal();
  const cwd = await sessionCwd(sessionId).catch(() => null);
  requestNewWorkspaceTerminal(home.teamKey, home.workspaceId, {
    launch: cli,
    cwd,
    machineId: home.tab.origin?.machineId ?? LOCAL_MACHINE_ID,
  });
}

/**
 * A live row's notice lines, under its button so their actions are real buttons of their own:
 * typed by hand (mail waits unwoken; Start managed) and release not supported (Check version,
 * for a session on this Mac, whose CLIs Settings lists). They wrap whole, never truncated.
 */
export function SessionNotices({
  sessionId,
  cli,
  typedByHand,
  unread,
  release,
  canCheckVersion,
  onReveal,
}: {
  /** Shows the row's own tab and workspace, as a click on the row does. */
  onReveal: () => void;
  sessionId: string | null;
  cli: string | null;
  typedByHand: boolean;
  unread: number;
  release: boolean;
  canCheckVersion: boolean;
}) {
  if (!typedByHand && !release) return null;
  const startable = !!sessionId && isAgentRuntime(cli);
  return (
    <div className="ag-notes">
      {typedByHand && (
        <span className="ag-note" data-testid="agent-unmanaged-note">
          {typedByHandNotice(unread)}
          {startable && (
            <>
              {" · "}
              <button
                type="button"
                className="ag-inline-action"
                aria-label={startManagedLabel(cli)}
                onClick={() => void startManaged(sessionId, cli, onReveal)}
              >
                Start managed
              </button>
            </>
          )}
        </span>
      )}
      {release && (
        <span className="ag-limited" data-testid="agent-limited-note">
          {RELEASE_NOT_SUPPORTED}
          {canCheckVersion && (
            <>
              {" · "}
              <button
                type="button"
                className="ag-inline-action"
                aria-label={checkVersionLabel(cli)}
                onClick={() => openSettingsTab("general", "agentClis")}
              >
                Check version
              </button>
            </>
          )}
        </span>
      )}
    </div>
  );
}
