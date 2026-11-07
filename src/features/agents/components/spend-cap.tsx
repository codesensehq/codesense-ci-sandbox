// SPDX-License-Identifier: MIT
import { useEffect, useId, useRef, useState } from "react";
import { CircleAlert } from "lucide-react";
import { Input } from "@/components/ui/input";
import { AppButton as Button } from "@/shared/components/app-button";
import { cn } from "@/lib/utils";
import {
  spendCapAccept,
  spendCapSet,
  spendRateAccept,
  type BudgetUsd,
  type RoutineRow,
  type SpendCapView,
} from "@/tauri/commands";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { useAgentsStore } from "../stores/agents-store";
import { showAgentCap } from "./agents-tab-nav";

const dollars = (value: number) =>
  `$${value.toLocaleString("en-US", { maximumFractionDigits: 0 })}`;
const exact = (value: number) => `$${value}`;
const warning = "inline-flex items-center gap-1 font-medium text-warning-ink";
/** Why an `unrecorded` cap's spend is not the real spend (the Rust `USAGE_REDIRECTED`). */
export const USAGE_NOT_RECORDED =
  "usage not recorded: this Mac's managed Claude settings send session telemetry to your organization's collector";
const link =
  "underline decoration-dotted underline-offset-2 disabled:cursor-wait disabled:opacity-50 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";

export function SpendCapChip({ agentKey }: { agentKey: string }) {
  const value = useAgentListProjectionStore((s) => s.spend[`agent:${agentKey}`]);
  if (!value?.cap) return null;
  const { cap, today, week, state } = value;
  const weekly =
    cap.perWeek != null && (cap.perDay == null || week / cap.perWeek > today / cap.perDay);
  const spend = weekly ? week : today;
  const limit = weekly ? cap.perWeek! : cap.perDay!;
  const window = weekly ? "this week" : "today";
  return (
    <>
      <span
        data-testid="spend-cap-chip"
        role={state === "reached" ? "status" : undefined}
        title={
          state === "unrecorded"
            ? USAGE_NOT_RECORDED
            : `${exact(today)} today · ${exact(week)} this week; cap ${exact(limit)} ${window}`
        }
        className={cn(
          state === "warn" || state === "unpriced" || state === "unrecorded"
            ? warning
            : state === "reached"
              ? "inline-flex items-center gap-1 font-medium text-destructive"
              : "text-muted-foreground",
        )}
      >
        {state === "reached" && <CircleAlert aria-hidden="true" className="size-3" />}
        {state === "unpriced"
          ? `Can't price ${value.unpricedModels.join(", ")}`
          : state === "unrecorded"
            ? "Usage not recorded"
            : state === "reached"
              ? `Cap reached · ${dollars(spend)} ${window}`
              : `${dollars(spend)} of ${dollars(limit)} ${window}`}
      </span>
      {value.pending && (
        <button type="button" className={link} onClick={() => showAgentCap(agentKey)}>
          raise pending
        </button>
      )}
    </>
  );
}

export function SpendCapBlock({
  scopeKind,
  scopeKey,
}: {
  scopeKind: "agent" | "routine";
  scopeKey: string;
}) {
  const value = useAgentListProjectionStore((s) => s.spend[`${scopeKind}:${scopeKey}`]);
  const focusKey = useAgentsStore((s) => s.capFocusKey);
  const ref = useRef<HTMLElement>(null);
  useEffect(() => {
    if (
      focusKey === (scopeKind === "agent" ? scopeKey : `routine:${scopeKey}`) &&
      value &&
      ref.current
    ) {
      ref.current.scrollIntoView?.({ block: "nearest" });
      ref.current.focus();
      useAgentsStore.setState({ capFocusKey: null });
    }
  }, [focusKey, scopeKind, scopeKey, value]);
  return (
    <section
      ref={ref}
      id={scopeKind === "agent" ? "spend-cap" : undefined}
      tabIndex={-1}
      aria-label="Spend cap"
      className="rounded-md text-xs text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
    >
      <h3 className="mb-2 text-sm font-semibold text-foreground">Spend cap</h3>
      {value ? (
        <CapEditor
          key={`${scopeKind}:${scopeKey}`}
          value={value}
          scopeKind={scopeKind}
          scopeKey={scopeKey}
        />
      ) : (
        <p>Loading spend…</p>
      )}
    </section>
  );
}

