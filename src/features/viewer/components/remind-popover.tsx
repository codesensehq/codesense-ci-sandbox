// SPDX-License-Identifier: MIT
import { useEffect, useId, useRef, useState, type KeyboardEvent } from "react";
import { ArrowLeft, Loader2, X } from "lucide-react";
import { toast } from "sonner";

import { Input } from "@/components/ui/input";
import { AppButton as Button } from "@/shared/components/app-button";
import { cn } from "@/lib/utils";
import { getErrorMessage } from "@/shared/lib/error-message";

import { useNowClock } from "../hooks/use-now-clock";
import {
  planReminder,
  reminderPreview,
  shortTime,
  timerTargetKey,
  type ReminderField,
  type ReminderInput,
  type TimerTarget,
} from "../lib/session-timers";
import { useSessionTimersStore } from "../stores/session-timers-store";
import { SessionTimersPopover, type SessionTimersPopoverProps } from "./session-timers-popover";

/** The message's cap is 8 KiB; its counter shows only past 7 KiB (§4.13). */
const MESSAGE_CAP = 8 * 1024;
const COUNTER_FROM = 7 * 1024;

const KINDS: { id: ReminderInput["kind"]; label: string }[] = [
  { id: "in", label: "In" },
  { id: "at", label: "At" },
  { id: "every", label: "Every" },
];

const CONTROL =
  "h-7 rounded-lg border border-input bg-transparent px-2 text-sm text-foreground outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 aria-invalid:border-destructive dark:bg-input/30";

/** The In | At | Every switch (the Agents tab's segmented control, at h-6). */
function KindSwitch({
  value,
  onChange,
}: {
  value: ReminderInput["kind"];
  onChange: (next: ReminderInput["kind"]) => void;
}) {
  const refs = useRef<(HTMLButtonElement | null)[]>([]);
  const onKeyDown = (e: KeyboardEvent<HTMLButtonElement>, i: number) => {
    const last = KINDS.length - 1;
    const next =
      e.key === "ArrowRight"
        ? (i + 1) % KINDS.length
        : e.key === "ArrowLeft"
          ? (i + last) % KINDS.length
          : e.key === "Home"
            ? 0
            : e.key === "End"
              ? last
              : null;
    if (next === null) return;
    e.preventDefault();
    onChange(KINDS[next].id);
    refs.current[next]?.focus();
  };
  return (
    <div
      role="tablist"
      aria-label="When"
      className="inline-flex h-6 self-start rounded-lg bg-muted p-0.5"
    >
      {KINDS.map((k, i) => (
        <button
          key={k.id}
          ref={(el) => {
            refs.current[i] = el;
          }}
          type="button"
          role="tab"
          aria-selected={value === k.id}
          tabIndex={value === k.id ? 0 : -1}
          onKeyDown={(e) => onKeyDown(e, i)}
          onClick={() => onChange(k.id)}
          className={cn(
            "h-5 rounded-md px-2 text-xs font-medium transition-colors duration-[var(--ib-motion-tap)] focus-visible:ring-1 focus-visible:ring-ring focus-visible:outline-none",
            value === k.id
              ? "bg-background text-foreground ring-1 ring-border"
              : "text-muted-foreground hover:text-foreground",
          )}
        >
          {k.label}
        </button>
      ))}
    </div>
  );
}

function FieldError({ id, text }: { id: string; text: string | undefined }) {
  if (!text) return null;
  return (
    <p id={id} className="text-xs text-destructive">
      {text}
    </p>
  );
}

/**
 * "Remind this agent…" (design §4.13): when (in / at / every), a message, a
 * live preview, then Set reminder. The timer is set on the tab's own machine
 * with the person as its author, and only ever wakes that tab.
 */
