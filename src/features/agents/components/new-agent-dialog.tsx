// SPDX-License-Identifier: MIT
import { useId, useMemo, useState, type FormEvent } from "react";
import { ChevronRight, Loader2, Plus } from "lucide-react";

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { InlineAlert } from "@/components/ui/inline-alert";
import { Input } from "@/components/ui/input";
import { Segmented } from "@/components/ui/segmented";
import { cn } from "@/lib/utils";
import { AppButton as Button } from "@/shared/components/app-button";
import type { AgentRow, AgentRuntime } from "@/tauri/commands";
import { AGENT_RUNTIMES } from "@/shared/lib/agent-runtimes";
import { PreviewBadge, PreviewNote, previewNoteRef } from "@/shared/components/preview-badge";
import { useOrgStore } from "@/shared/stores/org-store";

import { isBuiltin, runtimeLabel } from "../lib/agent-display";
import { useAgentsStore } from "../stores/agents-store";
import { RuntimeBadge } from "./agent-bits";
import { MiddleTruncate } from "@/features/viewer/components/middle-truncate";

/** The blank row's value in the template list. */
const BLANK = "blank";

/**
 * The New agent dialog: who can use it (Personal or the active team), then a template to
 * start from, or a blank agent. Picking a row chooses it; the footer's button makes it.
 *
 * Templates are the built-in starters CodeSense ships — read-only patterns a new agent can be
 * based on. Using one clones it into the chosen scope as an editable agent: Personal lands in
 * your scratch space, Team in the active team's shared agents dir so teammates can use (and
 * edit) it. Team is off when no team is active. Blank goes through the inline create form,
 * which has its own scope control. "Describe it to an agent" hands the job to an authoring
 * agent instead.
 */
