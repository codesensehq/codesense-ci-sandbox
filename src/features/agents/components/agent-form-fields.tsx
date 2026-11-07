// SPDX-License-Identifier: MIT
import type { AgentLaunchAdmission, AgentModelArguments } from "@/tauri/commands";
import { useId, useState } from "react";
import { AGENT_RUNTIMES, PREVIEW_RUNTIME_NOTE, registeredCli } from "@/shared/lib/agent-runtimes";
import { agentCliLabel } from "@/shared/lib/agent-clis";

import { useRoleConfigs } from "@/shared/hooks/use-role-configs";
import { QueueRole } from "@/shared/components/queue-role";
import { Lock } from "lucide-react";

import { Field } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { PortaledSelect } from "@/components/ui/portaled-select";
import { PreviewBadge, previewNoteRef } from "@/shared/components/preview-badge";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import { readModelArg, setModelArg } from "@/shared/lib/model-arg";
import { joinArgs, splitArgs } from "@/shared/lib/tokenize-args";

import type { AgentDraft } from "../lib/agent-form";
import { AutoApprovesBadge } from "./agent-bits";

export const FIELD = "bg-background text-foreground placeholder:text-muted-foreground";
/** The kit select's trigger, at the form fields' height and size. */
export const SELECT_TRIGGER = "h-8 rounded-lg border-input text-sm";
export const SELECT = cn(
  "h-8 rounded-lg border border-input bg-background px-2 text-sm text-foreground outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring disabled:opacity-60",
);

interface FieldsProps {
  draft: AgentDraft;
  onChange: (next: AgentDraft) => void;
}

/** The model an args line names, or "" (the CLI default). */
export function modelOfArgs(args: string): string {
  return readModelArg(splitArgs(args)) ?? "";
}

/** The args line without its model flag: everything else the agent passes. */
export function extraArgsOf(args: string): string {
  return joinArgs(setModelArg(splitArgs(args), null));
}

/** A copy of `args` with the value at `index` replaced. */
function withArgAt(args: readonly string[], index: number, value: string): string[] {
  const next = [...args];
  next[index] = value;
  return next;
}

/** Basics: its name, the role it joins, what it is for and the prompt it starts with. */
export function AgentBasicsFields({
  draft,
  onChange,
  teamKey,
  agentKey,
}: FieldsProps & { teamKey?: string; agentKey?: string }) {
  const { roles, teamKey: roleTeam } = useRoleConfigs(teamKey);
  const set = <K extends keyof AgentDraft>(field: K, value: AgentDraft[K]) =>
    onChange({ ...draft, [field]: value });
  return (
    <div className="grid gap-3">
      <div className="grid grid-cols-2 items-start gap-x-4 gap-y-3">
        <Field label="Name">
          <Input
            aria-label="Name"
            value={draft.name}
            placeholder="e.g. Slack triage"
            onChange={(e) => set("name", e.target.value)}
          />
        </Field>
        <Field label="Role when launched" hint="A launch can pick another role.">
          <div className="flex items-center gap-2">
            {draft.role && <QueueRole role={draft.role} teamKey={roleTeam} iconOnly />}
            <PortaledSelect
              ariaLabel="Default role"
              triggerClassName={SELECT_TRIGGER}
              value={draft.role ?? ""}
              onValueChange={(v) => set("role", v || null)}
              options={[
                { value: "", label: "No role (joins no queue)" },
                ...(draft.role && !roles.some((r) => r.id === draft.role)
                  ? [{ value: draft.role, label: `${draft.role} · not set up` }]
                  : []),
                ...roles.map((r) => ({
                  value: r.id,
                  label: `${r.name}${r.defaultHandler === agentKey ? " · default handler" : ""}`,
                })),
              ]}
            />
          </div>
        </Field>
      </div>
      <Field label="What it's for">
        <Textarea
          aria-label="Description"
          value={draft.description}
          placeholder="e.g. Sweeps the support channel and files what needs an owner."
          onChange={(e) => set("description", e.target.value)}
        />
      </Field>
      <Field label="Default prompt (when a launch brings none)">
        <Input
          aria-label="Default prompt"
          value={draft.defaultPrompt}
          placeholder="e.g. /triage-slack"
          onChange={(e) => set("defaultPrompt", e.target.value)}
        />
      </Field>
    </div>
  );
}

/** Runs on: the CLI, its model (read from and written to `--model`), the rest of the args,
 *  where a launch opens and whether it resumes after a terminal server update. */
