// SPDX-License-Identifier: MIT
import type { ReactNode } from "react";
import {
  ChevronRight,
  CircleAlert,
  FolderOpen,
  Inbox,
  Lock,
  ShieldCheck,
  TriangleAlert,
  Wallet,
  Wrench,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Meter } from "@/components/ui/meter";
import { cn } from "@/lib/utils";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import type { AgentDetailView, AgentRow } from "@/tauri/commands";

import { agentSentence } from "../lib/agent-display";
import { useAgentsStore } from "../stores/agents-store";
import { SpendCapChip } from "./spend-cap";
import { showAgentCap } from "./agents-tab-nav";

/** Tools shown on the sheet before "+N more". */
const TOOLS_SHOWN = 4;

/** One line of the fact sheet: a label, its value and a way into the editor section that
 *  changes it. The whole row is the button. */
function Fact({
  icon,
  label,
  children,
  onOpen,
  openLabel,
  testId,
}: {
  icon: ReactNode;
  label: string;
  children: ReactNode;
  onOpen: () => void;
  openLabel: string;
  testId?: string;
}) {
  return (
    <div
      role="button"
      tabIndex={0}
      aria-label={openLabel}
      data-testid={testId}
      onClick={onOpen}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onOpen();
        }
      }}
      className="group/fact grid cursor-pointer grid-cols-[128px_minmax(0,1fr)] rounded-sm border-b border-border/70 outline-none hover:bg-muted/55 focus-visible:ring-2 focus-visible:ring-ring"
    >
      <dt className="flex min-h-[38px] items-center gap-2 py-1 pl-2 text-xs text-muted-foreground [&_svg]:size-3.5">
        {icon}
        {label}
      </dt>
      <dd className="m-0 flex min-h-[38px] min-w-0 flex-wrap items-center gap-2 py-1 pr-2 text-sm">
        {children}
        <ChevronRight
          aria-hidden="true"
          className="ml-auto size-3.5 shrink-0 text-muted-foreground opacity-60 group-hover/fact:text-foreground group-hover/fact:opacity-100"
        />
      </dd>
    </div>
  );
}

const SUB = "text-xs text-muted-foreground";

/** The spend row: today against the cap with a meter, or an offer to set one. */
function SpendFact({ row }: { row: AgentRow }) {
  const value = useAgentListProjectionStore((s) => s.spend[`agent:${row.key}`]);
  const perDay = value?.cap?.perDay ?? null;
  const ratio = perDay ? value!.today / perDay : 0;
  return (
    <Fact
      icon={<Wallet />}
      label="Spend"
      openLabel={`Spend cap for ${row.name}`}
      testId="agent-fact-spend"
      onOpen={() => showAgentCap(row.key)}
    >
      {value?.cap ? (
        <>
          <SpendCapChip agentKey={row.key} />
          {perDay ? <Meter value={value.today} max={perDay} label="Spend today" /> : null}
          {ratio >= 0.9 && ratio < 1 && (
            <span className={cn("text-xs font-medium", "text-[var(--cs-warning-text)]")}>
              {Math.round(ratio * 100)}% of the cap
            </span>
          )}
        </>
      ) : (
        <>
          <span className="text-muted-foreground">No cap set</span>
          <span className={SUB}>Set a cap</span>
        </>
      )}
    </Fact>
  );
}

/**
 * The agent at a glance: spend, tools, what it starts with and its permissions, one row each.
 * Every row opens the Edit section that changes it. Runs on, account, health and next run are
 * in the strip above.
 */
export function AgentFacts({ row, detail }: { row: AgentRow; detail: AgentDetailView }) {
  const s = agentSentence(detail);
  const openSection = useAgentsStore((st) => st.openEditSection);
  const builtin = detail.scope === "builtin";
  const servers = detail.definition.mcp?.servers ?? [];
  // The team's MCP offer, bridge aside: say it on the fact, so "why can't my agent reach Slack?"
  // is answered on sight.
  const offered = detail.resolved.offered_mcp_servers ?? [];
  const open = (section: string) => () => openSection(row.key, section);
  return (
    <dl aria-label={`${row.name} at a glance`} className="m-0 border-t border-border/70">
      {!builtin && <SpendFact row={row} />}
      <Fact
        icon={<Wrench />}
        label="Tools"
        openLabel={`Tools for ${row.name}`}
        testId="agent-fact-tools"
        onOpen={open("tools")}
      >
        <span className="inline-flex h-[22px] items-center gap-1 rounded-full bg-muted px-2 text-xs text-muted-foreground">
          <Lock aria-hidden="true" className="size-3" />
          codesense
        </span>
        {servers.slice(0, TOOLS_SHOWN - 1).map((name) => (
          <span
            key={name}
            className="inline-flex h-[22px] items-center rounded-full border border-border/70 px-2 text-xs"
          >
            {name}
          </span>
        ))}
        {servers.length > TOOLS_SHOWN - 1 && (
          <span className={SUB}>+{servers.length - (TOOLS_SHOWN - 1)} more</span>
        )}
        {servers.length === 0 && <span className={SUB}>no servers beyond CodeSense</span>}
        {servers.length === 0 && offered.length > 0 && (
          <span
            data-testid="agent-mcp-offered"
            className="flex basis-full items-start gap-1.5 text-xs font-medium text-warning-ink"
          >
            <TriangleAlert aria-hidden="true" className="mt-0.5 size-3 shrink-0" />
            {builtin ? (
              <span>
                This starter has no MCP servers beyond the bridge. The team offers:{" "}
                {offered.join(", ")}. Click <strong>Use template</strong> to copy it, then check the
                servers it should reach.
              </span>
            ) : (
              <span>
                This agent has no MCP servers beyond the bridge. The team offers:{" "}
                {offered.join(", ")}. Edit the agent and check the servers it needs.
              </span>
            )}
          </span>
        )}
      </Fact>
      <Fact
        icon={<FolderOpen />}
        label="Starts with"
        openLabel={`What ${row.name} starts with`}
        testId="agent-fact-starts"
        onOpen={open("starts-with")}
      >
        <span>{s.startup}</span>
        {s.errorCount > 0 && (
          <Badge tone="failed" data-testid="agent-manifest-errors" className="gap-1">
            <CircleAlert aria-hidden="true" className="size-3" />
            {s.errorCount === 1 ? "1 manifest error" : `${s.errorCount} manifest errors`}
          </Badge>
        )}
      </Fact>
      <Fact
        icon={<ShieldCheck />}
        label="Permissions"
        openLabel={`Permissions for ${row.name}`}
        testId="agent-fact-permissions"
        onOpen={open("permissions")}
      >
        <span data-testid="agent-sentence-permissions">{s.permissions}</span>
      </Fact>
      {row.role && (
        <Fact
          icon={<Inbox />}
          label="Role"
          openLabel={`Role for ${row.name}`}
          testId="agent-fact-role"
          onOpen={open("basics")}
        >
          <span>Joins {row.role} when launched</span>
        </Fact>
      )}
    </dl>
  );
}