export function NewAgentDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const rows = useAgentsStore((s) => s.rows);
  const copyToMine = useAgentsStore((s) => s.copyToMine);
  const startCreate = useAgentsStore((s) => s.startCreate);
  const describeToAgent = useAgentsStore((s) => s.describeToAgent);
  const pending = useAgentsStore((s) => s.pending);
  const activeOrg = useOrgStore((s) => s.activeOrgSlug);
  const activeTeam = useOrgStore((s) => s.activeTeamName);
  const hasActiveTeam = Boolean(activeOrg && activeTeam);
  const [scope, setScope] = useState<"personal" | "team">("personal");
  const [chosen, setChosen] = useState<string | null>(null);
  const [describeError, setDescribeError] = useState<string | null>(null);

  // Templates are the builtins the service already returns. Order: ordinary runtimes first
  // (claude, codex, opencode), then their unattended twins.
  const templates = useMemo(() => orderTemplates((rows ?? []).filter(isBuiltin)), [rows]);
  const selected = chosen ?? templates[0]?.key ?? BLANK;
  const selectedTemplate = templates.find((t) => t.key === selected);
  const busy = selectedTemplate ? pending[`copy:${selectedTemplate.key}`] === true : false;

  const confirm = async () => {
    if (selected === BLANK || !selectedTemplate) {
      startCreate();
      onClose();
      return;
    }
    const target =
      scope === "team" && hasActiveTeam
        ? { org: activeOrg as string, team: activeTeam as string }
        : undefined;
    await copyToMine(selectedTemplate.key, target);
    onClose();
  };

  return (
    <Dialog open={open} onOpenChange={(next) => (!next ? onClose() : undefined)}>
      <DialogContent data-testid="new-agent-dialog" className="gap-0 p-0 sm:max-w-[640px]">
        <DialogHeader className="px-5 pt-[18px] pb-3">
          <DialogTitle className="text-lg font-semibold tracking-tight">New agent</DialogTitle>
          <DialogDescription>
            Start from a template, or begin blank. You can change everything afterwards.
          </DialogDescription>
        </DialogHeader>
        <div className="grid gap-3.5 px-5 pb-3">
          <ScopePicker
            scope={scope}
            onScope={setScope}
            hasActiveTeam={hasActiveTeam}
            activeTeam={activeTeam ?? null}
          />
          <div role="listbox" aria-label="Templates" className="grid gap-0.5">
            {templates.map((t) => (
              <TemplateRow
                key={t.key}
                template={t}
                selected={selected === t.key}
                onSelect={() => setChosen(t.key)}
                onUse={() => void confirm()}
              />
            ))}
            <BlankRow
              selected={selected === BLANK}
              onSelect={() => setChosen(BLANK)}
              onUse={() => void confirm()}
            />
          </div>
        </div>
        {describeError && (
          <InlineAlert severity="error" className="mx-5 mb-3">
            {describeError}
          </InlineAlert>
        )}
        <DialogFooter className="mx-0 mb-0 items-center border-t border-border/70 bg-muted/35 px-5 py-3 sm:justify-between">
          <span className="text-xs text-muted-foreground">
            Or{" "}
            <button
              type="button"
              className="underline underline-offset-2 hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
              onClick={() =>
                void describeToAgent().then((result) => {
                  if (result.opened) onClose();
                  else setDescribeError(result.message ?? "Couldn't open an authoring agent.");
                })
              }
            >
              describe it to an agent
            </button>
            .
          </span>
          <div className="flex gap-2">
            <Button type="button" variant="ghost" onClick={onClose}>
              Cancel
            </Button>
            <Button type="button" disabled={busy} onClick={() => void confirm()}>
              {busy && <Loader2 className="animate-spin" />}
              {selected === BLANK ? "Use blank agent" : `Use ${selectedTemplate?.name ?? ""}`}
            </Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** What "Save as agent" keeps of a running session (SW-AGR-6 "Keep this…"). */
export interface SessionToSave {
  runtime: AgentRuntime;
  /** The session's working directory as shown (`~/repos/codesense`), or null. */
  cwd: string | null;
  /** "This Mac" or the machine's label. */
  machineLabel: string;
}

/**
 * Save as agent: an unnamed session (a starter, or a CLI started by hand)
 * becomes a saved agent, from its runtime, so a timer on it can become that
 * agent's routine. It creates something durable and needs a name, so it is a
 * modal — the only one in the Keep this… flow. `onSaved` gets the new agent's
 * key and where it was saved.
 */
export function SaveSessionAsAgentDialog({
  open,
  session,
  onClose,
  onSaved,
}: {
  open: boolean;
  session: SessionToSave;
  onClose: () => void;
  onSaved: (key: string, scope: "personal" | "team") => void;
}) {
  const saveAgent = useAgentsStore((s) => s.saveAgent);
  const activeOrg = useOrgStore((s) => s.activeOrgSlug);
  const activeTeam = useOrgStore((s) => s.activeTeamName);
  const hasActiveTeam = Boolean(activeOrg && activeTeam);
  const [scope, setScope] = useState<"personal" | "team">("personal");
  const [name, setName] = useState(
    () => `${runtimeLabel(session.runtime)} on ${session.machineLabel}`,
  );
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  const save = async (e?: FormEvent) => {
    e?.preventDefault();
    if (saving) return;
    if (!name.trim()) {
      setError("Name the agent.");
      return;
    }
    setSaving(true);
    setError(null);
    const team = scope === "team" && hasActiveTeam;
    try {
      const reply = await saveAgent({
        scope: team ? "team" : "personal",
        ...(team ? { org: activeOrg as string, team: activeTeam as string } : {}),
        name: name.trim(),
        description: session.cwd
          ? `Saved from a ${runtimeLabel(session.runtime)} session in ${session.cwd} on ${session.machineLabel}.`
          : `Saved from a ${runtimeLabel(session.runtime)} session on ${session.machineLabel}.`,
        runtime: session.runtime,
        args: [],
        launch: "foreground",
        mcpServers: [],
        startup: [],
        permissions: null,
        defaultPrompt: "",
      });
      if (reply.outcome === "saved") {
        onSaved(reply.key, team ? "team" : "personal");
        return;
      }
      setError(reply.outcome === "invalid" ? reply.message : "Couldn't save the agent. Try again.");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={(next) => (!next ? onClose() : undefined)}>
      <DialogContent data-testid="save-session-as-agent" className="max-w-md">
        <form
          onSubmit={(e) => void save(e)}
          className="flex min-w-0 flex-col gap-4 [overflow-wrap:anywhere]"
        >
          <DialogHeader>
            <DialogTitle>Save as agent</DialogTitle>
            <DialogDescription>
              A timer lives only as long as its session. Save this session as an agent, then make
              the timer its routine.
            </DialogDescription>
          </DialogHeader>
          <label className="flex flex-col gap-1.5 text-sm font-medium">
            Name
            <Input
              aria-label="Name"
              value={name}
              autoFocus
              onChange={(e) => setName(e.target.value)}
              aria-invalid={error === "Name the agent." || undefined}
            />
          </label>
          {/* Agents keep no directory: this is where the session ran, for
              reference. A long path is cut in the middle (the full one is its
              title), so the dialog never overflows. */}
          <p
            className="flex min-w-0 flex-wrap gap-y-0.5 text-xs text-muted-foreground"
            data-testid="save-session-as-agent-from"
            aria-label={[
              `Runs ${runtimeLabel(session.runtime)}`,
              session.cwd ? `was running in ${session.cwd}` : null,
              `on ${session.machineLabel}`,
            ]
              .filter(Boolean)
              .join(" · ")}
          >
            <span className="basis-full">
              Runs {runtimeLabel(session.runtime)} · on {session.machineLabel}
            </span>
            {session.cwd && (
              <span className="flex min-w-0 basis-full items-baseline gap-1">
                <span className="shrink-0">was running in</span>
                <MiddleTruncate
                  text={session.cwd}
                  title={session.cwd}
                  className="min-w-0 font-mono text-[11px]"
                  data-testid="save-session-as-agent-cwd"
                />
              </span>
            )}
          </p>
          <ScopePicker
            scope={scope}
            onScope={setScope}
            hasActiveTeam={hasActiveTeam}
            activeTeam={activeTeam ?? null}
          />
          {error && (
            <p role="alert" className="text-xs text-destructive">
              {error}
            </p>
          )}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose} disabled={saving}>
              Cancel
            </Button>
            <Button type="submit" disabled={saving}>
              {saving && <Loader2 className="animate-spin" />}
              Save agent and continue
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function ScopePicker({
  scope,
  onScope,
  hasActiveTeam,
  activeTeam,
}: {
  scope: "personal" | "team";
  onScope: (next: "personal" | "team") => void;
  hasActiveTeam: boolean;
  activeTeam: string | null;
}) {
  // The team option disables itself (with a reason) when no team is active rather than
  // hiding: the user sees what they would get.
  return (
    <div className="flex flex-wrap items-center gap-3">
      <span className="text-[13px] text-muted-foreground">Who can use it</span>
      <Segmented
        aria-label="Where to save the new agent"
        value={scope}
        onValueChange={onScope}
        options={[
          { value: "personal", label: "Personal", testId: "new-agent-scope-personal" },
          {
            value: "team",
            label: hasActiveTeam ? `Team · ${activeTeam}` : "Team",
            disabled: !hasActiveTeam,
            testId: "new-agent-scope-team",
          },
        ]}
      />
      <span className="text-xs text-muted-foreground">
        {scope === "team"
          ? "Teammates can use and edit it."
          : hasActiveTeam
            ? "Only you can see or edit it."
            : "Only you can see or edit it. Select a team first to share with teammates."}
      </span>
    </div>
  );
}

const ROW =
  "grid cursor-pointer grid-cols-[84px_minmax(0,1fr)_auto] items-center gap-3 rounded-lg border px-3 py-2 text-left outline-none focus-visible:ring-2 focus-visible:ring-ring";

function TemplateRow({
  template,
  selected,
  onSelect,
  onUse,
}: {
  template: AgentRow;
  selected: boolean;
  onSelect: () => void;
  onUse: () => void;
}) {
  const noteId = useId();
  return (
    <div
      role="option"
      tabIndex={0}
      aria-selected={selected}
      aria-describedby={previewNoteRef(noteId, template.runtime)}
      data-testid="new-agent-template"
      data-template-key={template.key}
      onClick={onSelect}
      onDoubleClick={onUse}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onSelect();
        }
      }}
      className={cn(
        ROW,
        "min-h-14",
        selected ? "border-border/70 bg-muted" : "border-transparent hover:bg-muted/55",
      )}
    >
      <RuntimeBadge runtime={template.runtime} />
      <span className="min-w-0">
        <span className="flex flex-wrap items-center gap-2 text-sm font-medium">
          {template.name}
          <PreviewBadge cli={template.runtime} />
          <PreviewNote id={noteId} cli={template.runtime} />
        </span>
        {template.description && (
          <span className="block text-xs leading-snug text-muted-foreground">
            {template.description}
          </span>
        )}
      </span>
      <ChevronRight
        aria-hidden="true"
        className={cn("size-3.5 text-muted-foreground", selected ? "opacity-100" : "opacity-0")}
      />
    </div>
  );
}

