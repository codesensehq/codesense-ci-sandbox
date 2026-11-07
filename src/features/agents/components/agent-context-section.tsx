// SPDX-License-Identifier: MIT
import { commitTextEditOnBlur } from "@/shared/lib/text-menu-focus";
import { useEffect, useId, useMemo, useState } from "react";
import {
  ArrowRight,
  CircleAlert,
  FileText,
  FolderOpen,
  Info,
  Lightbulb,
  Loader2,
  Minus,
  Plus,
  RotateCcw,
  TriangleAlert,
  Undo2,
  X,
} from "lucide-react";

import { Input } from "@/components/ui/input";
import { AppButton as Button } from "@/shared/components/app-button";
import { PortaledSelect } from "@/components/ui/portaled-select";
import { cn } from "@/lib/utils";
import {
  contextSummary,
  formatContextBytes,
  repoRequestLabel,
} from "@/features/agent-context/lib/agent-context-model";
import { useOrgStore } from "@/shared/stores/org-store";
import {
  agentStartupCatalog,
  type AgentContextEntry,
  type AgentContextView,
  type AgentRuntime,
  type AgentStartupCatalog,
  type AgentTeamRef,
} from "@/tauri/commands";

import { MAX_CONTEXT_ENTRIES, checkContextDest, checkContextPath } from "../lib/agent-form";
import { useAgentContextEditor, type ContextEditProblem } from "../lib/use-agent-context-editor";
import { FIELD } from "./agent-form-fields";

/** The runtime's name as the product says it. */
const RUNTIME_NAME: Record<AgentRuntime, string> = {
  claude: "Claude",
  codex: "Codex",
  opencode: "OpenCode",
  hermes: "Hermes",
};

/** The "Preview in" value for the team workspace. */
const WORKSPACE = "";

/** Every icon button: a 24px hit target with the app's focus ring. */
const ICON_BUTTON =
  "inline-flex size-6 shrink-0 items-center justify-center rounded text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-40";

type ViewEntry = AgentContextView["entries"][number];
type ViewWarning = AgentContextView["warnings"][number];

function plural(n: number, one: string, many = `${one}s`): string {
  return `${n.toLocaleString("en-US")} ${n === 1 ? one : many}`;
}

/** A repo by the name people call it. */
function repoName(label: string): string {
  return label.split("/").pop() ?? label;
}

/** Is `path` the entry `entry` or inside it? */
function under(path: string, entry: string): boolean {
  return path === entry || path.startsWith(`${entry}/`);
}

/** The warnings that belong to a row: its own path, or a path inside it. */
function warningsFor(warnings: readonly ViewWarning[], path: string): ViewWarning[] {
  return warnings.filter((w) => w.path !== undefined && under(w.path, path));
}

/** A server reason as the editor shows it: it already is the agent editor. */
function inEditor(reason: string): string {
  return reason.replace(/ in the agent editor(?=[.;]|$)/g, " here");
}

/** A warning in the words the editor uses. */
function warningText(w: ViewWarning): string {
  if (w.code === "missing") return `${w.path ?? "This"}: not in team content yet`;
  return w.message;
}

/** The team's source repos (`host/owner/repo`), as the org lists them. */
function useTeamRepos(team: AgentTeamRef | undefined): string[] {
  const orgs = useOrgStore((s) => s.orgs);
  return useMemo(() => {
    if (!team) return [];
    const found = orgs
      .find((o) => o.slug === team.org)
      ?.teams.find((t) => t.name === team.team)?.source_repos;
    const seen = new Set<string>();
    return (found ?? [])
      .map((slug) => repoRequestLabel(slug))
      .filter((label) => !seen.has(label.toLowerCase()) && seen.add(label.toLowerCase()));
  }, [orgs, team]);
}

/** How long typing settles before the content is listed for suggestions. */
const SUGGEST_DEBOUNCE_MS = 150;
const SUGGEST_MAX = 8;

/**
 * A path field that suggests team content as you type: the folder before the last `/` is
 * listed one level at a time (as the startup picker lists it), filtered by what follows.
 * Arrow keys move through the suggestions, Enter or Tab takes one, Escape closes them; Enter
 * with none highlighted submits.
 */
