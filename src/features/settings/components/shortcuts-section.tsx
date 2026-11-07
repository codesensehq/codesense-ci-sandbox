// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import { Switch } from "@/components/ui/switch";
import { AppButton } from "@/shared/components/app-button";
import { formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";
import {
  allShortcutActions,
  NAV_AXES,
  resolveBindings,
  shortcutLabelById,
  useDynamicShortcutActions,
  type ShortcutAction,
} from "@/shared/lib/shortcuts/registry";
import { rowMatches, type SettingRowSpec } from "@/features/settings/lib/row-matches";
import { useEffectiveShortcut, useShortcutsStore } from "@/shared/stores/shortcuts-store";

import { ShortcutRow, showResetAllConfirm, takeChord } from "./shortcut-row";
import { commandHumanOnly, dashboardHumanOnly, useHumanOnlyPolicy } from "./human-only";
import { SectionHeader } from "./section-header";

/** A shortcut row's search spec: a dashboard command also matches its dashboard's name. */
function shortcutRowSpec(action: ShortcutAction): SettingRowSpec {
  return {
    key: action.id,
    label: action.label,
    keywords: action.command ? [action.command.dashboardTitle] : [],
  };
}

/** Row specs for every action in the registry, dashboard commands included. Shared
 *  with the sidebar's `SECTION_ROW_SPECS` map so both filter by the same data. */
export function shortcutRowSpecs(): SettingRowSpec[] {
  return allShortcutActions().map(shortcutRowSpec);
}

const NAV_ID_SET = new Set(NAV_AXES.flatMap((axis) => axis.actionIds));

/** A stable, space-free suffix for a group label's test id. */
function axisSlug(label: string): string {
  return label.toLowerCase().replace(/\s+/g, "-");
}

const GROUP_HEADING = "mb-1 text-xs font-medium uppercase tracking-wide text-muted-foreground";

interface ShortcutsSectionProps {
  query: string;
}

/** One unbound dashboard command whose wanted chord another action holds. */
function ConflictRow({
  action,
  wanted,
  winner,
}: {
  action: ShortcutAction;
  wanted: string;
  winner: string;
}) {
  const chord = formatKeystrokeDisplay(wanted);
  return (
    <div
      data-testid={`shortcut-conflict-${action.id}`}
      className="flex items-center justify-between gap-3 px-3 py-2"
    >
      <div className="min-w-0">
        <div className="text-sm font-medium">{action.label}</div>
        <div className="text-xs text-muted-foreground">
          {action.command?.dashboardTitle} · wants {chord}, used by {shortcutLabelById(winner)}
        </div>
      </div>
      <AppButton
        size="sm"
        variant="outline"
        data-testid={`shortcut-conflict-${action.id}-take`}
        onClick={() => takeChord(action, wanted)}
      >
        Use {chord} for this
      </AppButton>
    </div>
  );
}

/**
 * One slot dashboard's commands, with its "Humans only" switch in the heading
 * (covering every command) and one per row. Team-safe locks a private dashboard's.
 */
function DashboardGroup({
  dashboard,
  title,
  actions,
  palette,
  humanOnly: { policy, setDashboard, setCommand },
}: {
  dashboard: string;
  title: string;
  actions: ShortcutAction[];
  /** The command palette's chord, for the note on a humans-only dashboard. */
  palette: string;
  humanOnly: ReturnType<typeof useHumanOnlyPolicy>;
}) {
  const whole = dashboardHumanOnly(policy, actions[0].command!);
  const groupNote =
    whole.lockedBy === "team-safe"
      ? "Humans only while sharing is team-safe. This is a private dashboard."
      : whole.on
        ? `Agents can't run any ${title} command. You still can, from keys or ${palette}.`
        : null;
  return (
    <div className="mt-4" data-testid={`shortcuts-dashboard-${dashboard}`}>
      <div className="mb-1 flex items-center justify-between gap-3">
        <h3 className={GROUP_HEADING.replace("mb-1 ", "")}>{title}</h3>
        <label className="flex items-center gap-2 text-xs text-muted-foreground">
          Humans only
          <Switch
            size="sm"
            checked={whole.on}
            disabled={whole.lockedBy !== null}
            aria-label={
              whole.lockedBy === "team-safe"
                ? `Humans only: all ${title} commands, set by team-safe sharing`
                : `Humans only: all ${title} commands`
            }
            data-testid={`shortcuts-dashboard-${dashboard}-human-only`}
            onCheckedChange={(on) => setDashboard(dashboard, on)}
          />
        </label>
      </div>
      {groupNote && (
        <p
          className="mb-1 text-xs text-muted-foreground"
          data-testid={`shortcuts-dashboard-${dashboard}-human-only-note`}
        >
          {groupNote}
        </p>
      )}
      <div className="divide-y divide-border">
        {actions.map((action) => {
          const state = commandHumanOnly(policy, action);
          const why =
            state.lockedBy === "team-safe"
              ? ", set by team-safe sharing"
              : state.lockedBy === "dashboard"
                ? `, on for all ${title} commands`
                : "";
          return (
            <ShortcutRow
              key={action.id}
              action={action}
              humanOnly={{
                on: state.on,
                locked: state.lockedBy !== null,
                label: `Humans only: ${action.label}${why}`,
                onChange: (on) => setCommand(action.id, on),
                overridden: state.overridden,
                onReset: () => setCommand(action.id, undefined),
                note: state.suggested ? `${title} suggests humans only.` : null,
              }}
            />
          );
        })}
      </div>
    </div>
  );
}

/**
 * Settings → Shortcuts section. Renders the whole shortcut registry as editable
 * rows, filtered by the panel's search query: a "Conflicts" group first while any
 * dashboard command's wanted chord is taken, then the keyboard-navigation axes as
 * labelled clusters (driven by `NAV_AXES`), then every other built-in as a flat
 * list, then one group per slot dashboard's commands. Each row resolves its live
 * (rebound-aware) keystroke through the registry, so the surface can never drift
 * from the real keys.
 *
 * A "Reset all to defaults" button lives at the section foot; it confirms via a
 * sonner toast action button before clearing every override. Individual rows
 * have their own Reset button (disabled when the action is already at its
 * default).
 */
export function ShortcutsSection({ query }: ShortcutsSectionProps) {
  const actions = useDynamicShortcutActions((s) => s.all);
  const shortcuts = useShortcutsStore((s) => s.shortcuts);

  const visible = useMemo(
    () =>
      query.trim() ? actions.filter((a) => rowMatches(shortcutRowSpec(a), query)) : [...actions],
    [actions, query],
  );

  const { conflicts, navGroups, rest, dashboards } = useMemo(() => {
    const bindings = resolveBindings(shortcuts);
    const byId = new Map(visible.map((a) => [a.id, a]));
    const builtIns = visible.filter((a) => !a.command);
    const dashboardGroups = new Map<string, { title: string; actions: ShortcutAction[] }>();
    for (const action of visible) {
      if (!action.command) continue;
      const group = dashboardGroups.get(action.command.dashboard) ?? {
        title: action.command.dashboardTitle,
        actions: [],
      };
      group.actions.push(action);
      dashboardGroups.set(action.command.dashboard, group);
    }
    return {
      conflicts: visible.flatMap((action) => {
        const reason = bindings.get(action.id)?.unbound;
        return reason?.kind === "conflict"
          ? [{ action, wanted: reason.wanted, winner: reason.winner }]
          : [];
      }),
      navGroups: NAV_AXES.map((axis) => ({
        label: axis.label,
        actions: axis.actionIds
          .map((id) => byId.get(id))
          .filter((a): a is ShortcutAction => a !== undefined),
      })).filter((g) => g.actions.length > 0),
      rest: builtIns.filter((a) => !NAV_ID_SET.has(a.id)),
      dashboards: [...dashboardGroups.entries()],
    };
  }, [visible, shortcuts]);

  const humanOnly = useHumanOnlyPolicy();
  const palette = formatKeystrokeDisplay(useEffectiveShortcut("nav.open-command-palette"));
  const announcement = query.trim().length > 0 ? `${visible.length} shortcuts shown` : "";

  return (
    <section className="pt-6" data-testid="settings-shortcuts-section">
      <SectionHeader id="shortcuts">Shortcuts</SectionHeader>
      <div aria-live="polite" className="sr-only" data-testid="settings-search-count-shortcuts">
        {announcement}
      </div>
      {visible.length === 0 ? (
        <p data-testid="settings-shortcuts-empty" className="py-4 text-sm text-muted-foreground">
          No matching shortcuts.
        </p>
      ) : (
        <>
          {conflicts.length > 0 && (
            <div className="mb-4" data-testid="shortcuts-conflicts">
              <h3 className={GROUP_HEADING}>Conflicts · {conflicts.length}</h3>
              <div className="divide-y divide-border rounded-md border border-border">
                {conflicts.map((c) => (
                  <ConflictRow key={c.action.id} {...c} />
                ))}
              </div>
            </div>
          )}
          {navGroups.map((group) => (
            <div
              key={group.label}
              className="mb-4"
              data-testid={`shortcuts-nav-axis-${axisSlug(group.label)}`}
            >
              <h3 className={GROUP_HEADING}>{group.label}</h3>
              <div className="divide-y divide-border">
                {group.actions.map((action) => (
                  <ShortcutRow key={action.id} action={action} />
                ))}
              </div>
            </div>
          ))}
          {rest.length > 0 && (
            <div className="divide-y divide-border" data-testid="shortcuts-rest">
              {rest.map((action) => (
                <ShortcutRow key={action.id} action={action} />
              ))}
            </div>
          )}
          {dashboards.map(([key, group]) => (
            <DashboardGroup
              key={key}
              dashboard={key}
              {...group}
              palette={palette}
              humanOnly={humanOnly}
            />
          ))}
        </>
      )}
      {visible.length > 0 && (
        <div className="mt-4 flex justify-end">
          <AppButton
            size="sm"
            variant="ghost"
            data-testid="shortcuts-reset-all"
            onClick={showResetAllConfirm}
          >
            Reset all to defaults
          </AppButton>
        </div>
      )}
    </section>
  );
}
