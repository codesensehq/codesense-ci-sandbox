// SPDX-License-Identifier: MIT
import { useState } from "react";
import { Lock, Plus, X } from "lucide-react";

import { Input } from "@/components/ui/input";
import { AppButton as Button } from "@/shared/components/app-button";
import { cn } from "@/lib/utils";
import { agentCliLabel } from "@/shared/lib/agent-clis";
import { registeredCli } from "@/shared/lib/agent-runtimes";
import type { ResolvedAgentView } from "@/tauri/commands";

import {
  CLAUDE_MODES,
  CODEX_APPROVAL_POLICIES,
  CODEX_HEADLESS_DEFAULT_HINT,
  CODEX_SANDBOX_MODES,
  type AgentDraft,
  type ClaudeDraft,
} from "../lib/agent-form";
import { AutoApprovesBadge } from "./agent-bits";
import { FIELD, SELECT_TRIGGER } from "./agent-form-fields";
import { PortaledSelect } from "@/components/ui/portaled-select";

/** Whether a Claude `deny` or `ask` rule shuts out a tool CodeSense needs: the tool itself, the
 *  bridge as a whole, or every tool. Mirrors the backend's check, which only ever warns. */
export function blocksRequired(rule: string, required: readonly string[]): boolean {
  const r = rule.trim();
  return required.includes(r) || r === "*" || r === "mcp__codesense" || r === "mcp__codesense__*";
}

/** What a blocked required tool means, shown inline on the rule. */
const BLOCKS_CODESENSE =
  "invalid — blocks a tool CodeSense needs: the agent won't be able to signal_done or ask you";

/** What the editor says about tools no rule names: the wildcard rule's decision, else ask. */
export function wildcardCopy(rules: readonly { pattern: string; decision: string }[]): string {
  const decision = rules.find((rule) => rule.pattern.trim() === "*")?.decision;
  if (decision === "allow") return "run without asking";
  if (decision === "deny") return "are blocked";
  return "ask before running";
}

/** What an OpenCode launch actually enforces: the generated permission map laid over the
 *  per-tool grants the launch's MCP scope writes for a declared `bridge_tools` allowlist.
 *  Without an allowlist the scope adds no rules. Later keys win, as in OpenCode. */
export function effectiveOpenCodeRules(
  content: unknown,
  bridgeTools: readonly string[] | null,
): Record<string, unknown> {
  const generated = (content ?? {}) as Record<string, unknown>;
  if (bridgeTools === null) return generated;
  const scope: Record<string, unknown> = { "*": "ask", "codesense_*": "deny" };
  for (const tool of bridgeTools) scope[`codesense_${tool}`] = "allow";
  return { ...scope, ...generated };
}

function RuleList({
  label,
  rules,
  onChange,
  required = [],
  conflictsWith = [],
  placeholder,
}: {
  label: "allow" | "ask" | "deny";
  rules: string[];
  onChange: (next: string[]) => void;
  /** Rules CodeSense adds, shown first and not removable. */
  required?: readonly string[];
  /** Required rules a rule here would block. */
  conflictsWith?: readonly string[];
  placeholder: string;
}) {
  const [typed, setTyped] = useState("");
  const add = () => {
    const rule = typed.trim();
    if (!rule || rules.includes(rule)) return;
    onChange([...rules, rule]);
    setTyped("");
  };
  return (
    <div data-testid={`agent-rules-${label}`} className="flex flex-col gap-1">
      <span className="text-xs font-medium capitalize text-muted-foreground">{label}</span>
      <ul className="flex flex-col gap-0.5">
        {required.map((rule) => (
          <li
            key={`required:${rule}`}
            data-testid="agent-rule-required"
            className="flex items-start gap-1.5 text-xs text-muted-foreground"
          >
            <Lock aria-hidden="true" className="mt-0.5 size-3 shrink-0" />
            <span className="min-w-0">
              <span className="[overflow-wrap:anywhere] font-mono">{rule}</span>{" "}
              <span className="whitespace-nowrap">· required by CodeSense</span>
            </span>
          </li>
        ))}
        {rules.map((rule, i) => {
          const blocks = conflictsWith.length > 0 && blocksRequired(rule, conflictsWith);
          return (
            // Rules are kept as written, a repeated one included, so each is its own row.
            <li key={`${i}:${rule}`} data-testid="agent-rule" className="flex flex-col">
              <span className="flex min-w-0 items-center gap-1.5 font-mono text-xs">
                <span className="min-w-0 [overflow-wrap:anywhere]">{rule}</span>
                <button
                  type="button"
                  aria-label={`Remove ${label} rule ${rule}`}
                  className="rounded p-0.5 text-muted-foreground hover:bg-muted hover:text-foreground"
                  onClick={() => onChange(rules.filter((_, at) => at !== i))}
                >
                  <X aria-hidden="true" className="size-3" />
                </button>
              </span>
              {blocks && (
                <span data-testid="agent-rule-invalid" className="text-xs text-destructive">
                  {BLOCKS_CODESENSE}
                </span>
              )}
            </li>
          );
        })}
      </ul>
      <div className="flex items-center gap-1.5">
        <Input
          aria-label={`New ${label} rule`}
          className={cn("h-7 font-mono text-xs", FIELD)}
          value={typed}
          placeholder={placeholder}
          onChange={(e) => setTyped(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              add();
            }
          }}
        />
        <Button
          size="sm"
          variant="outline"
          aria-label={`Add ${label} rule`}
          disabled={!typed.trim()}
          onClick={add}
        >
          <Plus />
        </Button>
      </div>
    </div>
  );
}

