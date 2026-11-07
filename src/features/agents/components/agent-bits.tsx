// SPDX-License-Identifier: MIT
import {
  Code2,
  Terminal,
  Lock,
  ShieldAlert,
  Sparkles,
  SquareTerminal,
  type LucideIcon,
} from "lucide-react";

import type { AgentRuntime } from "@/tauri/commands";

import { BUILTIN_READ_ONLY, runtimeLabel } from "../lib/agent-display";

/** Each CLI's icon. Colours live in agents.css (`.cs-runtime-badge`), from `--cs-runtime-*`. */
const RUNTIME_ICON: Record<AgentRuntime, LucideIcon> = {
  claude: Sparkles,
  codex: SquareTerminal,
  opencode: Code2,
  hermes: Terminal,
};

/** Which CLI an agent runs on, as a small labelled icon. */
export function RuntimeBadge({ runtime }: { runtime: AgentRuntime }) {
  const Icon = RUNTIME_ICON[runtime];
  return (
    <span
      data-testid="agent-runtime"
      data-runtime={runtime}
      title={runtimeLabel(runtime)}
      className="cs-runtime-badge inline-flex shrink-0 items-center gap-1 rounded px-1 py-px text-[11px] font-semibold"
    >
      <Icon aria-hidden="true" className="size-3" />
      {runtimeLabel(runtime)}
    </span>
  );
}

/** The launch auto-approves tool use (an escalating flag or permission mode). A label, never a
 *  block: CodeSense shows what an agent does, the CLI decides. */
export function AutoApprovesBadge() {
  return (
    <span
      data-testid="agent-auto-approves"
      title="A launch runs tools without asking"
      className="inline-flex items-center gap-1 rounded-full bg-warning-wash px-1.5 text-[11px] font-medium leading-5 text-warning-ink"
    >
      <ShieldAlert aria-hidden="true" className="size-3" />
      auto-approves tools
    </span>
  );
}

/** A starter CodeSense ships: read-only, copied to customize. */
export function BuiltinBadge() {
  return (
    <span
      data-testid="agent-builtin-badge"
      title={BUILTIN_READ_ONLY}
      className="inline-flex shrink-0 items-center gap-1 rounded-full bg-muted px-1.5 text-[11px] font-medium leading-5 text-muted-foreground"
    >
      <Lock aria-hidden="true" className="size-3" />
      Built-in
    </span>
  );
}
