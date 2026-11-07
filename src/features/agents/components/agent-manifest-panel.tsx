// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";
import {
  ArrowDown,
  ArrowUp,
  ChevronRight,
  CircleAlert,
  File,
  FileText,
  Folder,
  FolderOpen,
  Info,
  Lock,
  Plus,
  TriangleAlert,
  X,
} from "lucide-react";

import { Input } from "@/components/ui/input";
import { AppButton as Button } from "@/shared/components/app-button";
import { cn } from "@/lib/utils";
import {
  agentStartupCatalog,
  type AgentManifestEntry,
  type AgentStartupCatalog,
  type AgentTeamRef,
  type ResolvedAgentView,
} from "@/tauri/commands";

import { checkStartupPath, displayPath, startupConflict } from "../lib/agent-form";
import { useCurrentDocument } from "../lib/current-document";
/** The honest note while the startup manifest has no effect on a launch yet. */
export const BOOTSTRAP_NOTE =
  "Named local product launches apply this startup manifest. Story, remote, and other launch paths retain their existing context behavior.";
import { FIELD } from "./agent-form-fields";

function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** A byte count in the units a person reads. */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

const entryBytes = (entry: AgentManifestEntry) => entry.files.reduce((n, f) => n + f.bytes, 0);

/** Why an entry is there, in words. */
function originLabel(entry: AgentManifestEntry): string | null {
  switch (entry.origin.kind) {
    case "dependency":
      return `needed by ${entry.origin.of}`;
    case "required":
      return "required by CodeSense";
    default:
      return null;
  }
}

function layerLabel(entry: AgentManifestEntry | AgentStartupCatalog["entries"][number]): string {
  return entry.layer.kind === "personal" ? "personal" : entry.layer.team;
}

/** How long the picker's filter settles before the folder is listed again. */
const FILTER_DEBOUNCE_MS = 250;

/**
 * Browse a team's content one folder at a time and pick a file or a folder (a folder brings
 * everything under it). Each level is read only when opened; nothing walks the tree. A big
 * folder comes a page at a time ("Show more") and can be filtered by name, so every entry can
 * be reached and picked.
 */
