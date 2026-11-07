// SPDX-License-Identifier: MIT
import { tabCloseKey, type CloseVerb } from "../lib/terminal-close";
import { useViewerStore, selectTerminalTabIndex } from "@/shared/stores/viewer-store";
import { openMachinesPanel } from "@/features/machines/lib/machine-actions";
import { requestMachineAppearance } from "@/features/machines/lib/machine-appearance";
import { isMac } from "@/shared/lib/shortcuts/keystroke";
import {
  PositionedContextMenu,
  PositionedMenuItem,
  PositionedSubmenu,
} from "@/shared/components/positioned-context-menu";
import {
  DROPDOWN_MENU_KIT,
  FaceChoices,
  RoleChoices,
  type AgentLook,
} from "./agent-identity-menus";

interface TabContextMenuProps {
  contextMenu: { x: number; y: number; tabId: string } | null;
  onClose: () => void;
  /** Hide — keep the tab's session running and list it under its tray chip. */
  onHideTab: (tabId: string) => void;
  /** End — kill the tab (asks first when it is running). */
  onEndTab: (tabId: string) => void;
  /** Stop the agent through the same confirmation as its agents-list row. */
  onStopAgent?: () => void;
  stopAgentDisabledReason?: string;
  /** Restart — a new process in the tab; passed only when its process ended
   *  (a live tab is Ended first — no "end and restart" in one step). */
  onRestartTab?: (tabId: string) => void;
  /** The verb Delete (⌫ on macOS) takes on the focused tab — its item shows the key. */
  keyCloseVerb?: CloseVerb;
  onEndOtherTabs: (tabId: string) => void;
  onEndAllTabs: () => void;
  /** The tile's doc (a story's file, an epic's README) — passed only when the
   *  tile has one and it can be opened. */
  doc?: MenuAction;
  /** Open the configured external editor at the workspace's directory ("Open
   *  in VS Code") — passed only when an editor is set and the directory exists. */
  editor?: MenuAction;
  /** SW-EW-46 (D2): the workspace is expanded and can show all its tabs at once —
   *  show the "Add to all tabs" / "Remove from all tabs" verb. */
  canGrid?: boolean;
  /** SW-EW-46: total tab count — "Add to all tabs" needs ≥2 to be meaningful;
   *  "Remove from all tabs" is offered for any paned tab regardless of count (so
   *  a lone paned cell is never stranded). */
  tabCount?: number;
  /** SW-EW-46: the tab ids currently paned — decides Add to vs. Remove from all tabs. */
  panedTabIds?: readonly string[];
  /** SW-EW-46: pane the right-clicked tab out as a grid cell. */
  onPaneOut?: (tabId: string) => void;
  /** SW-EW-46: return the right-clicked cell to the tab bar. */
  onDemotePane?: (tabId: string) => void;
  /** Rename the right-clicked tab — enters the tab bar's
   *  inline-edit mode (the menu equivalent of double-click-to-rename). */
  onRenameTab?: (tabId: string) => void;
  /** The Rename shortcut, drawn after its item. */
  renameKeystroke?: KeyHint;
  /** Open a new terminal tab (wired to onNewShell). */
  onNewTab?: () => void;
  /** The New terminal shortcut, drawn after its item. */
  newTabKeystroke?: KeyHint;
  /** The machine chooser: a new terminal on another machine. */
  onNewTabOn?: () => void;
  newTabOnKeystroke?: KeyHint;
  /** Split: a new terminal beside the right-clicked tab, side by side. */
  onSplit?: (tabId: string) => void;
  /** The Split shortcut (from the registry), drawn after its item. */
  splitKeystroke?: KeyHint;
  /** The right-clicked tab runs an agent: its Role and Face, as in the agents list —
   *  passed only for an agent tab. */
  agentLook?: AgentLook;
  /** SW-AGR-6: "Remind this agent…" — passed only for an interactive agent tab
   *  (saved or unnamed, local or remote), never a plain shell or a headless run. */
  onRemind?: (tabId: string) => void;
}

/** An item that is shown only when it can act. */
export interface MenuAction {
  label: string;
  run: () => void;
}

/** A shortcut as drawn (`⌘T`) and as announced (`aria-keyshortcuts`). */
export interface KeyHint {
  display: string;
  aria: string;
}

