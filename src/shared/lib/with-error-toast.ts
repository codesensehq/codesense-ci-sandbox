// SPDX-License-Identifier: MIT
import { toast } from "sonner";
import { getErrorMessage, userFacingErrorMessage } from "./error-message";
import { logger } from "./logger";

/** Toast `${prefix}${message}` for an unknown thrown value. The one shared unit
 *  behind every error-toast site — works for `try/catch` (sync + async) blocks
 *  directly, and for terminal `.catch` chains via `withErrorToast`. Centralizes
 *  the `toast.error(\`…: ${getErrorMessage(err)}\`)` idiom that was duplicated
 *  across the viewer/dashboard features. */
export function toastError(prefix: string, err: unknown): void {
  toast.error(`${prefix}${userFacingErrorMessage(err)}`);
}

/** Fire-and-forget: run `promise`, toast + SWALLOW on rejection. Returns `void`
 *  — it is never awaited, so it can't serialize an async event handler. Sugar
 *  for the common `x().catch((err) => toastError(prefix, err))` shape. */
export function withErrorToast(promise: Promise<unknown>, prefix: string): void {
  void promise.catch((err: unknown) => toastError(prefix, err));
}

/** The one next action a failure toast offers. */
export interface ToastFailureAction {
  label: string;
  onClick: () => void;
}

/**
 * SW-FU-B8 (consistency spec, Rule 5): the shared failure toast, for background
 * work only (a form's own result goes in its InlineAlert). The title says what
 * failed, the description is the reason in words (never raw error text), and at
 * most one next action is offered.
 */
export function toastFailure(title: string, err: unknown, action?: ToastFailureAction): void {
  const description = userFacingErrorMessage(err);
  // The raw value goes to the log (the safe reason points there), never the screen.
  logger.warn("error", `${title}: ${getErrorMessage(err)}`);
  toast.error(title, action ? { description, action } : { description });
}
