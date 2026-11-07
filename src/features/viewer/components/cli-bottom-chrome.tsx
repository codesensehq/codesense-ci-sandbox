// SPDX-License-Identifier: MIT
import { Columns2, Rows2, LayoutGrid, SquareDashed, PanelBottom, FolderGit2 } from "lucide-react";

import { InfoTip, Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { ShortcutHint } from "@/components/ui/shortcut-hint";
import { formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";
import { useEffectiveShortcut } from "@/shared/stores/shortcuts-store";
import { getViewerState } from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import {
  useHasFocusedWorkspace,
  useTerminalColumnShown,
  useActiveForegroundPreset,
  useActiveForegroundTileCount,
  useEpicReposOpen,
  useTabGridWorkspaceId,
  useTabGridPresetOf,
} from "@/features/viewer/lib/cli-workspace/store/terminal-selectors";
import { workspaceGroupKey } from "@/shared/lib/workspace-key";
import { useActiveTeamKey } from "@/shared/lib/team-key";
import { useIsConsole } from "@/shared/hooks/use-is-console";
import { isEpicShellWorkspace } from "@/shared/lib/workspace-predicates";
import { useActiveWorkspaceContext } from "@/shared/hooks/use-active-workspace-context";
import {
  CHROME_DIVIDER,
  CHROME_ICON,
  CHROME_ICON_BUTTON,
} from "@/shared/components/chrome-control";

/** Which edge of the window the chrome rides: tooltips and the preset popover open
 *  away from it. */
type TipSide = "top" | "bottom";

/**
 * SW-EW-35: the CLI's own bottom-chrome controls, rendered as the right-pinned
 * `trailing` cluster on the workspace tray's strip (NOT the retired global
 * `StatusBar`, and NOT a second strip below the tray — one bottom chrome for the
 * CLI). Because the tray is a child of the terminal column, the controls span
 * only the CLI and stop at the right-pane divider via CSS flex — no store-derived
 * width calc is needed (the SW-EW-18 `StatusBar` rendered outside the viewer DOM
 * and had to align its right edge from `rightPaneOpen`/`rightPaneRatio`).
 *
 * Hosts the layout-preset picker, the console / full-screen toggle, and the
 * epic-repos toggle. Shown whenever the terminal is foreground (a workspace is
 * focused and the layout is showing the terminal — shared split OR console
 * full-screen, not the file viewer).
 *
 * SW-EW-35: the chrome rides the tray in BOTH shared and console mode so the
 * CLI's controls stay put when the terminal goes full-screen. The layout-preset
 * picker applies in both (the mosaic tiles by `foregroundLayout.preset`
 * regardless of mode); the full-screen toggle flips to a "back to bottom pane"
 * affordance in console (see `ConsoleToggle`). SW-EW-34: this bottom toggle is
 * now the ONLY in-mode exit-full-screen affordance — the title-bar `ConsoleTabs`
 * row was retired, so console mode is bottom-pane mode with the viewer collapsed.
 */
export function CliBottomChrome({
  placement = "tray",
  leadingDivider = false,
  trailingDivider = false,
}: {
  /** Where the chrome rides: the bottom tray (tooltips open up) or the title bar
   *  in full-screen CLI (tooltips open down). */
  placement?: "tray" | "titlebar";
  /** Hairlines that group the controls apart from their neighbours. They ride with
   *  the chrome so neither can strand alone when the chrome self-gates to null. */
  leadingDivider?: boolean;
  trailingDivider?: boolean;
} = {}) {
  const teamKey = useActiveTeamKey();
  const foreground = useTerminalForeground(teamKey);
  const hasWorkspace = useHasFocusedWorkspace(teamKey);
  if (!foreground) return null;
  const side: TipSide = placement === "titlebar" ? "bottom" : "top";

  return (
    <div
      data-testid="cli-bottom-chrome"
      className="flex flex-shrink-0 items-center gap-0.5 self-center text-muted-foreground"
    >
      {/* SW-EW-34 v2: when hosted in the title-bar right cluster (console), the
          leading divider rides WITH the chrome so it can't strand a lone
          separator when the chrome self-gates to null (e.g. Settings open). */}
      {leadingDivider && <span aria-hidden className={CHROME_DIVIDER} />}
      {hasWorkspace && <CliLayoutControl teamKey={teamKey} side={side} />}
      <ConsoleToggle side={side} />
      <EpicReposToggle side={side} />
      {trailingDivider && <span aria-hidden className={CHROME_DIVIDER} />}
    </div>
  );
}

/** True when the terminal area is on screen as the active surface (the active team
 *  has a workspace focused, and the layout is showing the terminal — shared split
 *  or console full-screen, not the file viewer). Whether its column is shown is one
 *  setting for every team (D39). Full-screen CLI keeps its chrome with no workspace
 *  focused: it is never left on the user's behalf (a team switch or the last close
 *  keeps it), so the full-screen toggle is the visible way back from an empty CLI.
 *  The CLI chrome shares this gate; where it rides (the tray in shared, the title
 *  bar in console) is the caller's.
 *
 *  Gating on `layoutMode !== "viewer"`: a direct `setTerminalVisible(true)` can
 *  leave `terminalVisible` true while `layoutMode` is still "viewer" — in which
 *  case the chrome must not show over the viewer. The Settings tab does not
 *  suppress it: Settings is a viewer tab and the terminal section stays on
 *  screen beneath it. */
function useTerminalForeground(teamKey: string): boolean {
  const hasWorkspace = useHasFocusedWorkspace(teamKey);
  const isConsole = useIsConsole();
  const shown = useTerminalColumnShown(teamKey);
  return (hasWorkspace || isConsole) && shown;
}

/** Pickable presets (the picker offers these three); `"custom"` is the store's
 *  marker for a manually drag-resized arrangement. */
type PickablePreset = "columns" | "rows" | "grid";
type ForegroundPreset = PickablePreset | "custom";

/** Trigger icon + label for each preset, so the layout control's button
 *  reflects the active arrangement (not a fixed grid glyph). `"custom"` (after a
 *  manual drag-resize) falls back to the generic grid glyph. */
const PRESET_META: Record<ForegroundPreset, { label: string; Icon: typeof LayoutGrid }> = {
  columns: { label: "Columns", Icon: Columns2 },
  rows: { label: "Rows", Icon: Rows2 },
  grid: { label: "Grid", Icon: LayoutGrid },
  custom: { label: "Custom", Icon: LayoutGrid },
};

/** The preset picker (columns / rows / grid), aware of what it arranges — the
 *  same target as the ⌘⇧L cycle: an expanded workspace showing all its tabs at
 *  once arranges those tabs; otherwise the workspace mosaic, shown once it holds
 *  ≥2 tiles. Picking a preset (even the current one) lays it out evenly again.
 *  The trigger glyph + tooltip track the active preset. */
function CliLayoutControl({ teamKey, side }: { teamKey: string; side: TipSide }) {
  // SW-EW-40: reflect the ACTIVE foreground slot (the selected rail's arrangement
  // in grouped mode, the per-team slot in flat) so the preset picker AND the ≥2
  // split gate track exactly what the mosaic renders — not the comingled set.
  const foregroundPreset: ForegroundPreset = useActiveForegroundPreset(teamKey) ?? "columns";
  const tileCount = useActiveForegroundTileCount(teamKey);
  const tabGridWorkspaceId = useTabGridWorkspaceId(teamKey);
  const tabGridPreset = useTabGridPresetOf(teamKey);
  const cycleShortcut = formatKeystrokeDisplay(
    useEffectiveShortcut("viewer.cycle-foreground-layout"),
  );
  const arrangesTabs = tabGridWorkspaceId !== null && tabGridPreset !== null;
  if (!arrangesTabs && tileCount < 2) return null;

  const preset: ForegroundPreset = arrangesTabs ? tabGridPreset : foregroundPreset;
  const setPreset = (p: PickablePreset) =>
    arrangesTabs
      ? getViewerState().setTabGridPreset(teamKey, tabGridWorkspaceId, p)
      : getViewerState().setForegroundPreset(teamKey, p);

  const { label: activeLabel, Icon: ActiveIcon } = PRESET_META[preset];

  return (
    <Popover>
      <Tooltip>
        <TooltipTrigger asChild>
          <PopoverTrigger asChild>
            <button
              type="button"
              data-testid="cli-chrome-layout"
              data-target={arrangesTabs ? "tabs" : "workspaces"}
              aria-label={`Terminal layout: ${activeLabel}`}
              className={CHROME_ICON_BUTTON}
            >
              <ActiveIcon {...CHROME_ICON} aria-hidden="true" />
            </button>
          </PopoverTrigger>
        </TooltipTrigger>
        <TooltipContent side={side}>
          <ShortcutHint label={`Terminal layout: ${activeLabel}`} shortcut={cycleShortcut} />
        </TooltipContent>
      </Tooltip>
      <PopoverContent side={side} align="end" className="w-auto flex-row gap-0.5 p-1">
        <PresetButton
          active={preset === "columns"}
          label="Columns"
          testid="cli-chrome-layout-columns"
          icon={<Columns2 {...CHROME_ICON} />}
          onClick={() => setPreset("columns")}
        />
        <PresetButton
          active={preset === "rows"}
          label="Rows"
          testid="cli-chrome-layout-rows"
          icon={<Rows2 {...CHROME_ICON} />}
          onClick={() => setPreset("rows")}
        />
        <PresetButton
          active={preset === "grid"}
          label="Grid"
          testid="cli-chrome-layout-grid"
          icon={<LayoutGrid {...CHROME_ICON} />}
          onClick={() => setPreset("grid")}
        />
      </PopoverContent>
    </Popover>
  );
}

function PresetButton({
  active,
  label,
  testid,
  icon,
  onClick,
}: {
  active: boolean;
  label: string;
  testid: string;
  icon: React.ReactNode;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      data-testid={testid}
      onClick={onClick}
      aria-label={label}
      aria-pressed={active}
      className={CHROME_ICON_BUTTON}
    >
      {icon}
    </button>
  );
}

/** SW-EW-35 AC-5: the full-screen / console toggle. A genuine toggle of the
 *  store's `toggleConsoleShared` (console↔shared) — NOT a hand-rolled
 *  `lastTerminalMode` round-trip: `setLayoutMode` records `lastTerminalMode` for
 *  BOTH console and shared, so reading it back on exit would resolve to
 *  "console" and never leave console mode.
 *
 *  The glyph + label reflect the action for the CURRENT state (not a pressed
 *  state — full-screen is just another layout, and the bottom-pane state isn't
 *  highlighted either): shared shows "Full screen" (the `SquareDashed`
 *  dotted-box glyph, AC-5); console shows "Back to bottom pane" (`PanelBottom`).
 *  SW-EW-34: this is the sole exit-full-screen affordance now (the title-bar
 *  `ConsoleTabs` row was retired). The rebindable `viewer.toggle-console`
 *  keystroke shows in the tooltip. */
function ConsoleToggle({ side }: { side: TipSide }) {
  const consoleShortcut = useEffectiveShortcut("viewer.toggle-console");
  const isConsole = useIsConsole();
  const onClick = () => getViewerState().toggleConsoleShared();

  const label = isConsole ? "Back to bottom pane" : "Full screen";
  const Icon = isConsole ? PanelBottom : SquareDashed;

  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <button
          type="button"
          data-testid="cli-chrome-console-toggle"
          onClick={onClick}
          aria-label={label}
          className={CHROME_ICON_BUTTON}
        >
          <Icon {...CHROME_ICON} aria-hidden="true" />
        </button>
      </TooltipTrigger>
      <TooltipContent side={side}>
        <ShortcutHint label={label} shortcut={formatKeystrokeDisplay(consoleShortcut)} />
      </TooltipContent>
    </Tooltip>
  );
}

/** SW-EW-15: epic-repo-config toggle. Shown only when the workspace focused in the CLI is an
 *  epic-integration terminal (slug-less integration with an
 *  `epicDirPath`); drives the store-backed `epicReposOpen` so the overlay (rendered in the team
 *  view over that tile) opens/closes. */
function EpicReposToggle({ side }: { side: TipSide }) {
  const { teamKey, workspace } = useActiveWorkspaceContext();
  const key = workspace ? workspaceGroupKey(teamKey, workspace.id) : null;
  const open = useEpicReposOpen(key);
  if (!workspace) return null;
  if (!isEpicShellWorkspace(workspace)) return null;

  return (
    <InfoTip label="Configure epic repos" side={side}>
      <button
        type="button"
        data-testid="cli-chrome-epic-repos-toggle"
        onClick={() => getViewerState().toggleEpicReposOpen(key!)}
        aria-pressed={open}
        aria-label={open ? "Hide epic repos" : "Configure epic repos"}
        className={CHROME_ICON_BUTTON}
      >
        <FolderGit2 {...CHROME_ICON} aria-hidden="true" />
      </button>
    </InfoTip>
  );
}