export function AgentRunsOnFields({
  draft,
  onChange,
  escalating,
  saved,
  admission,
}: FieldsProps & {
  /** The draft as last saved: an edited model says what it was. */
  saved?: AgentDraft;
  admission?: AgentLaunchAdmission;
  /** The preview says a launch auto-approves tool use. Shown, never blocked. */
  escalating: boolean;
}) {
  const set = <K extends keyof AgentDraft>(field: K, value: AgentDraft[K]) =>
    onChange({ ...draft, [field]: value });
  const previewNoteId = useId();
  const endpoint =
    admission?.model_endpoint === true || registeredCli(draft.runtime)?.modelEndpoint === true;
  const [native, setNative] = useState<AgentModelArguments | undefined>(admission?.model_arguments);
  const [seenNative, setSeenNative] = useState("");
  const currentArgs = JSON.stringify(splitArgs(draft.args));
  const matches = (fact: AgentModelArguments | undefined) =>
    fact && JSON.stringify(fact.input_args) === currentArgs;
  const incoming = admission?.model_arguments;
  const incomingKey = JSON.stringify(incoming ?? null);
  if (endpoint && matches(incoming) && incomingKey !== seenNative) {
    setSeenNative(incomingKey);
    setNative(incoming);
  }
  const currentNative = matches(native) ? native : matches(incoming) ? incoming : undefined;
  const settingsModel = (draft.startupSettings?.model ?? {}) as {
    default?: string;
    provider?: string;
    base_url?: string;
  };
  const model = endpoint
    ? (currentNative?.model ?? settingsModel.default ?? admission?.model?.default ?? "")
    : (readModelArg(splitArgs(draft.args)) ?? "");
  const setEndpoint = (patch: Record<string, string>) =>
    onChange({
      ...draft,
      startupSettings: {
        ...draft.startupSettings,
        model: {
          ...admission?.model,
          ...settingsModel,
          provider: "custom",
          default: model,
          ...patch,
        },
      },
    });
  const savedModel = !endpoint && saved ? modelOfArgs(saved.args) : null;
  // The Extra args box holds what was typed; it follows the args only when something else (the
  // Model field, Discard, a reload) changed them.
  const [seen, setSeen] = useState(draft.args);
  const [extra, setExtra] = useState(() => (endpoint ? draft.args : extraArgsOf(draft.args)));
  if (seen !== draft.args) {
    setSeen(draft.args);
    setExtra(endpoint ? draft.args : extraArgsOf(draft.args));
  }
  return (
    <div className="grid gap-3">
      <span id={previewNoteId} hidden>
        {PREVIEW_RUNTIME_NOTE}
      </span>
      <div className="grid grid-cols-3 items-start gap-x-4 gap-y-3">
        <Field label="CLI">
          <PortaledSelect
            ariaLabel="Runtime"
            triggerClassName={SELECT_TRIGGER}
            value={draft.runtime}
            onValueChange={(v) => set("runtime", v as AgentDraft["runtime"])}
            options={AGENT_RUNTIMES.map((value) => ({
              value,
              label: agentCliLabel(value),
              adornment: <PreviewBadge cli={value} className="h-4 px-[5px] text-[10.5px]" />,
              describedBy: previewNoteRef(previewNoteId, value),
            }))}
          />
        </Field>
        <Field
          label={
            <>
              Model
              {savedModel !== null && savedModel !== model && (
                <span
                  data-testid="agent-model-was"
                  className="ml-auto text-xs font-normal text-muted-foreground"
                >
                  was {savedModel || "CLI default"}
                </span>
              )}
            </>
          }
          hint={
            endpoint
              ? "Updates existing model arguments, or the saved endpoint model when no model argument is set."
              : "Reads and writes --model. Empty leaves it to the CLI default."
          }
        >
          <Input
            aria-label="Model"
            disabled={endpoint && !currentNative}
            value={model}
            placeholder="CLI default"
            onChange={(e) => {
              if (endpoint) {
                if (!currentNative) return;
                const value = e.target.value;
                const edit = currentNative.edit;
                const args = edit
                  ? value
                    ? withArgAt(edit.args, edit.value_index, value)
                    : edit.clear_args
                  : splitArgs(draft.args);
                setNative({
                  input_args: args,
                  model: edit && value ? value : null,
                  edit: edit && value ? { ...edit, args } : null,
                });
                onChange({
                  ...draft,
                  args: edit ? joinArgs(args) : draft.args,
                  startupSettings: {
                    ...draft.startupSettings,
                    model: {
                      ...admission?.model,
                      ...settingsModel,
                      provider: "custom",
                      default: value,
                    },
                  },
                });
                return;
              }
              onChange({
                ...draft,
                args: joinArgs(setModelArg(splitArgs(draft.args), e.target.value)),
              });
            }}
          />
        </Field>
        <Field label="Opens in">
          <PortaledSelect
            ariaLabel="Launch"
            triggerClassName={SELECT_TRIGGER}
            value={draft.launch}
            onValueChange={(v) => set("launch", v as AgentDraft["launch"])}
            options={[
              { value: "foreground", label: "A terminal tab" },
              { value: "headless", label: "Headless" },
            ]}
          />
        </Field>
      </div>
      {endpoint && (
        <Field
          label="Local endpoint"
          hint="Keyless OpenAI-compatible endpoint on this Mac. API-key providers come later."
        >
          <Input
            aria-label="Local endpoint"
            placeholder="http://127.0.0.1:11434/v1"
            value={settingsModel.base_url ?? admission?.model?.base_url ?? ""}
            onChange={(event) => setEndpoint({ base_url: event.target.value })}
          />
        </Field>
      )}
      {admission?.allowed === false && (
        <p role="alert" data-testid="agent-launch-blocked" className="text-xs text-destructive">
          {admission.reason}
        </p>
      )}
      <Field
        label="Extra args"
        hint={endpoint ? "Arguments are passed through as typed." : "Everything except --model."}
      >
        <Input
          aria-label="Args"
          className="font-mono text-xs"
          value={extra}
          placeholder="e.g. --effort high"
          onChange={(e) => {
            if (endpoint) {
              setSeen(e.target.value);
              setExtra(e.target.value);
              set("args", e.target.value);
              return;
            }
            // The text stays as typed, spaces and unfinished quotes included; only the argv built
            // from it is normalized, with the Model field's value riding along.
            const typed = splitArgs(e.target.value);
            const next = joinArgs(
              readModelArg(typed) !== null ? typed : setModelArg(typed, model || null),
            );
            setSeen(next);
            setExtra(e.target.value);
            set("args", next);
          }}
        />
      </Field>
      {escalating && <AutoApprovesBadge />}
      <label className="flex cursor-pointer items-start gap-2 text-sm">
        <input
          type="checkbox"
          aria-label="Resume after a terminal server update"
          className="mt-1 size-3.5 accent-current"
          checked={draft.resumeAfterRestart}
          onChange={(e) => set("resumeAfterRestart", e.target.checked)}
        />
        <span className="flex flex-col gap-0.5">
          <span>Resume after a terminal server update</span>
          <span className="text-xs text-muted-foreground">
            Relaunched in its tab with a brief of where it was: its last progress, inbox, git state
            and terminal. Off, its tab reopens as a shell.
          </span>
        </span>
      </label>
    </div>
  );
}

