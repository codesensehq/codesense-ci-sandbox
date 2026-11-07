// SPDX-License-Identifier: MIT
import { useMemo, useState } from "react";
import { ArrowDown, ArrowUp } from "lucide-react";

import type { StatusState } from "@/tauri/commands";
import { saveTeamConfig } from "@/tauri/commands";
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";
import { useStatusTaxonomyRefresh } from "@/shared/stores/status-taxonomy-refresh-store";
import { LucideByName } from "@/shared/lib/icons/lucide-icon-map";
import { useSubmitAction } from "@/shared/hooks/use-submit-action";
import { SettingsCard } from "../workspace-section/settings-card";

/** Human-readable label for each fixed role — shown in the read-only role chip. */
const ROLE_LABEL: Record<StatusState["role"], string> = {
  not_started: "Not started",
  active: "Active",
  review: "Review",
  done: "Done",
  blocked: "Blocked",
};

interface PhasesPanelProps {
  orgRoot: string;
  team: string;
  teamKey: string;
}

/**
 * SW-GT-6 (AC-1) · Edit the team's status taxonomy: reorder, rename (label),
 * recolor, and mark a terminal (done-role) state as dropped (`delivered:false`).
 * The derived `role` is shown READ-ONLY — teams customize states, never the
 * fixed role set. Saves optimistically via `setStates` + `saveTeamConfig` +
 * a refresh `bump` (never `loadOrg`).
 */
export function PhasesPanel({ orgRoot, team, teamKey }: PhasesPanelProps) {
  const states = useStatusTaxonomyStore((s) => s.states);
  const setStates = useStatusTaxonomyStore((s) => s.setStates);
  const bump = useStatusTaxonomyRefresh((s) => s.bump);
  const { submitting, submit } = useSubmitAction();

  // Local working copy so edits are immediate; committed on Save. `order` is
  // normalized to the array index at save time, so reorder is just splice.
  const [draft, setDraft] = useState<StatusState[] | null>(null);
  const working = draft ?? states;

  const dirty = useMemo(() => draft !== null && !sameStates(draft, states), [draft, states]);

  const editState = (id: string, patch: Partial<StatusState>) => {
    setDraft(working.map((s) => (s.id === id ? { ...s, ...patch } : s)));
  };

  const move = (index: number, delta: number) => {
    const target = index + delta;
    if (target < 0 || target >= working.length) return;
    const next = [...working];
    const [moved] = next.splice(index, 1);
    next.splice(target, 0, moved);
    setDraft(next);
  };

  const onSave = () => {
    // Normalize `order` to the current array index so the on-disk order matches
    // what the user sees, regardless of the loaded orders.
    const normalized = working.map((s, order) => ({ ...s, order }));
    void submit(
      async () => {
        await saveTeamConfig(orgRoot, team, { kind: "status_taxonomy", states: normalized });
        // Optimistic: reflect the write immediately, then bump so the async
        // hydration re-loads in place (no loadOrg / no app restart).
        setStates(normalized);
        bump(teamKey);
        setDraft(null);
      },
      { success: "Saved phases", errorPrefix: "Failed to save phases" },
    );
  };

  const onReset = () => setDraft(null);

  return (
    <SettingsCard title="Phases" count={working.length} data-testid="settings-phases-card">
      <ul className="flex flex-col gap-2" data-testid="settings-phases-list">
        {working.map((state, index) => (
          <PhaseRow
            key={state.id}
            state={state}
            index={index}
            count={working.length}
            disabled={submitting}
            onMove={move}
            onEdit={editState}
          />
        ))}
      </ul>

      <div className="mt-3 flex items-center gap-2">
        <button
          type="button"
          onClick={onSave}
          disabled={!dirty || submitting}
          data-testid="settings-phases-save"
          className="inline-flex items-center gap-1 rounded border border-border bg-background px-2 py-1 text-xs hover:bg-accent disabled:opacity-50"
        >
          {submitting ? "Saving…" : "Save phases"}
        </button>
        {dirty && (
          <button
            type="button"
            onClick={onReset}
            disabled={submitting}
            data-testid="settings-phases-reset"
            className="rounded px-2 py-1 text-xs text-muted-foreground hover:bg-accent disabled:opacity-50"
          >
            Discard
          </button>
        )}
      </div>
    </SettingsCard>
  );
}