function StartupPicker({
  team,
  onPick,
  onClose,
}: {
  team: AgentTeamRef;
  onPick: (path: string, isDir: boolean) => string | null;
  onClose: () => void;
}) {
  const [dir, setDir] = useState("");
  const [typedFilter, setTypedFilter] = useState("");
  const [filter, setFilter] = useState("");
  useEffect(() => {
    const timer = setTimeout(() => setFilter(typedFilter.trim()), FILTER_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [typedFilter]);
  // Each listing, with the folder and filter it lists: a listing of another is not shown.
  const [listing, setListing] = useState<{
    key: string;
    catalog?: AgentStartupCatalog;
    error?: string;
  } | null>(null);
  const [more, setMore] = useState<{ key: string; loading: boolean; error?: string } | null>(null);
  const [refused, setRefused] = useState<string | null>(null);
  const { org, team: teamName } = team;
  const key = `${org}\n${teamName}\n${dir}\n${filter}`;
  useEffect(() => {
    let live = true;
    agentStartupCatalog({ org, team: teamName }, dir || undefined, {
      filter: filter || undefined,
    }).then(
      (catalog) => live && setListing({ key, catalog }),
      (e: unknown) =>
        live && setListing({ key, error: e instanceof Error ? e.message : String(e) }),
    );
    return () => {
      live = false;
    };
  }, [org, teamName, dir, filter, key]);
  const current = listing?.key === key ? listing : null;
  const catalog = current?.catalog ?? null;
  const error = current?.error ?? null;
  const loadingMore = more?.key === key && more.loading;
  const moreError = more?.key === key ? (more.error ?? null) : null;
  const showMore = () => {
    if (!catalog || catalog.next_offset === null || loadingMore) return;
    setMore({ key, loading: true });
    agentStartupCatalog({ org, team: teamName }, dir || undefined, {
      offset: catalog.next_offset,
      filter: filter || undefined,
    }).then(
      (page) => {
        setListing((was) =>
          was?.key === key && was.catalog
            ? { key, catalog: { ...page, entries: [...was.catalog.entries, ...page.entries] } }
            : was,
        );
        setMore(null);
      },
      (e: unknown) =>
        setMore({ key, loading: false, error: e instanceof Error ? e.message : String(e) }),
    );
  };
  const open = (next: string) => {
    setDir(next);
    setTypedFilter("");
    setFilter("");
  };
  const crumbs = dir ? dir.split("/") : [];
  return (
    <div
      data-testid="agent-startup-picker"
      className="flex flex-col gap-1.5 rounded-md border border-border bg-muted/30 p-2"
    >
      <div className="flex flex-wrap items-center gap-1 text-xs">
        <button type="button" className="font-medium hover:underline" onClick={() => open("")}>
          {teamName} content
        </button>
        {crumbs.map((segment, i) => (
          <span key={`${i}:${segment}`} className="inline-flex items-center gap-1">
            <ChevronRight aria-hidden="true" className="size-3 text-muted-foreground" />
            <button
              type="button"
              className="hover:underline"
              onClick={() => open(crumbs.slice(0, i + 1).join("/"))}
            >
              {segment}
            </button>
          </span>
        ))}
        <Button className="ml-auto" size="sm" variant="ghost" onClick={onClose}>
          Done
        </Button>
      </div>
      <Input
        aria-label="Filter this folder"
        className={cn("h-7 text-xs", FIELD)}
        value={typedFilter}
        placeholder="Filter this folder by name"
        onChange={(e) => setTypedFilter(e.target.value)}
      />
      {error && <p className="text-xs text-destructive">Couldn&apos;t list this folder: {error}</p>}
      {!catalog && !error && <p className="text-xs text-muted-foreground">Loading…</p>}
      {refused && (
        <p data-testid="agent-picker-refused" className="text-xs text-warning-ink">
          {refused}
        </p>
      )}
      {catalog && (
        <ul className="flex max-h-64 flex-col overflow-y-auto">
          {catalog.entries.length === 0 && (
            <li className="text-xs text-muted-foreground">
              {filter ? `Nothing here matches "${filter}".` : "Nothing here."}
            </li>
          )}
          {catalog.entries.map((entry) => {
            const name = entry.path.slice(entry.path.lastIndexOf("/") + 1);
            return (
              <li
                key={entry.path}
                data-testid="agent-picker-entry"
                data-path={entry.path}
                className="flex items-center gap-1.5 rounded px-1 py-0.5 text-sm hover:bg-muted"
              >
                {entry.is_dir ? (
                  <button
                    type="button"
                    aria-label={`Open folder ${entry.path}`}
                    className="flex min-w-0 flex-1 items-center gap-1.5 text-left"
                    onClick={() => open(entry.path)}
                  >
                    <Folder
                      aria-hidden="true"
                      className="size-3.5 shrink-0 text-muted-foreground"
                    />
                    <span className="truncate">{name}/</span>
                  </button>
                ) : (
                  <span className="flex min-w-0 flex-1 items-center gap-1.5">
                    <File aria-hidden="true" className="size-3.5 shrink-0 text-muted-foreground" />
                    <span className="truncate">{name}</span>
                  </span>
                )}
                <span className="shrink-0 text-[11px] text-muted-foreground">
                  {layerLabel(entry)}
                </span>
                <Button
                  size="sm"
                  variant="outline"
                  aria-label={`Add ${displayPath(entry.path, entry.is_dir)}`}
                  onClick={() => setRefused(onPick(entry.path, entry.is_dir))}
                >
                  <Plus />
                  Add
                </Button>
              </li>
            );
          })}
        </ul>
      )}
      {catalog?.truncated && (
        <div
          data-testid="agent-picker-more"
          className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground"
        >
          <span>
            Showing {catalog.entries.length} of {catalog.total}.
          </span>
          <Button size="sm" variant="outline" disabled={loadingMore} onClick={showMore}>
            {loadingMore ? "Loading…" : "Show more"}
          </Button>
          {moreError && (
            <span className="text-destructive">Couldn&apos;t list more: {moreError}</span>
          )}
        </div>
      )}
    </div>
  );
}

/**
 * What the agent starts with: the declared startup paths in order (reorder, remove, add by
 * picking from team content, by typing a path, or the document you were viewing), then the
 * effective manifest the live preview resolved — each entry with its files and bytes, what a
 * path pulled in and why, what CodeSense itself adds, totals, errors against the path at fault,
 * and the `/name`s the agent will have. Errors and warnings are shown, never enforced.
 */
export function AgentManifestPanel({
  startup,
  onChange,
  resolved,
  catalogTeam,
  contentRoot,
}: {
  startup: string[];
  onChange: (next: string[]) => void;
  resolved: ResolvedAgentView | null;
  /** The team whose content the picker browses. */
  catalogTeam: AgentTeamRef;
  /** The active team's content root, for "Add current document"; empty when the active team
   *  is not the agent's. */
  contentRoot: string;
}) {
  const [picking, setPicking] = useState(false);
  const [typed, setTyped] = useState("");
  const [typedError, setTypedError] = useState<string | null>(null);
  const current = useCurrentDocument(contentRoot);
  // Folders picked or typed with a trailing `/`, known before the preview resolves them.
  const [knownDirs, setKnownDirs] = useState<ReadonlySet<string>>(() => new Set());
  const manifest = resolved?.manifest ?? null;
  const isDir = (path: string) => {
    if (knownDirs.has(path)) return true;
    const entry = manifest?.entries.find((e) => e.path === path);
    return entry !== undefined && entry.files.some((f) => f.logical !== path);
  };

  /** Add `raw` to the list, or say why not. */
  const add = (raw: string, folder = false): string | null => {
    const checked = checkStartupPath(raw);
    if ("error" in checked) return checked.error;
    const conflict = startupConflict(startup, checked.path);
    if (conflict) return conflict;
    if (folder) setKnownDirs((known) => new Set(known).add(checked.path));
    onChange([...startup, checked.path]);
    return null;
  };
  const move = (from: number, to: number) => {
    const next = [...startup];
    const [item] = next.splice(from, 1);
    next.splice(to, 0, item);
    onChange(next);
  };
  const errorsFor = (path: string) => (manifest?.errors ?? []).filter((e) => e.path === path);
  const unplaced = (manifest?.errors ?? []).filter(
    (e) => e.path === null || !startup.includes(e.path),
  );

  return (
    <section
      aria-label="Starts with"
      data-testid="agent-manifest-panel"
      className="flex flex-col gap-3"
    >
      <h3 className="text-sm font-semibold">Starts with</h3>
      {resolved && !resolved.manifest_applies_at_launch && (
        <p
          data-testid="agent-editor-bootstrap-note"
          className="flex items-start gap-1.5 rounded-md border border-border bg-muted/40 px-2.5 py-1.5 text-xs text-muted-foreground"
        >
          <Info aria-hidden="true" className="mt-0.5 size-3.5 shrink-0" />
          {BOOTSTRAP_NOTE}
        </p>
      )}
      <ol data-testid="agent-startup-list" className="flex flex-col gap-1">
        {startup.length === 0 && (
          <li className="text-xs text-muted-foreground">
            Nothing declared: the agent starts with only what CodeSense itself needs.
          </li>
        )}
        {startup.map((path, i) => {
          const entry = manifest?.entries.find((e) => e.path === path);
          const errors = errorsFor(path);
          return (
            <li
              key={path}
              data-testid="agent-startup-item"
              data-path={path}
              className="flex flex-col"
            >
              <div className="flex items-center gap-1.5 text-sm">
                {isDir(path) ? (
                  <FolderOpen
                    aria-hidden="true"
                    className="size-3.5 shrink-0 text-muted-foreground"
                  />
                ) : (
                  <FileText
                    aria-hidden="true"
                    className="size-3.5 shrink-0 text-muted-foreground"
                  />
                )}
                <span className="min-w-0 truncate font-mono text-xs">
                  {displayPath(path, isDir(path))}
                </span>
                {entry && (
                  <span className="shrink-0 text-xs text-muted-foreground">
                    {plural(entry.files.length, "file")} · {formatBytes(entryBytes(entry))} ·{" "}
                    {layerLabel(entry)}
                  </span>
                )}
                <span className="ml-auto flex shrink-0 items-center">
                  <button
                    type="button"
                    aria-label={`Move ${path} up`}
                    disabled={i === 0}
                    className="rounded p-0.5 text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-30"
                    onClick={() => move(i, i - 1)}
                  >
                    <ArrowUp aria-hidden="true" className="size-3.5" />
                  </button>
                  <button
                    type="button"
                    aria-label={`Move ${path} down`}
                    disabled={i === startup.length - 1}
                    className="rounded p-0.5 text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-30"
                    onClick={() => move(i, i + 1)}
                  >
                    <ArrowDown aria-hidden="true" className="size-3.5" />
                  </button>
                  <button
                    type="button"
                    aria-label={`Remove ${path}`}
                    className="rounded p-0.5 text-muted-foreground hover:bg-muted hover:text-foreground"
                    onClick={() => onChange(startup.filter((p) => p !== path))}
                  >
                    <X aria-hidden="true" className="size-3.5" />
                  </button>
                </span>
              </div>
              {errors.map((e) => (
                <span
                  key={`${e.code}:${e.message}`}
                  data-testid="agent-startup-error"
                  data-code={e.code}
                  className="ml-5 flex items-start gap-1 text-xs text-destructive"
                >
                  <CircleAlert aria-hidden="true" className="mt-0.5 size-3 shrink-0" />
                  {e.code}: {e.message}
                </span>
              ))}
            </li>
          );
        })}
      </ol>
      <div className="flex flex-wrap items-start gap-2">
        <Button size="sm" variant="outline" onClick={() => setPicking((p) => !p)}>
          <FolderOpen />
          Pick files/folders from team content
        </Button>
        <Button
          size="sm"
          variant="outline"
          disabled={!current}
          title={
            current
              ? `Add ${current}`
              : contentRoot
                ? "Open a document in this team first"
                : "Switch to this agent's team to add the document you're viewing there"
          }
          onClick={() => {
            if (current) setTypedError(add(current));
          }}
        >
          <Plus />
          Add current document
        </Button>
        <div className="flex min-w-64 flex-1 flex-col gap-0.5">
          <div className="flex items-center gap-1.5">
            <Input
              aria-label="Startup path"
              className={cn("h-8 font-mono text-xs", FIELD)}
              value={typed}
              placeholder="Type a path, e.g. docs/runbooks/"
              onChange={(e) => {
                setTyped(e.target.value);
                setTypedError(null);
              }}
              onKeyDown={(e) => {
                if (e.key !== "Enter") return;
                e.preventDefault();
                const why = add(typed, typed.trim().endsWith("/"));
                setTypedError(why);
                if (!why) setTyped("");
              }}
            />
            <Button
              size="sm"
              variant="outline"
              disabled={!typed.trim()}
              onClick={() => {
                const why = add(typed, typed.trim().endsWith("/"));
                setTypedError(why);
                if (!why) setTyped("");
              }}
            >
              Add path
            </Button>
          </div>
          {typedError && (
            <span data-testid="agent-startup-path-error" className="text-xs text-destructive">
              {typedError}
            </span>
          )}
        </div>
      </div>
      {picking && (
        <StartupPicker team={catalogTeam} onPick={add} onClose={() => setPicking(false)} />
      )}
      <EffectiveManifest resolved={resolved} startup={startup} unplaced={unplaced} />
    </section>
  );
}

/** The resolved manifest beyond the declared list: what was pulled in, what CodeSense adds,
 *  totals, the `/name`s provided, warnings, and errors not tied to a declared path. */
function EffectiveManifest({
  resolved,
  startup,
  unplaced,
}: {
  resolved: ResolvedAgentView | null;
  startup: string[];
  unplaced: ResolvedAgentView["manifest"]["errors"];
}) {
  if (!resolved) {
    return <p className="text-xs text-muted-foreground">Resolving the effective manifest…</p>;
  }
  const { manifest, provides, warnings } = resolved;
  const implied = manifest.entries.filter((e) => !startup.includes(e.path));
  const names = [
    ...provides.commands.map((c) => `/${c}`),
    ...provides.skills.map((s) => `/${s} (skill)`),
  ];
  return (
    <div data-testid="agent-effective-manifest" className="flex flex-col gap-2 text-xs">
      <h4 className="font-medium text-muted-foreground">Effective manifest</h4>
      <ul className="flex flex-col gap-0.5">
        {implied.map((entry) => (
          <li
            key={entry.path}
            data-testid="agent-manifest-implied"
            data-path={entry.path}
            className="flex flex-wrap items-center gap-1.5"
          >
            <Lock aria-hidden="true" className="size-3 shrink-0 text-muted-foreground" />
            <span className="font-mono">{entry.path}</span>
            <span className="text-muted-foreground">
              {originLabel(entry)} · {plural(entry.files.length, "file")} ·{" "}
              {formatBytes(entryBytes(entry))}
            </span>
          </li>
        ))}
        {manifest.essentials.map((essential) => (
          <li
            key={essential.id}
            data-testid="agent-manifest-essential"
            className="flex items-center gap-1.5 text-muted-foreground"
          >
            <Lock aria-hidden="true" className="size-3 shrink-0" />
            {essential.reason}
          </li>
        ))}
      </ul>
      <p data-testid="agent-manifest-totals" className="font-medium">
        {plural(manifest.entries.length, "entry", "entries")} ·{" "}
        {plural(manifest.totals.files, "file")} · {formatBytes(manifest.totals.bytes)}
      </p>
      {names.length > 0 && (
        <p data-testid="agent-provides" className="text-muted-foreground">
          Provides {names.join(", ")}
        </p>
      )}
      {unplaced.map((e) => (
        <p
          key={`${e.path ?? ""}:${e.message}`}
          data-testid="agent-manifest-error"
          className="flex items-start gap-1 text-destructive"
        >
          <CircleAlert aria-hidden="true" className="mt-0.5 size-3 shrink-0" />
          {e.path ? `${e.path}: ` : ""}
          {e.code}: {e.message}
        </p>
      ))}
      {warnings.map((w) => (
        <p
          key={`${w.code}:${w.message}`}
          data-testid="agent-manifest-warning"
          className="flex items-start gap-1 text-warning-ink"
        >
          <TriangleAlert aria-hidden="true" className="mt-0.5 size-3 shrink-0" />
          {w.message}
        </p>
      ))}
    </div>
  );
}