/** Tools: the MCP servers the agent can use, from the team's config. */
export function AgentToolsFields({
  draft,
  onChange,
  offered,
}: FieldsProps & {
  /** The servers the agent's target team's MCP config offers, the bridge aside, as the preview
   *  resolved them; `null` until the first preview lands. */
  offered: readonly string[] | null;
}) {
  const set = <K extends keyof AgentDraft>(field: K, value: AgentDraft[K]) =>
    onChange({ ...draft, [field]: value });
  // The agent's own servers stay listed (and removable) even when the team config lacks them;
  // the preview says which are missing.
  const choices = [...(offered ?? []), ...draft.mcpServers.filter((id) => !offered?.includes(id))];
  const toggle = (id: string, on: boolean) =>
    set("mcpServers", on ? [...draft.mcpServers, id] : draft.mcpServers.filter((s) => s !== id));
  return (
    <fieldset aria-label="MCP servers" className="flex flex-col gap-1.5">
      <div className="flex flex-wrap items-center gap-2">
        <span
          data-testid="agent-server-bridge"
          title="CodeSense's own bridge is always there"
          className="inline-flex h-[26px] items-center gap-1.5 rounded-full bg-muted px-2.5 text-[13px] text-muted-foreground"
        >
          <Lock aria-hidden="true" className="size-3" />
          codesense (always on)
        </span>
        {choices.map((id) => {
          const on = draft.mcpServers.includes(id);
          return (
            <label
              key={id}
              className={cn(
                "inline-flex h-[26px] cursor-pointer items-center gap-1.5 rounded-full border px-2.5 text-[13px] has-focus-visible:ring-2 has-focus-visible:ring-ring",
                on
                  ? "border-foreground bg-background text-foreground"
                  : "border-border/70 text-muted-foreground hover:text-foreground",
              )}
            >
              <input
                type="checkbox"
                aria-label={`MCP server ${id}`}
                className="size-3 accent-current"
                checked={on}
                onChange={(e) => toggle(id, e.target.checked)}
              />
              {id}
            </label>
          );
        })}
        {choices.length === 0 && (
          <span className="text-xs text-muted-foreground">
            {offered === null
              ? "Reading the team's MCP config…"
              : "The team's MCP config lists no other servers."}
          </span>
        )}
      </div>
      {/* When the team offers servers, say so plainly, and tell the user why the CodeSense
       *  bridge is alone if nothing is checked (otherwise the launch is silently bridge-only).
       *  The "per-agent opt-in" model is documented at `setting-up-agents/mcp-access-per-agent.md`
       *  in the CodeSense docs. */}
      {offered && offered.length > 0 && (
        <p data-testid="agent-offered-servers-hint" className="text-xs text-muted-foreground">
          {draft.mcpServers.length === 0
            ? `Nothing checked, so this agent reaches only the CodeSense bridge. Check the servers it needs — the team offers: ${offered.join(", ")}.`
            : `The team offers: ${offered.join(", ")}.`}{" "}
          See <em>Granting MCP servers per agent</em> in the CodeSense docs.
        </p>
      )}
    </fieldset>
  );
}