function PhaseRow({
  state,
  index,
  count,
  disabled,
  onMove,
  onEdit,
}: {
  state: StatusState;
  index: number;
  count: number;
  disabled: boolean;
  onMove: (index: number, delta: number) => void;
  onEdit: (id: string, patch: Partial<StatusState>) => void;
}) {
  // AC-1: the dropped toggle is meaningful only on a terminal (done-role) state —
  // it marks a "won't-do" outcome that velocity excludes.
  const isTerminal = state.role === "done";
  const dropped = state.delivered === false;

  return (
    <li
      className="flex items-center gap-2 rounded border border-border px-2 py-1.5"
      data-testid={`settings-phase-row-${state.id}`}
    >
      <div className="flex flex-col">
        <button
          type="button"
          aria-label={`Move ${state.label} up`}
          disabled={disabled || index === 0}
          onClick={() => onMove(index, -1)}
          data-testid={`settings-phase-up-${state.id}`}
          className="text-muted-foreground hover:text-foreground disabled:opacity-30"
        >
          <ArrowUp size={12} aria-hidden="true" />
        </button>
        <button
          type="button"
          aria-label={`Move ${state.label} down`}
          disabled={disabled || index === count - 1}
          onClick={() => onMove(index, 1)}
          data-testid={`settings-phase-down-${state.id}`}
          className="text-muted-foreground hover:text-foreground disabled:opacity-30"
        >
          <ArrowDown size={12} aria-hidden="true" />
        </button>
      </div>

      <LucideByName
        name={state.icon}
        size={16}
        aria-hidden="true"
        className="shrink-0"
        style={{ color: state.color }}
      />

      <input
        type="color"
        value={normalizeHex(state.color)}
        disabled={disabled}
        onChange={(e) => onEdit(state.id, { color: e.target.value })}
        aria-label={`${state.label} color`}
        data-testid={`settings-phase-color-${state.id}`}
        className="h-6 w-6 shrink-0 cursor-pointer rounded border border-border bg-transparent p-0 disabled:opacity-50"
      />

      <input
        type="text"
        value={state.label}
        disabled={disabled}
        onChange={(e) => onEdit(state.id, { label: e.target.value })}
        aria-label={`${state.id} label`}
        data-testid={`settings-phase-label-${state.id}`}
        className="h-6 min-w-0 flex-1 rounded border border-border bg-background px-2 text-xs outline-none focus:border-ring disabled:opacity-50"
      />

      <span
        className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-muted-foreground"
        data-testid={`settings-phase-role-${state.id}`}
      >
        {ROLE_LABEL[state.role]}
      </span>

      {isTerminal && (
        <label
          className="flex shrink-0 items-center gap-1 text-[11px] text-muted-foreground"
          data-testid={`settings-phase-dropped-${state.id}`}
        >
          <input
            type="checkbox"
            checked={dropped}
            disabled={disabled}
            onChange={(e) =>
              // `delivered:false` marks dropped; clearing it removes the key
              // (absent ⇒ delivered) so an ordinary terminal writes nothing.
              onEdit(state.id, { delivered: e.target.checked ? false : undefined })
            }
            data-testid={`settings-phase-dropped-input-${state.id}`}
          />
          Dropped
        </label>
      )}
    </li>
  );
}

/** Structural equality on the editable fields (ignores object identity). */
function sameStates(a: StatusState[], b: StatusState[]): boolean {
  if (a.length !== b.length) return false;
  return a.every((s, i) => {
    const o = b[i];
    return (
      s.id === o.id &&
      s.label === o.label &&
      s.color === o.color &&
      s.role === o.role &&
      s.delivered === o.delivered
    );
  });
}

/** `<input type=color>` requires a 7-char `#rrggbb`; fall back for named/short. */
function normalizeHex(color: string): string {
  return /^#[0-9a-fA-F]{6}$/.test(color) ? color : "#94a3b8";
}
