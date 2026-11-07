// SPDX-License-Identifier: MIT
import { Check, Eye, Ban } from "lucide-react";

import { cn } from "@/lib/utils";

/**
 * SW-WV-F5 (AC-5) — the recipient-ack echo strip on an assertion row.
 *
 * When an assertion has recorded recipient reactions (👀 seen / ✅ acknowledged /
 * 🚫 declined — recorded via `wv_record_recipient_ack`, SEPARATE from owner
 * ratification), this renders one small icon+count chip per NON-ZERO group. A zero
 * count is omitted (no "seen 0" noise); an all-zero strip renders nothing. A non-zero
 * declined count is toned `text-destructive` — a decline never ratifies (the wall
 * never moves on a recipient signal), it routes a finding back to the worker, so the
 * operator should see it stand out.
 */
export interface RecipientAckEchoProps {
  acks: { seen: number; acknowledged: number; declined: number };
}

export function RecipientAckEcho({ acks }: RecipientAckEchoProps): React.JSX.Element | null {
  const { seen, acknowledged, declined } = acks;
  if (seen === 0 && acknowledged === 0 && declined === 0) return null;

  return (
    <span
      data-testid="recipient-ack-echo"
      className="flex items-center gap-1.5 text-[10px] tabular-nums text-muted-foreground"
    >
      {seen > 0 && (
        <span data-testid="ack-chip-seen" className="flex items-center gap-0.5">
          <Eye size={11} aria-hidden />
          <span>seen {seen}</span>
        </span>
      )}
      {acknowledged > 0 && (
        <span data-testid="ack-chip-acknowledged" className="flex items-center gap-0.5">
          <Check size={11} aria-hidden />
          <span>ack {acknowledged}</span>
        </span>
      )}
      {declined > 0 && (
        <span
          data-testid="ack-chip-declined"
          className={cn("flex items-center gap-0.5", "text-destructive")}
        >
          <Ban size={11} aria-hidden />
          <span>declined {declined}</span>
        </span>
      )}
    </span>
  );
}