/** An item's shortcut: announced with `aria-keyshortcuts` (not in its name),
 *  and drawn after its label, hidden from assistive tech. */
function MenuItemLabel({ label, hint }: { label: string; hint?: KeyHint }) {
  return (
    <>
      {label}
      {hint?.display && (
        <span aria-hidden className="float-right pl-4 text-xs text-muted-foreground">
          {hint.display}
        </span>
      )}
    </>
  );
}

/** The Machine a remote tab runs on, or `null` for a local tab. */
function useTabMachineId(tabId: string | undefined): string | null {
  return useViewerStore((s) => {
    if (!tabId) return null;
    for (const byWorkspace of Object.values(selectTerminalTabIndex(s)))
      for (const tabs of Object.values(byWorkspace))
        for (const tab of tabs) if (tab.id === tabId) return tab.origin?.machineId ?? null;
    return null;
  });
}

/** The close item Delete (⌫ on macOS) takes on the focused tab. */
function closeKeyHint(marked: boolean): KeyHint | undefined {
  return marked ? { display: tabCloseKey(), aria: isMac() ? "Backspace" : "Delete" } : undefined;
}

export function TabContextMenu({
  contextMenu,
  onClose,
  onHideTab,
  onEndTab,
  onStopAgent,
  stopAgentDisabledReason,
  onRestartTab,
  keyCloseVerb,
  onEndOtherTabs,
  onEndAllTabs,
  doc,
  editor,
  canGrid = false,
  tabCount = 0,
  panedTabIds,
  onPaneOut,
  onDemotePane,
  onRenameTab,
  renameKeystroke,
  onNewTab,
  newTabKeystroke,
  onNewTabOn,
  newTabOnKeystroke,
  onSplit,
  splitKeystroke,
  agentLook,
  onRemind,
}: TabContextMenuProps) {
  const machineId = useTabMachineId(contextMenu?.tabId);
  if (!contextMenu) return null;

  // SW-EW-46 (D2): the paned verb — return a cell to the bar, or add a tab to
  // the ones shown at once. "Remove from all tabs" is available for any paned tab
  // (even a lone cell); "Add to all tabs" needs ≥2 tabs to be meaningful.
  const isPaned = panedTabIds?.includes(contextMenu.tabId) ?? false;
  const showPaneVerb =
    canGrid && (isPaned ? Boolean(onDemotePane) : tabCount >= 2 && Boolean(onPaneOut));

  return (
    <PositionedContextMenu
      position={contextMenu}
      onClose={onClose}
      minWidth="140px"
      label="Tab actions"
    >
      {showPaneVerb && (
        <>
          <PositionedMenuItem
            onClick={() => {
              if (isPaned) onDemotePane?.(contextMenu.tabId);
              else onPaneOut?.(contextMenu.tabId);
              onClose();
            }}
          >
            {isPaned ? "Remove from all tabs" : "Add to all tabs"}
          </PositionedMenuItem>
          <div role="separator" className="my-1 border-t border-border" />
        </>
      )}
      {/* Rename + New terminal — both verbs already exist
          elsewhere (double-click rename, the + button); surfaced here so the
          context menu is a complete tab-management affordance. */}
      {(onRenameTab || onNewTab || onSplit) && (
        <>
          {onRenameTab && (
            <PositionedMenuItem
              aria-keyshortcuts={renameKeystroke?.aria || undefined}
              onClick={() => {
                onRenameTab(contextMenu.tabId);
                onClose();
              }}
            >
              <MenuItemLabel label="Rename" hint={renameKeystroke} />
            </PositionedMenuItem>
          )}
          {onNewTab && (
            <PositionedMenuItem
              aria-keyshortcuts={newTabKeystroke?.aria || undefined}
              onClick={() => {
                onNewTab();
                onClose();
              }}
            >
              <MenuItemLabel label="New terminal" hint={newTabKeystroke} />
            </PositionedMenuItem>
          )}
          {onNewTabOn && (
            <PositionedMenuItem
              aria-keyshortcuts={newTabOnKeystroke?.aria || undefined}
              onClick={() => {
                onNewTabOn();
                onClose();
              }}
            >
              <MenuItemLabel label="New terminal on…" hint={newTabOnKeystroke} />
            </PositionedMenuItem>
          )}
          {onSplit && (
            <PositionedMenuItem
              aria-keyshortcuts={splitKeystroke?.aria || undefined}
              onClick={() => {
                onSplit(contextMenu.tabId);
                onClose();
              }}
            >
              <MenuItemLabel label="Split" hint={splitKeystroke} />
            </PositionedMenuItem>
          )}
          <div role="separator" className="my-1 border-t border-border" />
        </>
      )}

      {/* An agent tab: what the agent looks like, the same choices as its agents-list
          row, then Remind this agent… (SW-AGR-6). */}
      {(agentLook || onRemind) && (
        <>
          {agentLook && (
            <>
              <PositionedSubmenu label="Role">
                <RoleChoices
                  kit={DROPDOWN_MENU_KIT}
                  choice={agentLook.choice}
                  onChoose={agentLook.onChoose}
                  onDone={onClose}
                />
              </PositionedSubmenu>
              <PositionedSubmenu label="Face">
                <FaceChoices
                  kit={DROPDOWN_MENU_KIT}
                  choice={agentLook.choice}
                  avatars={agentLook.avatars}
                  onChoose={agentLook.onChoose}
                  onDone={onClose}
                />
              </PositionedSubmenu>
            </>
          )}
          {onRemind && (
            <PositionedMenuItem
              data-testid="tab-menu-remind"
              onClick={() => {
                onRemind(contextMenu.tabId);
                onClose();
              }}
            >
              Remind this agent…
            </PositionedMenuItem>
          )}
          <div role="separator" className="my-1 border-t border-border" />
        </>
      )}

      {/* The tile's doc and its directory in the editor — each only when it works. */}
      {[doc, editor].map(
        (action) =>
          action && (
            <PositionedMenuItem
              key={action.label}
              onClick={() => {
                action.run();
                onClose();
              }}
            >
              {action.label}
            </PositionedMenuItem>
          ),
      )}
      {/* A remote tab: its machine's color and background, in the Machines tab. */}
      {machineId && (
        <PositionedMenuItem
          onClick={() => {
            requestMachineAppearance(machineId);
            openMachinesPanel(null);
            onClose();
          }}
        >
          Machine appearance…
        </PositionedMenuItem>
      )}
      {(doc || editor || machineId) && (
        <div role="separator" className="my-1 border-t border-border" />
      )}

      {/* Restart: only once the tab's process ended, like its pane's ended bar. */}
      {onRestartTab && (
        <PositionedMenuItem
          onClick={() => {
            onRestartTab(contextMenu.tabId);
            onClose();
          }}
        >
          Restart
        </PositionedMenuItem>
      )}
      {/* The lifecycle group — Restart (an ended tab) and Hide (keeps it
          running, in the tray) — then the destructive group on its own: End
          kills it (asking first when anything is running). */}
      <PositionedMenuItem
        aria-keyshortcuts={closeKeyHint(keyCloseVerb === "Hide")?.aria}
        onClick={() => {
          onHideTab(contextMenu.tabId);
          onClose();
        }}
      >
        <MenuItemLabel label="Hide" hint={closeKeyHint(keyCloseVerb === "Hide")} />
      </PositionedMenuItem>
      <div role="separator" className="my-1 border-t border-border" />
      {onStopAgent && (
        <PositionedMenuItem
          disabled={Boolean(stopAgentDisabledReason)}
          title={stopAgentDisabledReason}
          onClick={() => {
            onStopAgent();
            onClose();
          }}
        >
          Stop…
        </PositionedMenuItem>
      )}
      <PositionedMenuItem
        aria-keyshortcuts={closeKeyHint(keyCloseVerb === "End")?.aria}
        onClick={() => {
          onEndTab(contextMenu.tabId);
          onClose();
        }}
      >
        <MenuItemLabel label="End" hint={closeKeyHint(keyCloseVerb === "End")} />
      </PositionedMenuItem>
      <PositionedMenuItem
        onClick={() => {
          onEndOtherTabs(contextMenu.tabId);
          onClose();
        }}
        title="Ends the other open tabs — hidden tabs keep running"
      >
        End Others
      </PositionedMenuItem>
      <PositionedMenuItem
        onClick={() => {
          onEndAllTabs();
          onClose();
        }}
        title="Ends this tile's open tabs — hidden tabs keep running"
      >
        End All
      </PositionedMenuItem>
    </PositionedContextMenu>
  );
}
