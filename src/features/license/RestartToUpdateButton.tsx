// SPDX-License-Identifier: MIT
import { useEffect, useId, useRef, useState } from "react";

import { AppButton as Button } from "@/shared/components/app-button";

import { licenseCancelRestart, licenseHeadlessRuns, licenseRestartToUpdate } from "./api";
import { COPY } from "./copy";

/** How long an accepted Restart to update may take before the button resets. */
export const RESTART_TIMEOUT_MS = 30_000;

/**
 * SW-PB-23 AC-1: installs the staged build and relaunches. Only ever the user's own click: the
 * app asks the same gate as the install at quit, and an error says why nothing happened.
 */
export function RestartToUpdateButton({
  label = COPY.restartToUpdate,
  testId,
  variant = "outline",
}: {
  label?: string;
  testId: string;
  variant?: "outline" | "default";
}) {
  const [restarting, setRestarting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Counting the agent runs a restart would end.
  const [checking, setChecking] = useState(false);
  // Agent runs without a terminal that the restart would end: the user confirms first. A
  // count that couldn't be read is null, and still asks (fail closed).
  const [confirm, setConfirm] = useState<{ runs: number | null } | null>(null);
  const labelId = useId();
  // Bumped by Cancel, so a recount that was in flight then is ignored: the cancel wins.
  const generation = useRef(0);
  const trigger = useRef<HTMLButtonElement>(null);
  // Focus goes back to the button once the question has gone and the button is enabled again.
  const refocus = useRef(false);
  const close = () => {
    generation.current += 1;
    refocus.current = true;
    setConfirm(null);
  };
  useEffect(() => {
    if (confirm === null && !checking && !restarting && refocus.current) {
      refocus.current = false;
      trigger.current?.focus();
    }
  }, [confirm, checking, restarting]);
  // The app normally closes within seconds of an accepted click. If it is still here after
  // RESTART_TIMEOUT_MS, the button comes back and says so.
  useEffect(() => {
    if (!restarting) return;
    const id = setTimeout(() => {
      // The exit never came: drop the armed restart, so a later plain quit doesn't relaunch.
      licenseCancelRestart().catch(() => {});
      refocus.current = true;
      setRestarting(false);
      setError(COPY.restartTimedOut);
    }, RESTART_TIMEOUT_MS);
    return () => clearTimeout(id);
  }, [restarting]);
  const count = async (): Promise<number | null> => {
    setChecking(true);
    try {
      return await licenseHeadlessRuns();
    } catch {
      return null;
    } finally {
      setChecking(false);
    }
  };
  const ask = async () => {
    setError(null);
    const asked = generation.current;
    const runs = await count();
    if (asked !== generation.current) return;
    if (runs === 0) {
      await restart();
      return;
    }
    setConfirm({ runs });
  };
  // Counted again on yes: more runs than the question named ask again.
  const confirmed = async () => {
    const shown = confirm?.runs ?? null;
    const asked = generation.current;
    const runs = await count();
    if (asked !== generation.current) return;
    if (runs !== 0 && (runs === null || shown === null ? runs !== shown : runs > shown)) {
      setConfirm({ runs });
      return;
    }
    await restart();
  };
  const restart = async () => {
    setConfirm(null);
    setRestarting(true);
    setError(null);
    try {
      await licenseRestartToUpdate();
    } catch (e) {
      refocus.current = true;
      setError(typeof e === "string" && e.trim() !== "" ? e : COPY.restartFailed);
      setRestarting(false);
    }
  };
  return (
    <span className="inline-flex flex-wrap items-center gap-2">
      <Button
        ref={trigger}
        size="sm"
        variant={variant}
        onClick={ask}
        disabled={restarting || checking || confirm !== null}
        data-testid={testId}
      >
        {restarting ? COPY.restarting : label}
      </Button>
      {confirm !== null && (
        <span
          className="inline-flex flex-wrap items-center gap-2 text-xs"
          role="alertdialog"
          aria-labelledby={labelId}
          data-testid={`${testId}-confirm`}
          onKeyDown={(e) => {
            if (e.key === "Escape") close();
          }}
        >
          <span id={labelId} aria-live="polite">
            {confirm.runs === null
              ? COPY.confirmRestartRunsUnknown
              : COPY.confirmRestartRuns(confirm.runs)}
          </span>
          <Button
            size="sm"
            variant="default"
            onClick={confirmed}
            disabled={checking}
            data-testid={`${testId}-confirm-yes`}
          >
            {COPY.restartAnyway}
          </Button>
          <Button
            size="sm"
            variant="outline"
            autoFocus
            onClick={close}
            data-testid={`${testId}-confirm-no`}
          >
            {COPY.cancel}
          </Button>
        </span>
      )}
      {error && (
        <span className="text-xs text-destructive" data-testid={`${testId}-error`}>
          {error}
        </span>
      )}
    </span>
  );
}
