// SPDX-License-Identifier: MIT
import { CheckCircle2 } from "lucide-react";

import { CONTEXT_LANDS_ATTACHED, CONTEXT_LANDS_MANAGED } from "@/shared/lib/onboarding-copy";

interface Props {
  /** `owner/repo` shown in the heading. */
  label: string;
  /** CodeSense cloned and manages this working copy ("Clone for me"). */
  managed: boolean;
}

/**
 * SW-LN-29 (AC-1): the attach success state. Says the repo is attached and where
 * the team context lands for this working copy.
 */
export function AttachSuccessNotice({ label, managed }: Props) {
  return (
    <div
      data-testid="attach-success"
      className="flex flex-col gap-3 rounded-md border border-emerald-500/40 bg-emerald-500/5 px-4 py-3 text-sm"
    >
      <p role="status" className="flex items-center gap-2 font-medium">
        <CheckCircle2 className="size-4 text-emerald-600 dark:text-emerald-400" aria-hidden />
        Attached {label}
      </p>
      <p className="text-muted-foreground">
        {managed ? CONTEXT_LANDS_MANAGED : CONTEXT_LANDS_ATTACHED}
      </p>
    </div>
  );
}