export function RemindForm({
  target,
  agentName,
  offline = false,
  machineLabel = "This Mac",
  onBack,
  onDone,
}: {
  target: TimerTarget;
  /** Null for an unnamed session: "Remind this agent". */
  agentName: string | null;
  /** The remote tab's machine is unreachable: nothing can be set there now. */
  offline?: boolean;
  machineLabel?: string;
  /** Back to the Timers list, when it opened from there. */
  onBack?: () => void;
  onDone: () => void;
}) {
  const ids = useId();
  const who = agentName ?? "the agent";
  const timerCount = useSessionTimersStore(
    (s) => s.bySession[timerTargetKey(target)]?.rows?.length ?? 0,
  );
  const [kind, setKind] = useState<ReminderInput["kind"]>("in");
  const [inAmount, setInAmount] = useState("20");
  const [everyAmount, setEveryAmount] = useState("15");
  const [unit, setUnit] = useState<ReminderInput["unit"]>("minutes");
  const [time, setTime] = useState("");
  const [stop, setStop] = useState<ReminderInput["stop"]>("tab");
  const [stopTime, setStopTime] = useState("");
  const [stopCount, setStopCount] = useState("");
  const [message, setMessage] = useState("");
  const [tried, setTried] = useState(false);
  const [serverError, setServerError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const now = useNowClock(30_000);
  const messageRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    messageRef.current?.focus();
  }, []);

  const input: ReminderInput = {
    kind,
    amount: kind === "every" ? everyAmount : inAmount,
    unit,
    time,
    stop,
    stopTime,
    stopCount,
    message,
    timerCount,
  };
  const plan = planReminder(input, now);
  const errors: Partial<Record<ReminderField, string>> = tried && !plan.ok ? plan.errors : {};
  // The preview reads what the schedule would do, whether or not a message is in yet.
  const previewPlan = planReminder({ ...input, message: message || "-" }, now);
  const preview = previewPlan.ok ? reminderPreview(previewPlan, who, shortTime) : null;
  const tooLong = new TextEncoder().encode(message).length > MESSAGE_CAP;
  // The limit is said at once: no point filling in a form that cannot be set.
  const atLimit = (!plan.ok && plan.errors.form) || null;

  const submit = async () => {
    setTried(true);
    setServerError(null);
    const fresh = planReminder(input, Date.now());
    if (!fresh.ok || tooLong || saving || offline) return;
    setSaving(true);
    try {
      const set = await useSessionTimersStore.getState().remind(target, fresh.ask);
      toast(`Reminder set for ${shortTime(Date.parse(set.nextAt))}.`);
      onDone();
    } catch (e) {
      setServerError(getErrorMessage(e));
    } finally {
      setSaving(false);
    }
  };
  const submitOnEnter = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      void submit();
    }
  };

  const amountId = `${ids}-amount`;
  const amount = (
    <>
      <Input
        aria-label="Amount"
        inputMode="numeric"
        className={cn(CONTROL, "w-16 tabular-nums")}
        value={kind === "every" ? everyAmount : inAmount}
        aria-invalid={!!errors.amount || undefined}
        aria-describedby={errors.amount ? amountId : undefined}
        onChange={(e) => (kind === "every" ? setEveryAmount : setInAmount)(e.target.value)}
        onKeyDown={submitOnEnter}
      />
      <select
        aria-label="Unit"
        className={CONTROL}
        value={unit}
        onChange={(e) => setUnit(e.target.value as ReminderInput["unit"])}
      >
        <option value="minutes">minutes</option>
        <option value="hours">hours</option>
      </select>
    </>
  );

  // Tab stays inside the form (it is a form in a popover); Escape closes it.
  const formRef = useRef<HTMLFormElement>(null);
  const trapTab = (e: KeyboardEvent<HTMLFormElement>) => {
    if (e.key !== "Tab" || !formRef.current) return;
    const focusable = [
      ...formRef.current.querySelectorAll<HTMLElement>(
        'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex="0"]',
      ),
    ];
    if (!focusable.length) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  };

  return (
    <form
      ref={formRef}
      onKeyDown={trapTab}
      className="flex flex-col"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <header className="flex items-center gap-1 border-b border-border px-3 py-1.5">
        {onBack && (
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            aria-label="Back to timers"
            onClick={onBack}
          >
            <ArrowLeft />
          </Button>
        )}
        <h2 className="flex-1 truncate text-sm font-semibold">
          {agentName ? `Remind ${agentName}` : "Remind this agent"}
        </h2>
        <Button type="button" variant="ghost" size="icon-xs" aria-label="Close" onClick={onDone}>
          <X />
        </Button>
      </header>
      <div className="flex flex-col gap-2.5 p-3">
        {offline && (
          <p role="status" className="text-xs">
            {machineLabel} is offline. Its timers keep firing there; you can set a reminder once
            it's back.
          </p>
        )}
        <KindSwitch value={kind} onChange={setKind} />
        {kind === "at" ? (
          <div className="flex items-center gap-1.5 text-sm">
            <span className="w-10 text-muted-foreground">At</span>
            <Input
              type="time"
              aria-label="Time"
              className={cn(CONTROL, "w-28 tabular-nums")}
              value={time}
              aria-invalid={!!errors.time || undefined}
              onChange={(e) => setTime(e.target.value)}
              onKeyDown={submitOnEnter}
            />
          </div>
        ) : (
          <div className="flex items-center gap-1.5 text-sm">
            <span className="w-10 text-muted-foreground">{kind === "every" ? "Every" : "In"}</span>
            {amount}
          </div>
        )}
        <FieldError id={amountId} text={errors.amount} />
        <FieldError id={`${ids}-time`} text={errors.time} />
        {kind === "every" && (
          <>
            <div className="flex flex-wrap items-center gap-1.5 text-sm">
              <span className="w-10 text-muted-foreground">Stop</span>
              <select
                aria-label="Stop after"
                className={CONTROL}
                value={stop}
                onChange={(e) => setStop(e.target.value as ReminderInput["stop"])}
              >
                <option value="tab">when the tab ends</option>
                <option value="time">at a time…</option>
                <option value="count">after N times…</option>
              </select>
              {stop === "time" && (
                <Input
                  type="time"
                  aria-label="Stop at"
                  className={cn(CONTROL, "w-28 tabular-nums")}
                  value={stopTime}
                  aria-invalid={!!errors.stopTime || undefined}
                  onChange={(e) => setStopTime(e.target.value)}
                  onKeyDown={submitOnEnter}
                />
              )}
              {stop === "count" && (
                <Input
                  aria-label="Times"
                  inputMode="numeric"
                  className={cn(CONTROL, "w-16 tabular-nums")}
                  value={stopCount}
                  aria-invalid={!!errors.stopCount || undefined}
                  onChange={(e) => setStopCount(e.target.value)}
                  onKeyDown={submitOnEnter}
                />
              )}
            </div>
            <FieldError id={`${ids}-stop`} text={errors.stopTime ?? errors.stopCount} />
          </>
        )}
        <label className="flex flex-col gap-1 text-xs font-medium text-muted-foreground">
          Message
          <textarea
            ref={messageRef}
            className="min-h-16 w-full resize-y rounded-lg border border-input bg-transparent px-2.5 py-1.5 text-sm font-normal text-foreground outline-none placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 aria-invalid:border-destructive dark:bg-input/30"
            placeholder={`What ${who} should do when reminded`}
            value={message}
            aria-invalid={!!errors.message || tooLong || undefined}
            onChange={(e) => setMessage(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                e.preventDefault();
                void submit();
              }
            }}
          />
        </label>
        <FieldError id={`${ids}-message`} text={errors.message} />
        {message.length > COUNTER_FROM && (
          <p
            className={cn(
              "text-xs tabular-nums",
              tooLong ? "text-destructive" : "text-muted-foreground",
            )}
          >
            {new TextEncoder().encode(message).length} / {MESSAGE_CAP} bytes
          </p>
        )}
        <p aria-live="polite" className="min-h-4 text-xs text-muted-foreground">
          {preview}
        </p>
        {(atLimit || serverError) && (
          <p role="alert" className="text-xs text-destructive">
            {atLimit ?? serverError}
          </p>
        )}
        <div className="flex justify-end gap-2">
          <Button type="button" size="sm" variant="outline" onClick={onDone}>
            Cancel
          </Button>
          <Button
            type="submit"
            size="sm"
            data-testid="remind-submit"
            disabled={saving || !!atLimit || offline}
          >
            {saving && <Loader2 className="animate-spin" />}
            Set reminder
          </Button>
        </div>
      </div>
    </form>
  );
}

/** "Remind this agent…" from the tab menu: the Remind form, hung from the tab
 *  pill. With `startOn="timers"` it is the Timers popover. */
export function RemindPopover(props: Omit<SessionTimersPopoverProps, "trigger">) {
  return <SessionTimersPopover startOn="remind" {...props} />;
}