function ContentPathInput({
  label,
  value,
  onChange,
  onSubmit,
  team,
  placeholder,
  disabled,
  errorId,
}: {
  label: string;
  value: string;
  onChange: (next: string) => void;
  onSubmit: () => void;
  team: AgentTeamRef | undefined;
  placeholder: string;
  disabled?: boolean;
  /** The id of the error this field is refused with, while it is. */
  errorId?: string;
}) {
  const listId = useId();
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(-1);
  const [options, setOptions] = useState<AgentStartupCatalog["entries"]>([]);
  const typed = value.replace(/^\.\//, "");
  const cut = typed.lastIndexOf("/");
  const dir = cut === -1 ? "" : typed.slice(0, cut);
  const filter = cut === -1 ? typed : typed.slice(cut + 1);
  const org = team?.org;
  const teamName = team?.team;
  useEffect(() => {
    if (!open || !org || !teamName) return;
    let live = true;
    const timer = setTimeout(() => {
      agentStartupCatalog({ org, team: teamName }, dir || undefined, {
        filter: filter || undefined,
      }).then(
        (catalog) => {
          if (!live) return;
          setOptions(catalog.entries.slice(0, SUGGEST_MAX));
          setActive(-1);
        },
        () => live && setOptions([]),
      );
    }, SUGGEST_DEBOUNCE_MS);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [open, org, teamName, dir, filter]);
  // Suggestions follow typing: an emptied field (after an add) shows none.
  const shown = open && options.length > 0 && value.trim() !== "";
  const take = (entry: AgentStartupCatalog["entries"][number]) => {
    onChange(entry.is_dir ? `${entry.path}/` : entry.path);
    setActive(-1);
  };
  return (
    <div className="relative min-w-0 flex-1">
      <Input
        role="combobox"
        aria-label={label}
        aria-expanded={shown}
        aria-controls={shown ? listId : undefined}
        aria-autocomplete="list"
        aria-invalid={errorId ? true : undefined}
        aria-describedby={errorId}
        aria-activedescendant={shown && active >= 0 ? `${listId}-${active}` : undefined}
        className={cn("h-8 font-mono text-xs", FIELD)}
        value={value}
        placeholder={placeholder}
        disabled={disabled}
        autoComplete="off"
        spellCheck={false}
        onFocus={() => setOpen(true)}
        onBlur={(event) => commitTextEditOnBlur(event.currentTarget, () => setOpen(false))}
        onChange={(e) => {
          onChange(e.target.value);
          setOpen(true);
        }}
        onKeyDown={(e) => {
          if (shown && e.key === "ArrowDown") {
            e.preventDefault();
            setActive((i) => (i + 1) % options.length);
          } else if (shown && e.key === "ArrowUp") {
            e.preventDefault();
            setActive((i) => (i <= 0 ? options.length - 1 : i - 1));
          } else if (e.key === "Escape" && shown) {
            e.preventDefault();
            setOpen(false);
          } else if ((e.key === "Enter" || e.key === "Tab") && shown && active >= 0) {
            e.preventDefault();
            take(options[active]);
          } else if (e.key === "Enter") {
            e.preventDefault();
            setOpen(false);
            onSubmit();
          }
        }}
      />
      {shown && (
        <ul
          id={listId}
          role="listbox"
          aria-label={`${label} suggestions`}
          className="absolute inset-x-0 top-full z-20 mt-1 max-h-56 overflow-y-auto rounded-md border border-border bg-popover p-1 text-popover-foreground shadow-md"
        >
          {options.map((entry, i) => (
            <li
              key={entry.path}
              id={`${listId}-${i}`}
              role="option"
              aria-selected={i === active}
              className={cn(
                "flex cursor-pointer items-center gap-1.5 rounded px-1.5 py-1 font-mono text-xs",
                i === active ? "bg-accent text-accent-foreground" : "hover:bg-muted",
              )}
              // Keep focus in the field: a click picks before the blur closes the list.
              onMouseDown={(e) => {
                e.preventDefault();
                take(entry);
              }}
            >
              {entry.is_dir ? (
                <FolderOpen
                  aria-hidden="true"
                  className="size-3.5 shrink-0 text-muted-foreground"
                />
              ) : (
                <FileText aria-hidden="true" className="size-3.5 shrink-0 text-muted-foreground" />
              )}
              <span className="truncate">{entry.is_dir ? `${entry.path}/` : entry.path}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function WarningLine({ warning }: { warning: ViewWarning }) {
  return (
    <span
      data-testid="agent-context-warning"
      data-code={warning.code}
      title={warning.message}
      className="flex items-start gap-1 text-xs text-warning-ink"
    >
      <TriangleAlert aria-hidden="true" className="mt-0.5 size-3 shrink-0" />
      {warningText(warning)}
    </span>
  );
}

/** `→ dest`, read as "delivered as dest". */
function DestTag({ dest }: { dest: string }) {
  return (
    <span className="flex min-w-0 shrink items-center gap-1 text-xs text-muted-foreground">
      <ArrowRight role="img" aria-label="delivered as" className="size-3 shrink-0" />
      <span className="truncate font-mono">{dest}</span>
    </span>
  );
}

/** One `context` entry (or one the runtime default holds): where it lands, what it resolved
 *  to here, and why it brings less. */
function EntryRow({
  entry,
  counts,
  isFile,
  warnings,
  onRemove,
  disabled,
  quiet,
}: {
  entry: AgentContextEntry;
  /** What it resolved to where the section previews; absent when it is not delivered there. */
  counts: Pick<ViewEntry, "files" | "bytes"> | undefined;
  /** It names one file (else a folder, or nothing yet). */
  isFile: boolean;
  warnings: ViewWarning[];
  onRemove?: () => void;
  disabled: boolean;
  /** A runtime-default path: one the team lacks is ordinary, so it reads "nothing here yet"
   *  in muted ink, never as a warning (while defaulted and after Customize alike). */
  quiet: boolean;
}) {
  const shown = quiet ? warnings.filter((w) => w.code !== "missing") : warnings;
  const empty = quiet && counts !== undefined && counts.files === 0;
  // An amber mark only where a warning says why.
  const flagged = shown.length > 0;
  return (
    <li
      data-testid="agent-context-entry"
      data-path={entry.path}
      className="flex flex-col gap-0.5 py-0.5"
    >
      <div className="flex min-h-7 min-w-0 items-center gap-1.5 text-sm">
        {flagged ? (
          <CircleAlert aria-hidden="true" className="size-3.5 shrink-0 text-warning-ink" />
        ) : isFile ? (
          <FileText aria-hidden="true" className="size-3.5 shrink-0 text-muted-foreground" />
        ) : (
          <FolderOpen
            aria-hidden="true"
            className={cn(
              "size-3.5 shrink-0",
              counts && counts.files > 0 ? "text-muted-foreground" : "text-muted-foreground",
            )}
          />
        )}
        <span className="min-w-0 truncate font-mono text-xs">{entry.path}</span>
        {entry.dest && <DestTag dest={entry.dest} />}
        <span className="ml-auto shrink-0 pl-2 text-xs tabular-nums text-muted-foreground">
          {empty
            ? "nothing here yet"
            : counts
              ? `${plural(counts.files, "file")} · ${formatContextBytes(counts.bytes)}`
              : ""}
        </span>
        {onRemove ? (
          <button
            type="button"
            aria-label={`Remove ${entry.path} from context`}
            disabled={disabled}
            className={ICON_BUTTON}
            onClick={onRemove}
          >
            <X aria-hidden="true" className="size-3.5" />
          </button>
        ) : (
          // Keeps the counts in one column whether or not a row can be removed.
          <span aria-hidden="true" className="size-6 shrink-0" />
        )}
      </div>
      {shown.map((w) => (
        <span key={`${w.code}:${w.message}`} className="pl-5">
          <WarningLine warning={w} />
        </span>
      ))}
    </li>
  );
}

function ProblemLine({
  problem,
  name,
  onReload,
}: {
  problem: ContextEditProblem;
  name: string;
  onReload: () => void;
}) {
  if (problem.kind === "conflict") {
    return (
      <p
        data-testid="agent-context-edit-error"
        role="alert"
        className="flex flex-wrap items-center gap-2 text-xs text-warning-ink"
      >
        <TriangleAlert aria-hidden="true" className="size-3 shrink-0" />
        {name}&apos;s context changed elsewhere (now revision {problem.currentRevision}), so nothing
        was saved.
        <Button size="sm" variant="outline" onClick={onReload}>
          <RotateCcw />
          Reload
        </Button>
      </p>
    );
  }
  const text =
    problem.kind === "blocked"
      ? inEditor(problem.reason)
      : problem.kind === "invalid"
        ? `Not saved: ${problem.message}`
        : `Couldn't change ${name}'s context: ${problem.message}`;
  return (
    <p
      data-testid="agent-context-edit-error"
      role={problem.kind === "blocked" ? "status" : "alert"}
      className={cn(
        "flex items-start gap-1 text-xs",
        problem.kind === "blocked" ? "text-muted-foreground" : "text-destructive",
      )}
    >
      <CircleAlert aria-hidden="true" className="mt-0.5 size-3 shrink-0" />
      {text}
    </p>
  );
}

type RepoEdit =
  | { op: "add"; path: string }
  | { op: "remove"; path: string }
  | { op: "drop_add"; path: string }
  | { op: "drop_remove"; path: string };

/** A repo's override: what it adds (with what each resolves to while that repo is previewed),
 *  what it leaves out, and (for the repo previewed) a field to change it. */
function RepoOverrideBlock({
  repo,
  add,
  remove,
  addCounts,
  editable,
  team,
  disabled,
  onEdit,
  onDirty,
}: {
  repo: string;
  add: AgentContextEntry[];
  remove: string[];
  /** What each add resolved to, when this repo is the one previewed. */
  addCounts: Map<string, Pick<ViewEntry, "files" | "bytes">> | null;
  editable: boolean;
  team: AgentTeamRef | undefined;
  disabled: boolean;
  onEdit: (edit: RepoEdit) => Promise<boolean>;
  onDirty: (dirty: boolean) => void;
}) {
  const name = repoName(repo);
  const errorId = useId();
  const [typed, setTyped] = useState("");
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (editable) onDirty(typed.trim() !== "");
  }, [editable, typed, onDirty]);
  const submit = async (op: "add" | "remove") => {
    const checked = checkContextPath(typed);
    if ("error" in checked) {
      setError(checked.error);
      return;
    }
    setError(null);
    if (await onEdit({ op, path: checked.path })) setTyped("");
  };
  return (
    <li data-testid="agent-context-repo" data-repo={repo} className="flex flex-col gap-1 py-1.5">
      <div className="flex items-baseline gap-2 text-sm">
        <span className="font-medium">{name}</span>
        <span className="truncate text-xs text-muted-foreground">{repo}</span>
      </div>
      {add.length === 0 && remove.length === 0 && (
        <p className="text-xs text-muted-foreground">Gets the same context as everywhere else.</p>
      )}
      {(add.length > 0 || remove.length > 0) && (
        <ul className="flex flex-col">
          {add.map((e) => {
            const counts = addCounts?.get(e.path);
            return (
              <li
                key={`add:${e.path}`}
                data-testid="agent-context-repo-add"
                className="flex min-h-7 min-w-0 items-center gap-1.5 text-xs"
              >
                <Plus
                  role="img"
                  aria-label="adds"
                  className="size-3 shrink-0 text-[var(--cs-status-done)]"
                />
                <span className="truncate font-mono">{e.path}</span>
                {e.dest && <DestTag dest={e.dest} />}
                <span className="ml-auto shrink-0 pl-2 tabular-nums text-muted-foreground">
                  {counts
                    ? `${plural(counts.files, "file")} · ${formatContextBytes(counts.bytes)}`
                    : ""}
                </span>
                <button
                  type="button"
                  aria-label={`Stop adding ${e.path} in ${name}`}
                  title="Stop adding it here"
                  disabled={disabled}
                  className={ICON_BUTTON}
                  onClick={() => void onEdit({ op: "drop_add", path: e.path })}
                >
                  <X aria-hidden="true" className="size-3.5" />
                </button>
              </li>
            );
          })}
          {remove.map((pattern) => (
            <li
              key={`remove:${pattern}`}
              className="flex min-h-7 min-w-0 items-center gap-1.5 text-xs"
            >
              <Minus aria-hidden="true" className="size-3 shrink-0 text-muted-foreground" />
              <span className="truncate font-mono">{pattern}</span>
              <span className="shrink-0 rounded bg-muted px-1 text-[11px] text-muted-foreground">
                left out
              </span>
              <button
                type="button"
                aria-label={`Stop leaving out ${pattern} in ${name}`}
                title="Stop leaving it out"
                disabled={disabled}
                className={cn(ICON_BUTTON, "ml-auto")}
                onClick={() => void onEdit({ op: "drop_remove", path: pattern })}
              >
                <Undo2 aria-hidden="true" className="size-3.5" />
              </button>
            </li>
          ))}
        </ul>
      )}
      {editable && (
        <div className="flex flex-col gap-0.5">
          <div className="flex flex-wrap items-center gap-1.5">
            <ContentPathInput
              label={`Path for ${name}`}
              value={typed}
              onChange={(v) => {
                setTyped(v);
                setError(null);
              }}
              onSubmit={() => void submit("add")}
              team={team}
              placeholder={`a path, only in ${name}`}
              disabled={disabled}
              errorId={error ? errorId : undefined}
            />
            <Button
              size="sm"
              variant="outline"
              aria-label={`Add for ${name}`}
              disabled={disabled || !typed.trim()}
              onClick={() => void submit("add")}
            >
              <Plus />
              Add
            </Button>
            <Button
              size="sm"
              variant="ghost"
              aria-label={`Leave out in ${name}`}
              disabled={disabled || !typed.trim()}
              onClick={() => void submit("remove")}
            >
              <Minus />
              Leave out
            </Button>
          </div>
          {error && (
            <span id={errorId} className="text-xs text-destructive">
              {error}
            </span>
          )}
        </div>
      )}
    </li>
  );
}

/**
 * SW-XS-16 — what the agent gets besides what it starts with: its `context` entries (or the
 * runtime default, exactly what "Customize" writes out), each with what it resolves to and why
 * it brings less (missing, excluded, large: warnings, never a refusal); its per-repo
 * overrides; the team's retired `projections.yml` as a one-time suggestion; and a live footer
 * for the repo previewed. Every change is saved at once, as one operation against the revision
 * shown (`agent_context_update`), separately from the form's own Save.
 */
export function AgentContextSection({
  agentKey,
  name,
  runtime,
  team,
  revision,
  onSaved,
  onDirtyChange,
}: {
  agentKey: string;
  name: string;
  runtime: AgentRuntime;
  /** The team whose content and repos the agent resolves against. */
  team: AgentTeamRef | undefined;
  /** The agent's revision as the editor last read it. */
  revision: number;
  /** A context edit saved `to` on top of `from`. */
  onSaved: (from: number, to: number) => void;
  /** A path typed here and not yet added, so leaving asks first. */
  onDirtyChange?: (dirty: boolean) => void;
}) {
  const repos = useTeamRepos(team);
  const [picked, setPicked] = useState<string | null>(null);
  // The first repo until one is picked: a repo launch is what most agents run as.
  const repo = picked === null ? (repos[0] ?? null) : picked === WORKSPACE ? null : picked;
  const ctx = useAgentContextEditor(agentKey, repo, revision, onSaved);
  const { view, problem, saving } = ctx;
  const shownName = view?.name ?? name;
  const runtimeName = RUNTIME_NAME[runtime] ?? runtime;
  const pathErrorId = useId();
  const [typed, setTyped] = useState("");
  const [typedDest, setTypedDest] = useState("");
  const [inputError, setInputError] = useState<string | null>(null);
  const [removed, setRemoved] = useState<AgentContextEntry | null>(null);
  const [repoDirty, setRepoDirty] = useState(false);
  const dirty = typed.trim() !== "" || typedDest.trim() !== "" || repoDirty;
  useEffect(() => onDirtyChange?.(dirty), [dirty, onDirtyChange]);
  useEffect(() => () => onDirtyChange?.(false), [onDirtyChange]);

  const defaulted = view ? view.context === null : false;
  // While defaulted the rows are the runtime default: exactly what Customize writes out.
  const rows: AgentContextEntry[] = view ? (view.context ?? view.runtime_default) : [];
  const resolved = (view?.entries ?? []).filter(
    (e) => e.source === "context" || e.source === "default",
  );
  const countsFor = (e: AgentContextEntry) => resolved.find((r) => r.path === e.path);
  // In the team workspace a defaulted agent gets the team-workspace default instead.
  const workspaceDefault =
    defaulted && !repo ? resolved.filter((e) => e.source === "default").map((e) => e.path) : [];
  const full = rows.length >= MAX_CONTEXT_ENTRIES;
  const sizeWarnings = (view?.warnings ?? []).filter((w) => w.path === undefined);
  const repoAdds = new Map(
    (view?.entries ?? []).filter((e) => e.source === "repo_add").map((e) => [e.path, e] as const),
  );
  // Warnings no row owns (a startup path's) still show, once.
  const placed = (w: ViewWarning) =>
    w.path !== undefined &&
    (rows.some((e) => under(w.path!, e.path)) ||
      [...repoAdds.keys()].some((p) => under(w.path!, p)));
  const loose = (view?.warnings ?? []).filter((w) => w.path !== undefined && !placed(w));

  const add = async () => {
    const checked = checkContextPath(typed);
    if ("error" in checked) {
      setInputError(checked.error);
      return;
    }
    const dest = checkContextDest(typedDest);
    if ("error" in dest) {
      setInputError(dest.error);
      return;
    }
    if (full) {
      setInputError(`${shownName} has ${MAX_CONTEXT_ENTRIES} entries, the most an agent can have.`);
      return;
    }
    setInputError(null);
    setRemoved(null);
    const saved = await ctx.apply({
      op: "add",
      path: checked.path,
      ...(dest.dest ? { dest: dest.dest } : {}),
    });
    if (saved) {
      setTyped("");
      setTypedDest("");
    }
  };

  const removeRow = async (entry: AgentContextEntry) => {
    setRemoved(null);
    if (await ctx.apply({ op: "remove", path: entry.path })) setRemoved(entry);
  };
  const undoRemove = async () => {
    if (!removed) return;
    // The whole stored entry, its `dest` and any keys a newer build stored on it.
    if (await ctx.apply({ op: "add_entries", entries: [removed] })) setRemoved(null);
  };

  // The repos with an override, and the one previewed (where a new override is made).
  const overrides = view?.repo_overrides ?? [];
  const repoBlocks = [
    ...overrides.map((o) => o.repo),
    ...(repo && !overrides.some((o) => o.repo.toLowerCase() === repo.toLowerCase()) ? [repo] : []),
  ];

  const suggestion = ctx.suggestion;

  return (
    <section
      aria-label="Context"
      data-testid="agent-context-section"
      className="flex flex-col gap-3"
    >
      <header className="flex flex-wrap items-start gap-x-3 gap-y-1.5">
        <div className="flex min-w-0 flex-1 basis-64 flex-col gap-0.5">
          <h3 className="text-sm font-semibold">Context</h3>
          <p className="text-xs text-muted-foreground">
            The folders and files {shownName} gets in its workspace at launch, beside what it starts
            with. A change here saves at once.
          </p>
        </div>
        <label className="flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground">
          Preview in
          <PortaledSelect
            ariaLabel="Preview in"
            triggerClassName="h-7 w-auto min-w-28 text-xs"
            value={repo ?? WORKSPACE}
            onValueChange={(v) => {
              setPicked(v);
              setRemoved(null);
              setRepoDirty(false);
            }}
            options={[
              ...repos.map((label) => ({ value: label, label: repoName(label) })),
              { value: WORKSPACE, label: "the team workspace" },
            ]}
          />
        </label>
      </header>

      {suggestion && (
        <div
          data-testid="agent-context-suggestion"
          role="region"
          aria-label="Suggested context"
          className="flex flex-col gap-2 rounded-md border border-border bg-muted/50 px-3 py-2.5"
        >
          <div className="flex items-start gap-2">
            <Lightbulb
              aria-hidden="true"
              className="mt-0.5 size-4 shrink-0 text-muted-foreground"
            />
            <div className="flex min-w-0 flex-col gap-0.5">
              <p className="text-sm font-medium">Add these to {shownName}&apos;s context?</p>
              <p className="text-xs text-muted-foreground">
                Your team&apos;s retired projections.yml used to copy these into all workspaces.
                Nothing changes unless you add them.
              </p>
            </div>
          </div>
          <ul className="flex flex-col gap-0.5 pl-6">
            {suggestion.entries.map((e) => (
              <li
                key={`${e.path}>${e.dest ?? ""}`}
                className="flex min-w-0 items-center gap-1.5 text-xs"
              >
                <span className="truncate font-mono">{e.path}</span>
                {e.dest &&
                  // A folder above already delivers it at its own path: this adds a copy.
                  (rows.some((r) => under(e.path, r.path)) ? (
                    <span className="text-muted-foreground">
                      also delivered as <span className="font-mono">{e.dest}</span>
                    </span>
                  ) : (
                    <DestTag dest={e.dest} />
                  ))}
              </li>
            ))}
          </ul>
          <div className="flex items-center gap-2 pl-6">
            <Button
              size="sm"
              disabled={saving || !view}
              onClick={() =>
                void ctx.apply({
                  op: "add_entries",
                  entries: suggestion.entries,
                  accepts: suggestion.fingerprint,
                })
              }
            >
              {saving ? <Loader2 className="animate-spin" /> : <Plus />}
              {suggestion.entries.length === 1
                ? "Add this one"
                : `Add these ${suggestion.entries.length}`}
            </Button>
            <Button
              size="sm"
              variant="ghost"
              disabled={saving}
              onClick={() => void ctx.dismissSuggestion()}
            >
              Dismiss
            </Button>
          </div>
        </div>
      )}

      {ctx.error && !view && (
        <p className="flex flex-wrap items-center gap-2 text-xs text-destructive">
          <CircleAlert aria-hidden="true" className="size-3 shrink-0" />
          Couldn&apos;t resolve {shownName}&apos;s context: {ctx.error}
          <Button size="sm" variant="outline" onClick={ctx.reload}>
            <RotateCcw />
            Retry
          </Button>
        </p>
      )}

      {view && defaulted && (
        <div
          data-testid="agent-context-default"
          className="flex flex-wrap items-center gap-x-3 gap-y-1.5 rounded-md bg-muted/50 px-3 py-2"
        >
          <Info aria-hidden="true" className="size-4 shrink-0 text-muted-foreground" />
          <div className="flex min-w-0 flex-1 flex-col">
            <span className="text-sm font-medium">Using the default for {runtimeName}</span>
            <span className="text-xs text-muted-foreground">
              Customize, or adding a path, writes these {plural(rows.length, "entry", "entries")}{" "}
              into {shownName} for every launch.
            </span>
          </div>
          <Button
            size="sm"
            variant="outline"
            disabled={saving}
            onClick={() => void ctx.apply({ op: "customize" })}
          >
            Customize
          </Button>
        </div>
      )}

      {view && (
        <ul
          aria-label={`${shownName}'s context`}
          className="flex flex-col divide-y divide-border/60"
        >
          {rows.length === 0 && (
            <li className="py-1 text-xs text-muted-foreground">
              No context: {shownName} gets only what it starts with.
            </li>
          )}
          {rows.map((entry) => (
            <EntryRow
              key={`${entry.path}>${entry.dest ?? ""}`}
              entry={entry}
              counts={workspaceDefault.length > 0 ? undefined : countsFor(entry)}
              isFile={
                view.files.some((f) => f.logical === entry.path) || /\.[a-z0-9]+$/i.test(entry.path)
              }
              warnings={warningsFor(view.warnings, entry.path)}
              quiet={view.runtime_default.some(
                (d) => d.path === entry.path && (d.dest ?? null) === (entry.dest ?? null),
              )}
              disabled={saving}
              onRemove={defaulted ? undefined : () => void removeRow(entry)}
            />
          ))}
        </ul>
      )}
      {workspaceDefault.length > 0 && (
        <p data-testid="agent-context-workspace-default" className="text-xs text-muted-foreground">
          In the team workspace this default delivers: {workspaceDefault.join(", ")}
        </p>
      )}
      {removed && (
        <p
          data-testid="agent-context-removed"
          role="status"
          className="flex flex-wrap items-center gap-x-2 text-xs text-muted-foreground"
        >
          <span>
            Removed <span className="font-mono">{removed.path}</span>
          </span>
          <span aria-hidden="true">·</span>
          <button
            type="button"
            disabled={saving}
            className="rounded px-1 font-medium text-foreground underline-offset-2 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            onClick={() => void undoRemove()}
          >
            Undo
          </button>
        </p>
      )}
      {loose.length > 0 && (
        <div className="flex flex-col gap-0.5">
          {loose.map((w) => (
            <WarningLine key={`${w.code}:${w.path}:${w.message}`} warning={w} />
          ))}
        </div>
      )}

      {view && (
        <div className="flex flex-col gap-0.5">
          <div className="flex flex-wrap items-center gap-1.5">
            <ContentPathInput
              label="Context path"
              value={typed}
              onChange={(v) => {
                setTyped(v);
                setInputError(null);
              }}
              onSubmit={() => void add()}
              team={team}
              placeholder="docs/architecture/"
              disabled={saving}
              errorId={inputError ? pathErrorId : undefined}
            />
            <Input
              aria-label="Deliver as (optional)"
              aria-describedby={inputError ? pathErrorId : undefined}
              className={cn("h-8 w-36 font-mono text-xs", FIELD)}
              value={typedDest}
              placeholder="as… (optional)"
              disabled={saving}
              spellCheck={false}
              onChange={(e) => {
                setTypedDest(e.target.value);
                setInputError(null);
              }}
              onKeyDown={(e) => {
                if (e.key !== "Enter") return;
                e.preventDefault();
                void add();
              }}
            />
            <Button
              size="sm"
              variant="outline"
              disabled={saving || !typed.trim()}
              onClick={() => void add()}
            >
              <Plus />
              Add to context
            </Button>
          </div>
          {inputError && (
            <span
              id={pathErrorId}
              data-testid="agent-context-input-error"
              className="text-xs text-destructive"
            >
              {inputError}
            </span>
          )}
        </div>
      )}

      {problem && (
        <ProblemLine
          problem={problem}
          name={shownName}
          onReload={() => {
            ctx.clearProblem();
            ctx.reload();
          }}
        />
      )}

      {view && repos.length + overrides.length > 0 && (
        <div className="flex flex-col gap-1 border-t border-border/60 pt-2">
          <h4 className="text-xs font-medium text-muted-foreground">Per repo</h4>
          <ul className="flex flex-col divide-y divide-border/60">
            {repoBlocks.map((label) => {
              const o = overrides.find((x) => x.repo.toLowerCase() === label.toLowerCase());
              const previewed = repo !== null && label.toLowerCase() === repo.toLowerCase();
              return (
                <RepoOverrideBlock
                  // Remounted when it stops being the previewed repo: a path typed for it
                  // is dropped, not kept hidden.
                  key={`${label}:${previewed}`}
                  repo={label}
                  add={o?.add ?? []}
                  remove={o?.remove ?? []}
                  addCounts={previewed ? repoAdds : null}
                  editable={previewed}
                  team={team}
                  disabled={saving}
                  onEdit={(edit) => ctx.apply({ ...edit, repo: label })}
                  onDirty={setRepoDirty}
                />
              );
            })}
          </ul>
          {!repo && repos.length > 0 && (
            <p className="text-xs text-muted-foreground">
              Preview a repo to add or leave out something for it only.
            </p>
          )}
        </div>
      )}

      <footer className="flex flex-wrap items-center gap-x-3 gap-y-1 border-t border-border/60 pt-2">
        <span
          data-testid="agent-context-footer"
          aria-live="polite"
          className={cn(
            "ml-auto text-xs font-medium tabular-nums",
            ctx.loading && "text-muted-foreground",
          )}
        >
          {view
            ? `${shownName} gets ${contextSummary(view.totals)} in ${repo ? repoName(repo) : "the team workspace"}`
            : ctx.error
              ? "Not resolved"
              : "Resolving…"}
        </span>
        {view && view.startup.length > 0 && (
          <span className="basis-full text-right text-[11px] text-muted-foreground">
            Counts its {plural(view.startup.length, "startup path")} too.
          </span>
        )}
        {sizeWarnings.map((w) => (
          <span key={w.message} className="basis-full">
            <WarningLine warning={w} />
          </span>
        ))}
      </footer>
    </section>
  );
}