function pendingText(value: SpendCapView, scopeKind: "agent" | "routine", key: string) {
  const changes: string[] = [];
  for (const [field, label] of [
    ["perDay", "daily"],
    ["perWeek", "weekly"],
  ] as const) {
    const current = value.cap?.[field];
    const requested = value.requestedCap?.[field];
    if (current != null && (requested == null || requested > current))
      changes.push(
        requested == null
          ? `remove the ${label} cap`
          : `raise the ${label} cap to ${dollars(requested)}`,
      );
  }
  const source = scopeKind === "agent" ? "agent.yaml" : "routine.yml";
  const time =
    value.pendingAt == null
      ? ""
      : `, ${new Date(value.pendingAt).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false })}`;
  return `${key.split("/").at(-1)} asked to ${changes.join(" and ")} (${source}${time}).`;
}

function CapEditor({
  value,
  scopeKind,
  scopeKey,
}: {
  value: SpendCapView;
  scopeKind: "agent" | "routine";
  scopeKey: string;
}) {
  const id = useId();
  const [editing, setEditing] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [day, setDay] = useState(String(value.cap?.perDay ?? ""));
  const [week, setWeek] = useState(String(value.cap?.perWeek ?? ""));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  useEffect(() => {
    if (!dirty) {
      setDay(String(value.cap?.perDay ?? ""));
      setWeek(String(value.cap?.perWeek ?? ""));
    }
  }, [value.cap?.perDay, value.cap?.perWeek, dirty]);
  const write = async (action: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    setSaved(false);
    try {
      await action();
      setDirty(false);
      setSaved(true);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  const save = () => {
    const budget: BudgetUsd = {};
    for (const [field, raw] of [
      ["perDay", day],
      ["perWeek", week],
    ] as const) {
      if (!raw.trim()) continue;
      const amount = Number(raw);
      if (!Number.isFinite(amount) || amount <= 0) {
        setError("Enter a positive USD amount, or leave the field empty for no cap.");
        return;
      }
      budget[field] = amount;
    }
    void write(() => spendCapSet(scopeKind, scopeKey, Object.keys(budget).length ? budget : null));
  };
  const keepValue =
    value.cap?.perDay != null && value.cap.perDay !== value.requestedCap?.perDay
      ? value.cap.perDay
      : (value.cap?.perWeek ?? value.cap?.perDay);
  return (
    <div className="flex flex-col gap-2">
      {value.cap ? (
        <p
          className="tabular-nums"
          title={`${exact(value.today)} today · ${exact(value.week)} this week`}
        >
          Today {dollars(value.today)}
          {value.cap.perDay != null && ` of ${dollars(value.cap.perDay)}`} · This week{" "}
          {dollars(value.week)}
          {value.cap.perWeek != null && ` of ${dollars(value.cap.perWeek)}`}
        </p>
      ) : (
        <p className="flex flex-wrap items-center gap-2" title={`${exact(value.today)} today`}>
          No cap · {dollars(value.today)} today
          {!editing && (
            <button type="button" className={link} onClick={() => setEditing(true)}>
              Set a cap
            </button>
          )}
        </p>
      )}
      {value.state === "unpriced" && (
        <p className="text-[var(--cs-warning-text)]">
          Can't price {value.unpricedModels.join(", ")}. Set a model rate before automatic work can
          continue.
        </p>
      )}
      {value.state === "unrecorded" && (
        <p className="text-[var(--cs-warning-text)]" title={USAGE_NOT_RECORDED}>
          Usage not recorded
        </p>
      )}
      {value.pendingRates?.map((request) => (
        <div
          key={`${request.modelPrefix}:${request.effectiveFrom}`}
          className="flex flex-col gap-1"
        >
          <p className="font-medium text-foreground">Model rate pending · {request.modelPrefix}</p>
          <p className="break-words tabular-nums">
            From {request.effectiveFrom}: input {exact(request.inputPerMtok)}, output{" "}
            {exact(request.outputPerMtok)}, cache read{" "}
            {exact(request.cacheReadPerMtok ?? request.inputPerMtok * 0.1)} per million tokens.
            {request.long &&
              ` From ${request.long.fromTokens} prompt tokens: input ${exact(request.long.inputPerMtok)}, output ${exact(request.long.outputPerMtok)}, cache read ${exact(request.long.cacheReadPerMtok ?? request.long.inputPerMtok * 0.1)} per million.`}
          </p>
          <button
            type="button"
            className={cn(link, "self-start")}
            disabled={busy}
            aria-label={`Accept rate for ${request.modelPrefix}`}
            onClick={() =>
              void write(async () => {
                await spendRateAccept(request);
              })
            }
          >
            Accept
          </button>
        </div>
      ))}
      {(value.cap || editing) && (
        <form
          className="flex flex-wrap items-end gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            save();
          }}
        >
          <label className="flex flex-col gap-1" htmlFor={`${id}-day`}>
            Per day
            <Input
              id={`${id}-day`}
              type="number"
              inputMode="decimal"
              step="any"
              min="0"
              placeholder="No cap"
              className="h-7 w-28 text-xs"
              disabled={busy}
              value={day}
              onChange={(e) => {
                setDay(e.target.value);
                setDirty(true);
                setSaved(false);
              }}
            />
          </label>
          <label className="flex flex-col gap-1" htmlFor={`${id}-week`}>
            Per week
            <Input
              id={`${id}-week`}
              type="number"
              inputMode="decimal"
              step="any"
              min="0"
              placeholder="No cap"
              className="h-7 w-28 text-xs"
              disabled={busy}
              value={week}
              onChange={(e) => {
                setWeek(e.target.value);
                setDirty(true);
                setSaved(false);
              }}
            />
          </label>
          <Button type="submit" size="sm" variant="outline" disabled={busy}>
            {busy ? "Saving…" : "Save"}
          </Button>
          <span>USD · empty means no cap</span>
        </form>
      )}
      {value.pending ? (
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-[var(--cs-warning-text)]">
          <p title={JSON.stringify(value.requestedCap)}>
            {pendingText(value, scopeKind, scopeKey)}
          </p>
          <button
            type="button"
            className={link}
            disabled={busy}
            onClick={() =>
              void write(() => spendCapAccept(scopeKind, scopeKey, value.requestedCap))
            }
          >
            Accept
          </button>
          <span aria-hidden="true">·</span>
          <button
            type="button"
            className={link}
            disabled={busy}
            onClick={() =>
              void write(() => spendCapAccept(scopeKind, scopeKey, value.requestedCap, true))
            }
          >
            Keep {keepValue == null ? "no cap" : dollars(keepValue)}
          </button>
        </div>
      ) : (
        (value.cap || editing) && (
          <p>Lowering applies now. Raising applies once you save it here.</p>
        )
      )}
      {error && (
        <p role="alert" className="text-destructive">
          {error}
        </p>
      )}
      {saved && <p role="status">Saved.</p>}
    </div>
  );
}

export function RoutineSpendCap({ row }: { row: RoutineRow }) {
  if (row.def.action === "command" || row.effective_mode === "wake")
    return (
      <p className="my-3 text-xs text-muted-foreground">Spend counts toward the agent's cap</p>
    );
  return (
    <div className="my-3">
      <SpendCapBlock scopeKind="routine" scopeKey={row.key} />
    </div>
  );
}
