// SPDX-License-Identifier: MIT
import type { FlatNavItem } from "@/features/dashboard/lib/flatten-nav-tree";

import type { ContextMembership, ContextTierTag } from "../lib/agent-context-model";
import { navItemPath, type NavAgentContext } from "../lib/nav-agent-context";

/** The tag's word per tier. The scratch tier is "private": the product's one name for it. */
const TIER_LABEL: Record<ContextTierTag, string> = {
  team: "team",
  repo: "repo",
  personal: "private",
};

/** The word in the tag slot. Every in-set or partial row has one, so membership never rests
 *  on colour alone: its tier, "mixed" (several tiers), "in" (tier unknown past the file cap),
 *  or "some" (a folder with only some of its files in the set). */
function tagWord(m: ContextMembership): string | null {
  if (m.state === "partial") return "some";
  if (m.state !== "in") return null;
  if (m.tier) return TIER_LABEL[m.tier];
  return m.mixed ? "mixed" : "in";
}

function srText(m: ContextMembership, ctx: NavAgentContext): string {
  if (m.state === "out") return `not in ${ctx.agentName}'s context`;
  if (m.state === "partial") return `partly in ${ctx.agentName}'s context`;
  if (m.tier) return tierTitle(m.tier, ctx);
  return m.mixed
    ? `${ctx.agentName} gets this from several places`
    : `in ${ctx.agentName}'s context`;
}

function tierTitle(tier: ContextTierTag, ctx: NavAgentContext): string {
  switch (tier) {
    case "repo":
      return `${ctx.agentName} gets this from ${ctx.repoName ?? "the repo"}'s own content`;
    case "personal":
      return `${ctx.agentName} gets your Private copy of this`;
    case "team":
      return `${ctx.agentName} gets this from the team`;
  }
}

/**
 * SW-XS-15: draw one nav row against the selected agent's want set. A row in the set keeps
 * full ink and carries a quiet tag (team / repo / private, or mixed / in / some); a row outside it is muted
 * (`.cs-agent-ctx-out`, back to full ink on hover or focus). Rows with no content path (pins,
 * placeholders), and every row while no agent is picked, render untouched.
 */
export function withAgentContext(
  item: FlatNavItem,
  row: React.ReactElement,
  ctx: NavAgentContext | undefined,
): React.ReactElement {
  if (!ctx) return row;
  const abs = navItemPath(item);
  const rel = abs ? ctx.relOf(abs) : null;
  if (!rel) return row;
  const m = ctx.membership(rel);
  return (
    <div
      className={`flex min-w-0 items-center ${m.state === "out" ? "cs-agent-ctx-out" : ""}`}
      data-agent-context={m.state}
      data-agent-context-tier={m.tier}
    >
      <div className="min-w-0 flex-1">{row}</div>
      {/* One fixed slot on every row, so the row actions line up; screen readers hear the
          membership here, since the muting itself is visual. */}
      <span className="cs-agent-ctx-tag" title={m.state === "out" ? undefined : srText(m, ctx)}>
        {tagWord(m) && <span aria-hidden="true">{tagWord(m)}</span>}
        <span className="sr-only">{srText(m, ctx)}</span>
      </span>
    </div>
  );
}
