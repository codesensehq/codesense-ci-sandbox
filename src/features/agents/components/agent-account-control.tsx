// SPDX-License-Identifier: MIT
import { useEffect, useState, type ReactNode } from "react";

import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import type { AgentRow } from "@/tauri/commands";

import { runtimeLabel } from "../lib/agent-display";
import { useAgentRequest } from "../lib/use-agent-request";
import { useAgentsStore } from "../stores/agents-store";

/** The radio value that means "no binding: my default account". */
const DEFAULT = "";

/**
 * The account picker of the profile strip: which of your accounts runs this agent. Change opens
 * it as "Your account (only you)"; Fix, when the account can't launch, opens the same picker as
 * "Choose an account". A binding is yours alone, kept on this machine and never written to the
 * shared definition, so teammates launching the same agent use their own. Only accounts that log
 * into the agent's CLI are offered. `children` is the trigger button.
 */
export function AccountMenu({
  row,
  mode,
  answersRequests = false,
  children,
}: {
  row: Pick<AgentRow, "key" | "runtime" | "account">;
  mode: "change" | "fix";
  /** Opens when something asked for this agent's picker. Only the strip's menu does: the one
   *  place that is on screen for the Overview the request brings back. */
  answersRequests?: boolean;
  children: ReactNode;
}) {
  const accounts = useAgentsStore((s) => s.accounts);
  const accountsError = useAgentsStore((s) => s.accountsError);
  const saving = useAgentsStore((s) => s.pending[`account:${row.key}`] === true);
  const setAccount = useAgentsStore((s) => s.setAccount);
  const capture = useAgentsStore(
    (s) =>
      s.details[row.key]?.resolved.account.capture_raw_terminal ?? row.account.capture_raw_terminal,
  );
  const savingRaw = useAgentsStore((s) => s.pending[`raw:${row.key}`] === true);
  const setRawTerminalCapture = useAgentsStore((s) => s.setRawTerminalCapture);
  const [open, setOpen] = useState(false);
  // Something asked for this agent's picker (the launch sheet's Fix): the shown team's tab opens
  // it, once. A hidden retained team never sees the request.
  const asked = useAgentRequest("account-picker", answersRequests ? row.key : null);
  if (asked && !open) setOpen(true);
  const consume = asked?.consume;
  useEffect(() => consume?.(), [consume]);
  const offered = (accounts ?? []).filter((a) => a.provider === row.runtime && !a.detachedAt);
  const bound = row.account.bound_account_id;
  // A binding to an account no longer offered still shows as chosen.
  const boundMissing = bound !== null && !offered.some((a) => a.id === bound);
  // A failed accounts read still lets you go back to your default.
  const loading = accounts === null && accountsError === null;
  return (
    <DropdownMenu open={open} onOpenChange={setOpen}>
      <DropdownMenuTrigger asChild>{children}</DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-72" data-testid="agent-account-menu">
        <DropdownMenuLabel className="text-sm font-medium text-foreground">
          {mode === "fix" ? "Choose an account" : "Your account (only you)"}
        </DropdownMenuLabel>
        <p data-testid="agent-account-hint" className="px-1.5 pb-1.5 text-xs text-muted-foreground">
          One of your own {runtimeLabel(row.runtime)} CLI logins on this Mac. Teammates launch it
          with theirs.
        </p>
        {accountsError && (
          <p className="px-1.5 pb-1.5 text-xs text-destructive">
            Couldn&apos;t read your accounts: {accountsError}
          </p>
        )}
        <DropdownMenuRadioGroup
          value={bound ?? DEFAULT}
          onValueChange={(v) => void setAccount(row.key, v === DEFAULT ? null : v)}
        >
          <DropdownMenuRadioItem value={DEFAULT} disabled={saving || loading}>
            Use my default
          </DropdownMenuRadioItem>
          {offered.map((a) => (
            <DropdownMenuRadioItem key={a.id} value={a.id} disabled={saving || loading}>
              {a.label}
            </DropdownMenuRadioItem>
          ))}
          {boundMissing && (
            <DropdownMenuRadioItem value={bound} disabled>
              {bound} (unavailable)
            </DropdownMenuRadioItem>
          )}
        </DropdownMenuRadioGroup>
        <DropdownMenuSeparator />
        {/* Your consent to a raw terminal snapshot: yours alone, saved at once, and kept apart
            from the definition, so a built-in agent has it too. */}
        <DropdownMenuCheckboxItem
          data-testid="agent-raw-terminal-toggle"
          checked={capture}
          disabled={savingRaw}
          onSelect={(e) => e.preventDefault()}
          onCheckedChange={(next) => void setRawTerminalCapture(row.key, next === true)}
          className="items-start"
        >
          <span className="flex flex-col">
            <span>Retain raw terminal snapshot for completed runs (only you)</span>
            <span className="text-xs text-muted-foreground">
              Saves at once. Advanced diagnostics for your interactive launches of this agent.
              Stores a bounded, redacted terminal tail locally; terminal text and control sequences
              may still contain sensitive context. Headless launches never capture.
            </span>
          </span>
        </DropdownMenuCheckboxItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