function ModeSelect({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: string;
  options: readonly string[];
  onChange: (value: string) => void;
}) {
  return (
    <label className="flex flex-wrap items-center gap-2 text-sm">
      <span className="text-xs text-muted-foreground">{label}</span>
      <PortaledSelect
        ariaLabel={label}
        triggerClassName={cn(SELECT_TRIGGER, "w-44")}
        value={value}
        onValueChange={onChange}
        options={[
          { value: "", label: "unset" },
          ...options.map((o) => ({ value: o, label: o })),
          ...(value && !options.includes(value) ? [{ value, label: value }] : []),
        ]}
      />
    </label>
  );
}

/**
 * What the CLI may do without asking, in the CLI's own terms: Claude's allow, ask and deny
 * rule strings and default mode, or Codex's approval policy and sandbox mode. CodeSense never
 * enforces these; it passes them to the CLI verbatim, adds only the allows its own tools need,
 * and warns when a rule would shut those out. The effective settings each launch gets are
 * shown as generated.
 */
export function AgentPermissionsSection({
  draft,
  onChange,
  resolved,
}: {
  draft: AgentDraft;
  onChange: (next: AgentDraft) => void;
  /** The live preview; `null` until the first one lands. */
  resolved: ResolvedAgentView | null;
}) {
  const generated = resolved?.manifest.generated[0];
  // The preview's runtime can lag a switch by one debounce; its rules belong to that runtime.
  const current = generated && resolved?.manifest.runtime === draft.runtime ? generated : null;
  const required = current?.required ?? [];
  const setClaude = (patch: Partial<ClaudeDraft>) =>
    onChange({ ...draft, claude: { ...draft.claude, ...patch } });
  if (registeredCli(draft.runtime)?.permissionsDisplayOnly) {
    return (
      <section aria-label="Permissions" data-testid="agent-permissions">
        <h3 className="text-sm font-semibold">Permissions</h3>
        <p className="text-xs text-muted-foreground">
          {agentCliLabel(draft.runtime)} permissions are read-only here. Saved settings are
          preserved when you edit this agent. Overview shows the effective launch settings.
        </p>
      </section>
    );
  }
  return (
    <section
      aria-label="Permissions"
      data-testid="agent-permissions"
      className="@container flex flex-col gap-3"
    >
      <header className="flex flex-wrap items-center gap-2">
        <h3 className="text-sm font-semibold">Permissions</h3>
        {resolved?.launch.escalating && <AutoApprovesBadge />}
        <span className="text-xs text-muted-foreground">
          {draft.runtime === "codex"
            ? "Codex's"
            : draft.runtime === "opencode"
              ? "OpenCode's"
              : "Claude's"}{" "}
          own settings, passed as written. The CLI enforces them.
        </span>
      </header>
      {draft.runtime === "claude" ? (
        <div className="grid gap-3 @4xl:grid-cols-3">
          <RuleList
            label="allow"
            rules={draft.claude.allow}
            required={required}
            onChange={(allow) => setClaude({ allow })}
            placeholder="e.g. Bash(npm test:*)"
          />
          <RuleList
            label="ask"
            rules={draft.claude.ask}
            conflictsWith={required}
            onChange={(ask) => setClaude({ ask })}
            placeholder="e.g. WebFetch"
          />
          <RuleList
            label="deny"
            rules={draft.claude.deny}
            conflictsWith={required}
            onChange={(deny) => setClaude({ deny })}
            placeholder="e.g. Bash(rm -rf:*)"
          />
          <div className="@4xl:col-span-3">
            <ModeSelect
              label="Default mode"
              value={draft.claude.defaultMode}
              options={CLAUDE_MODES}
              onChange={(defaultMode) => setClaude({ defaultMode })}
            />
          </div>
        </div>
      ) : draft.runtime === "codex" ? (
        <div className="flex flex-col gap-2">
          <div className="flex flex-wrap gap-4">
            <ModeSelect
              label="approval_policy"
              value={draft.codex.approvalPolicy}
              options={CODEX_APPROVAL_POLICIES}
              onChange={(approvalPolicy) =>
                onChange({ ...draft, codex: { ...draft.codex, approvalPolicy } })
              }
            />
            <ModeSelect
              label="sandbox_mode"
              value={draft.codex.sandboxMode}
              options={CODEX_SANDBOX_MODES}
              onChange={(sandboxMode) =>
                onChange({ ...draft, codex: { ...draft.codex, sandboxMode } })
              }
            />
          </div>
          <p data-testid="agent-codex-hint" className="text-xs text-muted-foreground">
            {CODEX_HEADLESS_DEFAULT_HINT}
          </p>
        </div>
      ) : (
        <div className="flex flex-col gap-2">
          {draft.opencode.permission.map((rule, index) => (
            <div className="flex flex-wrap items-center gap-2" key={`opencode-permission-${index}`}>
              <input
                aria-label={`OpenCode tool pattern ${index + 1}`}
                className="min-w-40 rounded border border-border bg-background px-2 py-1 font-mono text-xs"
                value={rule.pattern}
                placeholder="e.g. bash"
                onChange={(event) => {
                  const permission = [...draft.opencode.permission];
                  permission[index] = { ...rule, pattern: event.target.value };
                  onChange({ ...draft, opencode: { ...draft.opencode, permission } });
                }}
              />
              <ModeSelect
                label={`OpenCode decision ${index + 1}`}
                value={rule.decision}
                options={["allow", "ask", "deny"]}
                onChange={(decision) => {
                  const permission = [...draft.opencode.permission];
                  permission[index] = { ...rule, decision };
                  onChange({ ...draft, opencode: { ...draft.opencode, permission } });
                }}
              />
              <button
                type="button"
                className="text-xs text-muted-foreground hover:text-foreground"
                onClick={() =>
                  onChange({
                    ...draft,
                    opencode: {
                      ...draft.opencode,
                      permission: draft.opencode.permission.filter((_, i) => i !== index),
                    },
                  })
                }
              >
                Remove
              </button>
            </div>
          ))}
          <button
            type="button"
            className="self-start text-xs text-muted-foreground hover:text-foreground"
            onClick={() =>
              onChange({
                ...draft,
                opencode: {
                  ...draft.opencode,
                  permission: [...draft.opencode.permission, { pattern: "", decision: "ask" }],
                },
              })
            }
          >
            Add OpenCode permission
          </button>
          <p className="text-xs text-muted-foreground">
            Unspecified tools {wildcardCopy(draft.opencode.permission)}.
          </p>
        </div>
      )}
      <details data-testid="agent-effective-settings" className="text-xs">
        <summary className="cursor-pointer text-muted-foreground hover:text-foreground">
          Effective settings each launch gets
        </summary>
        {current ? (
          <pre className="mt-1 max-h-60 overflow-auto rounded bg-muted p-2 font-mono text-[11px] text-foreground">
            {JSON.stringify(
              draft.runtime === "opencode"
                ? effectiveOpenCodeRules(current.content, draft.mcpBridgeTools)
                : current.content,
              null,
              2,
            )}
          </pre>
        ) : (
          <p className="mt-1 text-muted-foreground">Resolving…</p>
        )}
      </details>
    </section>
  );
}