function BlankRow({
  selected,
  onSelect,
  onUse,
}: {
  selected: boolean;
  onSelect: () => void;
  onUse: () => void;
}) {
  return (
    <div
      role="option"
      tabIndex={0}
      aria-selected={selected}
      data-testid="new-agent-blank"
      onClick={onSelect}
      onDoubleClick={onUse}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onSelect();
        }
      }}
      className={cn(
        ROW,
        "mt-2 min-h-14 border-dashed border-input",
        selected && "border-solid border-border/70 bg-muted",
      )}
    >
      <span className="grid size-7 place-items-center rounded-lg bg-muted text-muted-foreground">
        <Plus aria-hidden="true" className="size-4" />
      </span>
      <span className="min-w-0">
        <span className="block text-sm font-medium">Blank agent</span>
        <span className="block text-xs leading-snug text-muted-foreground">
          Pick the CLI, tools and permissions yourself.
        </span>
      </span>
      <ChevronRight
        aria-hidden="true"
        className={cn("size-3.5 text-muted-foreground", selected ? "opacity-100" : "opacity-0")}
      />
    </div>
  );
}

/** Templates in launch-friendly order: `claude`, `codex`, `opencode`, then unattended twins. */
function orderTemplates(rows: readonly AgentRow[]): AgentRow[] {
  const rank = (row: AgentRow): number => {
    const runtimeRank = AGENT_RUNTIMES.findIndex((runtime) => runtime === row.runtime);
    return runtimeRank + (row.key.endsWith("-unattended") ? AGENT_RUNTIMES.length : 0);
  };
  return [...rows].sort((a, b) => {
    const r = rank(a) - rank(b);
    return r !== 0 ? r : a.name.localeCompare(b.name);
  });
}

/** Wire-friendly accessor so tests and consumers don't duplicate the ordering rule. */
export function templateRows(rows: readonly AgentRow[]): AgentRow[] {
  return orderTemplates(rows.filter(isBuiltin));
}
