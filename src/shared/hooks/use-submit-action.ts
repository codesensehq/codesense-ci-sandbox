// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";

/**
 * SW-GT-6 (T8) · Generic async-submit orchestration: owns the in-flight
 * `submitting` flag, the success/error toasts, and the try/catch — so callers
 * that persist an edit (a registry commit, a `saveTeamConfig` write) don't
 * re-implement the same dance. `onDone` fires ONLY on success.
 *
 * Extracted from members-card's private `useRegistryEditSubmit`; that hook now
 * delegates here, keeping only its not-signed-in guard + commit call. The
 * settings config editors (phases / transitions / field-schema panels) share
 * this one seam for their optimistic saves.
 *
 * Non-blocking: the `action` runs off the webview thread (a Tauri `invoke`);
 * nothing here awaits on the render path.
 */
export interface SubmitActionOptions {
  /** Toast shown on success. */
  success: string;
  /** Prefix for the error toast (`${errorPrefix}: ${message}`). */
  errorPrefix: string;
  /** Called after a successful action (e.g. close a dialog, reset state). */
  onDone?: () => void;
  /** Optional in-surface error handler for forms that keep user input available for retry. */
  onError?: (message: string) => void;
}

export interface UseSubmitAction {
  /** True while an action is in flight — drives disabled/spinner states. */
  submitting: boolean;
  /** Run `action`, guarding re-entry, toasting the outcome. */
  submit: (action: () => Promise<void>, opts: SubmitActionOptions) => Promise<void>;
}

export function useSubmitAction(): UseSubmitAction {
  const [submitting, setSubmitting] = useState(false);
  const inFlight = useRef(false);
  const generation = useRef(0);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      generation.current += 1;
    };
  }, []);

  const submit = async (action: () => Promise<void>, opts: SubmitActionOptions) => {
    // Re-entry guard: a double-click / Enter-while-in-flight is a no-op.
    if (inFlight.current) return;
    inFlight.current = true;
    const current = ++generation.current;
    setSubmitting(true);
    try {
      await action();
      if (!mounted.current || current !== generation.current) return;
      toast.success(opts.success);
      opts.onDone?.();
    } catch (e) {
      if (!mounted.current || current !== generation.current) return;
      const message = e instanceof Error ? e.message : String(e);
      toast.error(`${opts.errorPrefix}: ${message}`);
      opts.onError?.(message);
    } finally {
      if (current === generation.current) inFlight.current = false;
      if (mounted.current && current === generation.current) setSubmitting(false);
    }
  };

  return { submitting, submit };
}
