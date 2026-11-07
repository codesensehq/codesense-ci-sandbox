// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef, useState } from "react";
import { logger } from "@/shared/lib/logger";
import { toast } from "sonner";

/**
 * Shared clipboard-copy hook with inline "Copied" feedback.
 *
 * Returns `copiedValue` (the last successfully copied string, auto-clears
 * after 2 s) and `handleCopy(value)`. Cleans up the timer on unmount.
 */
export function useCopyToClipboard() {
  const [copiedValue, setCopiedValue] = useState<string | null>(null);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    return () => {
      if (timerRef.current) clearTimeout(timerRef.current);
    };
  }, []);

  const handleCopy = useCallback((value: string) => {
    // `navigator.clipboard` is absent outside a secure/focused context. Treat it
    // as a failed copy (same feedback as a rejected write) instead of throwing a
    // synchronous TypeError that would escape the `.catch()` into the caller.
    const clipboard = navigator.clipboard;
    if (!clipboard) {
      logger.warn("clipboard", "copy failed: Clipboard API unavailable");
      toast.error("Failed to copy");
      return;
    }
    clipboard
      .writeText(value)
      .then(() => {
        if (timerRef.current) clearTimeout(timerRef.current);
        setCopiedValue(value);
        timerRef.current = setTimeout(() => setCopiedValue(null), 2000);
      })
      .catch((e) => {
        logger.warn("clipboard", `copy failed: ${e}`);
        toast.error("Failed to copy");
      });
  }, []);

  return { copiedValue, handleCopy };
}
