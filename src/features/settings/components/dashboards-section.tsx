// SPDX-License-Identifier: MIT
import { useState } from "react";
import {
  OPEN_POLICY,
  useDashboardPolicy,
  updateDashboardPolicy,
  type DashboardPolicy,
} from "@/features/dashboard/lib/verb-policy";
import { rowMatches } from "../lib/row-matches";
import { SettingRow } from "./setting-row";
import { SectionHeader } from "./section-header";
export const DASHBOARDS_SECTION_SPEC = {
  key: "dashboards",
  label: "Dashboards",
  keywords: ["policy", "preset", "open", "team-safe", "confirm", "messages", "send as you"],
};
export function DashboardsSection({ query = "" }: { query?: string }) {
  const policy = useDashboardPolicy();
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
  if (!rowMatches(DASHBOARDS_SECTION_SPEC, query)) return null;
  const save = async (update: (p: DashboardPolicy) => DashboardPolicy) => {
    setSaving(true);
    setError("");
    try {
      await updateDashboardPolicy(update);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };
  const shown = policy ?? OPEN_POLICY;
  return (
    <section>
      <SectionHeader id="dashboards">Dashboards</SectionHeader>
      <p className="mt-2 text-sm text-muted-foreground">
        Control confirmations for dashboard actions. Changes apply to the next call.
      </p>
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
      <SettingRow label="Action policy">
        <select
          aria-label="Dashboard action policy"
          value={shown.preset}
          disabled={!policy || saving}
          className="rounded-md border border-input bg-background px-3 py-2 text-sm"
          onChange={(e) => {
            const preset = e.target.value as DashboardPolicy["preset"];
            void save((p) => ({
              ...p,
              preset,
              switches: {
                confirmMessageDestructive: preset === "team-safe",
                confirmTeamSendAsYou: preset === "team-safe",
              },
            }));
          }}
        >
          <option value="open">Open (default)</option>
          <option value="team-safe">Team-safe</option>
        </select>
        <p className="mt-2 text-sm text-muted-foreground">
          Open runs every page verb without new prompts. Team-safe denies destructive and send
          actions from messages and confirms dashboard stop, close, agent launch, and team sends as
          you.
        </p>
      </SettingRow>
      {(
        [
          [
            "confirmMessageDestructive",
            "Confirm message actions",
            "Ask before destructive and send actions from HTML messages. Team-safe denies these actions before any dialog.",
          ],
          [
            "confirmTeamSendAsYou",
            "Confirm team sends as you",
            "Ask before a team dashboard sends into a terminal or sends as you. Send confirmations always apply to one call.",
          ],
        ] as const
      ).map(([key, label, description]) => (
        <SettingRow key={key} label={label}>
          <label className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              aria-label={label}
              checked={shown.preset === "team-safe" || shown.switches[key]}
              disabled={!policy || saving || shown.preset === "team-safe"}
              onChange={(e) => {
                const value = e.target.checked;
                void save((p) => ({ ...p, switches: { ...p.switches, [key]: value } }));
              }}
            />
            {description}
          </label>
        </SettingRow>
      ))}
    </section>
  );
}
