// SPDX-License-Identifier: MIT
import { PositionedMenuItem } from "@/shared/components/positioned-context-menu";
import { useViewerStore, type TerminalTab } from "@/shared/stores/viewer-store";
import { dismissHiddenTab, endHiddenTab, restartHiddenTab } from "../lib/terminal-close";
import { StatusMark } from "@/shared/components/status-mark";
import { useSessionEnd } from "./session-status-mark";

/**
 * One Hidden tab's entries in its tray chip's menu. A running tab: Restore or
 * End. One whose process ended by itself stays listed, marked ended or crashed
 * with its exit, until the user acts: Restore (to its tile, which shows the
 * ended bar), Restart (on a fresh shell), or Dismiss.
 */
export function HiddenTabMenuItems({
  teamKey,
  workspaceId,
  tab,
  onDone,
}: {
  teamKey: string;
  workspaceId: string;
  tab: TerminalTab;
  onDone: () => void;
}) {
  const ended = useSessionEnd(tab.sessionId);
  const restore = () => {
    useViewerStore.getState().restoreHiddenTerminalTab(teamKey, workspaceId, tab.id);
    onDone();
  };
  if (ended) {
    const { end, label } = ended;
    return (
      <>
        <div
          className="flex items-center gap-1.5 px-2 py-1 text-xs"
          data-testid={`hidden-tab-ended-${tab.id}`}
        >
          <StatusMark mark={end} size="sm" label={label} />
          <span className="truncate">
            “{tab.name}” · {label}
          </span>
        </div>
        <PositionedMenuItem onClick={restore}>Restore “{tab.name}”</PositionedMenuItem>
        <PositionedMenuItem
          onClick={() => {
            restartHiddenTab(teamKey, workspaceId, tab.id);
            onDone();
          }}
        >
          Restart “{tab.name}”
        </PositionedMenuItem>
        <PositionedMenuItem
          onClick={() => {
            dismissHiddenTab(teamKey, workspaceId, tab.id);
            onDone();
          }}
        >
          Dismiss “{tab.name}”
        </PositionedMenuItem>
      </>
    );
  }
  return (
    <>
      <PositionedMenuItem onClick={restore}>Restore “{tab.name}”</PositionedMenuItem>
      <PositionedMenuItem
        onClick={() => {
          void endHiddenTab(teamKey, workspaceId, tab.id);
          onDone();
        }}
      >
        End “{tab.name}”
      </PositionedMenuItem>
    </>
  );
}
