// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  Bot,
  FolderOpen,
  Info,
  Layers,
  Loader2,
  ShieldCheck,
  Sparkles,
  User,
  Wrench,
  type LucideIcon,
} from "lucide-react";

import { InlineAlert } from "@/components/ui/inline-alert";
import { AppButton as Button } from "@/shared/components/app-button";
import { ONLY_ME, WhoCanUseItField, resolveAudience } from "@/shared/components/who-can-use-it";
import { useOrgStore } from "@/shared/stores/org-store";
import { cn } from "@/lib/utils";
import type {
  AgentDetailView,
  AgentForm,
  AgentLaunchWarning,
  AgentRow,
  AgentTeamRef,
} from "@/tauri/commands";

import {
  changedFields,
  draftFromDefinition,
  editorSectionOf,
  emptyDraft,
  formFromDraft,
  isChanged,
  type AgentDraft,
  type AgentPlacement,
  type EditorSection,
} from "../lib/agent-form";
import { isShown, sectionIn } from "../lib/section-dom";
import { useAgentRequest } from "../lib/use-agent-request";
import { lastResolved, useAgentPreview } from "../lib/use-agent-preview";
import { useAgentsStore } from "../stores/agents-store";
import { AgentContextSection } from "./agent-context-section";
import { AgentManifestPanel } from "./agent-manifest-panel";
import { AgentPermissionsSection } from "./agent-permissions";
import { AgentBasicsFields, AgentRunsOnFields, AgentToolsFields } from "./agent-form-fields";

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** The personal space as a team, for browsing when no team is active. */
const PERSONAL_SPACE: AgentTeamRef = { org: "_personal/_personal", team: "personal" };

/** What a preview names a draft that has no name yet, so it still resolves. */
const UNNAMED = "New agent";

/** Report whether this form holds an unsaved edit, so leaving it asks first; an unmounted form
 *  holds none. */
function useReportDirty(dirty: boolean) {
  const setFormDirty = useAgentsStore((s) => s.setFormDirty);
  useEffect(() => setFormDirty(dirty), [dirty, setFormDirty]);
  useEffect(() => () => setFormDirty(false), [setFormDirty]);
}

/** The active team, when one is. */
function useActiveTeam(): AgentTeamRef | undefined {
  const org = useOrgStore((s) => s.activeOrgSlug);
  const team = useOrgStore((s) => s.activeTeamName);
  return org && team ? { org, team } : undefined;
}

/** A refused input, named by the field the backend blames. */
function InvalidInput({ field, message }: { field: string | null; message: string }) {
  return (
    <p data-testid="agent-invalid" className="text-sm text-destructive">
      {field ? `${field}: ` : ""}
      {message}
    </p>
  );
}

function SaveWarnings({ warnings }: { warnings: AgentLaunchWarning[] | undefined }) {
  if (!warnings?.length) return null;
  return (
    <ul
      data-testid="agent-save-warnings"
      className="flex flex-col gap-0.5 text-xs text-warning-ink"
    >
      {warnings.map((w) => (
        <li key={`${w.code}:${w.message}`}>{w.message}</li>
      ))}
    </ul>
  );
}

/** The sections of Edit, in the order the in-page nav lists them. */
const SECTIONS: {
  id: EditorSection;
  label: string;
  icon: LucideIcon;
  sub?: string;
}[] = [
  { id: "basics", label: "Basics", icon: User },
  { id: "runs-on", label: "Runs on", icon: Sparkles },
  { id: "tools", label: "Tools", icon: Wrench },
  { id: "starts-with", label: "Starts with", icon: FolderOpen },
  { id: "context", label: "Context", icon: Layers, sub: "Saves at once" },
  { id: "permissions", label: "Permissions", icon: ShieldCheck },
];

const sectionDomId = (id: EditorSection) => `ed-${id}`;

/** A plain section: a heading over its fields, with a rule under it. */
function EditorSectionBlock({
  id,
  title,
  sub,
  children,
}: {
  id: EditorSection;
  title: string;
  sub?: string;
  children: ReactNode;
}) {
  return (
    <section
      id={sectionDomId(id)}
      aria-label={title}
      data-testid={`agent-section-${id}`}
      className="scroll-mt-4 border-b border-border/70 py-[18px] last:border-b-0"
    >
      <h3 className="mb-3 flex items-center gap-2 text-sm font-semibold">
        {title}
        {sub && <span className="text-xs font-normal text-muted-foreground">{sub}</span>}
      </h3>
      {children}
    </section>
  );
}

/** A section that brings its own heading, in the editor's rhythm. */
function OwnedSection({ id, children }: { id: EditorSection; children: ReactNode }) {
  return (
    <div id={sectionDomId(id)} className="scroll-mt-4 border-b border-border/70 py-[18px]">
      {children}
    </div>
  );
}

/**
 * The editable agent, in sections: basics, what it runs on, its tools, what it starts with,
 * its context and its permissions, over a live preview of the unsaved form. A personal agent
 * previews and browses against the active team (the team it would be launched into); a team
 * agent against its own team.
 */
function AgentFormBody({
  draft,
  onChange,
  form,
  previewTeam,
  catalogTeam,
  contentRoot,
  context,
  saved,
}: {
  draft: AgentDraft;
  onChange: (next: AgentDraft) => void;
  form: AgentForm;
  previewTeam: AgentTeamRef | undefined;
  catalogTeam: AgentTeamRef;
  contentRoot: string;
  /** The saved agent's Context section, after what it starts with. */
  context?: ReactNode;
  /** The draft as last saved, for what an edit says it was. */
  saved?: AgentDraft;
}) {
  const preview = useAgentPreview(form.name ? form : { ...form, name: UNNAMED }, previewTeam);
  const resolved = lastResolved(preview);
  return (
    <div className="flex flex-col" data-testid="agent-sentence-form">
      <EditorSectionBlock id="basics" title="Basics">
        <AgentBasicsFields
          agentKey={form.key}
          teamKey={`${catalogTeam.org}:${catalogTeam.team}`}
          draft={draft}
          onChange={onChange}
        />
      </EditorSectionBlock>
      <EditorSectionBlock id="runs-on" title="Runs on">
        <AgentRunsOnFields
          saved={saved}
          admission={
            resolved?.manifest.runtime === draft.runtime ? resolved.launch_admission : undefined
          }
          draft={draft}
          onChange={onChange}
          escalating={resolved?.launch.escalating ?? false}
        />
      </EditorSectionBlock>
      <EditorSectionBlock id="tools" title="Tools" sub="MCP servers this agent can use">
        <AgentToolsFields draft={draft} onChange={onChange} offered={preview.servers} />
      </EditorSectionBlock>
      {preview.status === "invalid" && (
        <p data-testid="agent-preview-invalid" className="pt-3 text-xs text-destructive">
          The preview can&apos;t resolve this yet — {preview.field ? `${preview.field}: ` : ""}
          {preview.message}
        </p>
      )}
      {preview.status === "error" && (
        <p className="pt-3 text-xs text-destructive">Couldn&apos;t preview: {preview.message}</p>
      )}
      <OwnedSection id="starts-with">
        <AgentManifestPanel
          startup={draft.startup}
          onChange={(startup) => onChange({ ...draft, startup })}
          resolved={resolved}
          catalogTeam={catalogTeam}
          contentRoot={contentRoot}
        />
      </OwnedSection>
      {context && <OwnedSection id="context">{context}</OwnedSection>}
      <OwnedSection id="permissions">
        <AgentPermissionsSection draft={draft} onChange={onChange} resolved={resolved} />
      </OwnedSection>
    </div>
  );
}

/** The in-page nav of Edit: a link per section, a dot on the ones with an unsaved edit. */
function SectionNav({
  active,
  dirty,
  onPick,
}: {
  active: EditorSection;
  dirty: ReadonlySet<EditorSection>;
  onPick: (id: EditorSection) => void;
}) {
  return (
    <nav
      aria-label="Sections"
      data-testid="agent-editor-nav"
      className="sticky top-0 flex w-44 shrink-0 flex-col gap-px self-start border-r border-border/70 py-4 pr-2 pl-5"
    >
      {SECTIONS.map(({ id, label, icon: Icon, sub }) => (
        <button
          key={id}
          type="button"
          onClick={() => onPick(id)}
          aria-current={active === id ? "true" : undefined}
          className={cn(
            "flex items-center gap-2 rounded-sm px-2.5 text-left text-sm text-muted-foreground outline-none hover:bg-muted/55 hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring",
            sub ? "h-10" : "h-[30px]",
            active === id && "bg-muted font-medium text-foreground",
          )}
        >
          <Icon aria-hidden="true" className="size-3.5 shrink-0" />
          <span className="min-w-0">
            {label}
            {sub && <small className="block text-[11px] font-normal leading-tight">{sub}</small>}
          </span>
          {dirty.has(id) && (
            <span
              role="img"
              aria-label="Edited"
              title="Edited"
              className="ml-auto size-1.5 rounded-full bg-ring"
            />
          )}
        </button>
      ))}
    </nav>
  );
}

/** The words for which sections an unsaved edit touches. */
function dirtySummary(sections: readonly EditorSection[]): string {
  const names = SECTIONS.filter((s) => sections.includes(s.id)).map((s) => s.label);
  if (names.length === 0) return "";
  if (names.length === 1) return `in ${names[0]}`;
  return `in ${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

/**
 * An agent's Edit view: everything in its sentence, editable by anyone who can see it. A save
 * is a local edit to the context repo, shared by the user's git push. It is made against the
 * revision the form was loaded from; if someone (or an agent) changed it since, nothing is
 * written, the edit stays in the form, and Reload brings the newer agent in.
 */
export function AgentEditor({
  row,
  detail,
  contentRoot,
}: {
  row: AgentRow;
  detail: AgentDetailView;
  contentRoot: string;
}) {
  const saveAgent = useAgentsStore((s) => s.saveAgent);
  const loadDetail = useAgentsStore((s) => s.loadDetail);
  const saved = useAgentsStore((s) => s.saveWarnings[row.key]);
  // Only the revision that save wrote: a later edit or restore has its own warnings.
  const warnings =
    saved && saved.revision === detail.definition.revision ? saved.warnings : undefined;
  const activeTeam = useActiveTeam();
  const [loaded, setLoaded] = useState(() => ({
    revision: detail.definition.revision,
    base: draftFromDefinition(detail.definition),
  }));
  const [draft, setDraft] = useState<AgentDraft>(() => loaded.base);
  const [conflict, setConflict] = useState<number | null>(null);
  const [invalid, setInvalid] = useState<{ field: string | null; message: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const changed = isChanged(loaded.base, draft);
  // A path typed in the Context section and not yet added counts too: leaving asks first.
  const [contextTyped, setContextTyped] = useState(false);
  useReportDirty(changed || contextTyped);

  // External YAML edits may keep the revision. Refresh an unchanged form from its
  // definition, while keeping any edit in progress for the normal conflict flow.
  const revision = detail.definition.revision;
  useEffect(() => {
    if (changed || contextTyped) return;
    const base = draftFromDefinition(detail.definition);
    if (revision === loaded.revision && !isChanged(loaded.base, base)) return;
    setLoaded({ revision, base });
    setDraft(base);
    // Re-checked when the agent moves on and when an edit is undone back to nothing; the
    // draft itself is not a dependency, or typing would reset it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [detail.definition, changed, contextTyped]);

  const personal = row.scope === "personal";
  const forActiveTeam = personal;
  // "Add current document" reads the active team's viewer, which speaks for this agent's own
  // team only when that is the active team (or the agent is personal).
  const ownRoot =
    forActiveTeam || (activeTeam?.org === row.org && activeTeam?.team === row.team)
      ? contentRoot
      : "";
  const previewTeam = forActiveTeam ? activeTeam : undefined;
  const catalogTeam = forActiveTeam
    ? (activeTeam ?? PERSONAL_SPACE)
    : { org: row.org, team: row.team };
  const form: AgentForm = formFromDraft(draft, { key: row.key });
  // A context edit saves at once, as a revision the form never sends (it leaves `context`
  // out, so a form save keeps it): move the form's base past it, or Save would conflict.
  const onContextSaved = useCallback((from: number, to: number) => {
    setLoaded((was) => (was.revision === from ? { ...was, revision: to } : was));
  }, []);

  const reload = async () => {
    setConflict(null);
    setError(null);
    setInvalid(null);
    await loadDetail(row.key);
    const state = useAgentsStore.getState();
    const fresh = state.details[row.key]?.definition;
    // A failed re-read keeps the edit and its base: rebasing on what was already stale would
    // only conflict again.
    if (!fresh || state.detailError[row.key]) {
      setError(`Couldn't reload: ${state.detailError[row.key] ?? "the agent could not be read"}`);
      return;
    }
    const base = draftFromDefinition(fresh);
    setLoaded({ revision: fresh.revision, base });
    setDraft(base);
  };

  const save = async () => {
    if (!changed || saving) return;
    setSaving(true);
    setError(null);
    setInvalid(null);
    try {
      const reply = await saveAgent(form, loaded.revision);
      if (reply.outcome === "conflict") {
        setConflict(reply.current_revision);
      } else if (reply.outcome === "invalid") {
        setInvalid(reply);
      } else {
        setConflict(null);
        setLoaded({ revision: reply.revision, base: draft });
      }
    } catch (e) {
      setError(errorText(e));
    } finally {
      setSaving(false);
    }
  };

  const fields = useMemo(() => changedFields(loaded.base, draft), [loaded.base, draft]);
  const touched = useMemo(() => new Set(fields.map(editorSectionOf)), [fields]);
  const [active, setActive] = useState<EditorSection>("basics");
  const jump = useAgentRequest("edit-section", row.key);
  // Retained teams each keep an editor with the same section ids: look in this one only.
  const rootRef = useRef<HTMLElement>(null);
  const goTo = useCallback((id: EditorSection) => {
    setActive(id);
    sectionIn(rootRef.current, id)?.scrollIntoView?.({ block: "start" });
  }, []);
  // A fact on the Overview asked for a section: go there once, then forget the request.
  useEffect(() => {
    // Only the shown team's editor gets the request (`useAgentRequest`); a hidden DOM subtree is
    // skipped too.
    if (!jump || !isShown(rootRef.current)) return;
    jump.consume();
    const id = SECTIONS.find((sec) => sec.id === jump.request.section)?.id;
    if (id) window.setTimeout(() => goTo(id), 0);
  }, [jump, goTo]);
  const count = fields.length + (contextTyped ? 1 : 0);

  return (
    <section ref={rootRef} aria-label="Edit" data-testid="agent-editor" className="flex min-h-full">
      <SectionNav active={active} dirty={touched} onPick={goTo} />
      <div className="relative flex min-w-0 flex-1 flex-col">
        <div className="max-w-[760px] px-7 pt-1.5 pb-6">
          <AgentFormBody
            draft={draft}
            onChange={setDraft}
            form={form}
            previewTeam={previewTeam}
            catalogTeam={catalogTeam}
            contentRoot={ownRoot}
            saved={loaded.base}
            context={
              <AgentContextSection
                key={row.key}
                agentKey={row.key}
                name={detail.definition.name || row.name}
                runtime={detail.definition.runtime}
                team={catalogTeam === PERSONAL_SPACE ? undefined : catalogTeam}
                revision={detail.definition.revision}
                onSaved={onContextSaved}
                onDirtyChange={setContextTyped}
              />
            }
          />
          <div className="flex flex-col gap-3 pt-3">
            <SaveWarnings warnings={warnings} />
            {conflict !== null && (
              <InlineAlert
                data-testid="agent-conflict"
                severity="warning"
                title="Changed elsewhere — reload"
                action={{ label: "Reload", onClick: () => void reload() }}
              >
                This agent is now at revision {conflict}, so nothing was saved. Your edit is still
                here; Reload replaces it with the newer version.
              </InlineAlert>
            )}
            {invalid && <InvalidInput {...invalid} />}
            {error && (
              <p data-testid="agent-save-error" className="text-sm text-destructive">
                {error}
              </p>
            )}
          </div>
        </div>
        <div
          role="region"
          aria-label="Unsaved changes"
          data-testid="agent-savebar"
          className="sticky bottom-0 mt-auto flex h-14 items-center gap-3 border-t border-border bg-background px-7 shadow-[0_-10px_18px_var(--background)]"
        >
          <span className="flex items-center gap-2 text-[13px]" data-testid="agent-savebar-msg">
            <Info aria-hidden="true" className="size-3.5 text-muted-foreground" />
            {count > 0 ? (
              <span>
                <b className="font-semibold">
                  {count === 1 ? "1 unsaved change" : `${count} unsaved changes`}
                </b>{" "}
                {dirtySummary([...touched])}
              </span>
            ) : (
              <span className="text-muted-foreground">
                Revision {loaded.revision} · saved here, shared when you push
              </span>
            )}
          </span>
          <div className="ml-auto flex items-center gap-2">
            {changed && (
              <Button
                variant="ghost"
                disabled={saving}
                onClick={() => {
                  setDraft(loaded.base);
                  setConflict(null);
                  setInvalid(null);
                }}
              >
                Discard
              </Button>
            )}
            <Button disabled={!changed || saving} onClick={() => void save()}>
              {saving && <Loader2 className="animate-spin" />}
              Save changes
            </Button>
          </div>
        </div>
      </div>
    </section>
  );
}

const EMPTY = emptyDraft();

/** The New agent form: the same fields, who can use it (a team, which everyone in it can
 *  use and edit — the default — or only you, kept in your scratch space), and a way to
 *  describe it to an agent instead. */
export function AgentCreateForm({ contentRoot }: { contentRoot: string }) {
  const saveAgent = useAgentsStore((s) => s.saveAgent);
  const cancel = useAgentsStore((s) => s.cancelCreate);
  const describe = useAgentsStore((s) => s.describeToAgent);
  const describing = useAgentsStore((s) => s.pending.describe === true);
  const rows = useAgentsStore((s) => s.rows);
  const activeTeam = useActiveTeam();
  const activeOrg = useOrgStore((s) => s.activeOrgSlug);
  const [draft, setDraft] = useState<AgentDraft>(emptyDraft);
  const [choice, setChoice] = useState<string | null>(null);
  const [invalid, setInvalid] = useState<{ field: string | null; message: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  useReportDirty(isChanged(EMPTY, draft) || choice !== null);

  // The teams a new agent can go to: the active one and every team that has agents here.
  const teams = new Map<string, AgentTeamRef>();
  if (activeTeam) teams.set(`${activeTeam.org}/${activeTeam.team}`, activeTeam);
  for (const row of rows ?? []) {
    if (row.scope === "team") teams.set(`${row.org}/${row.team}`, { org: row.org, team: row.team });
  }
  // Name each team with its org only when the list spans more than one org.
  const manyOrgs = new Set([...teams.values()].map((t) => t.org)).size > 1;
  const audience = [...teams.entries()].map(([id, t]) => ({
    value: id,
    label: manyOrgs ? `${t.team} · ${t.org}` : t.team,
  }));
  // Default to the open team, else a team of the open org; with neither, ask for a pick
  // (never a team of another org).
  const fallback =
    (activeTeam && `${activeTeam.org}/${activeTeam.team}`) ??
    audience.find((a) => teams.get(a.value)?.org === activeOrg)?.value ??
    null;
  const where = resolveAudience(choice, audience, fallback);
  const team = where === ONLY_ME ? undefined : teams.get(where);
  const placement: AgentPlacement = team ? { scope: "team", ...team } : { scope: "personal" };
  const form = formFromDraft(draft, { placement });
  const ready = draft.name.trim() !== "" && where !== "";

  const save = async () => {
    if (!ready || saving) return;
    setSaving(true);
    setError(null);
    setInvalid(null);
    try {
      const reply = await saveAgent(form);
      if (reply.outcome === "invalid") setInvalid(reply);
      if (reply.outcome !== "saved") setSaving(false);
    } catch (e) {
      setError(errorText(e));
      setSaving(false);
    }
  };

  return (
    <section aria-label="New agent" data-testid="agent-create" className="flex flex-col gap-4 p-4">
      <header className="flex flex-wrap items-center gap-2">
        <h2 className="text-base font-semibold">New agent</h2>
        <Button
          className="ml-auto"
          size="sm"
          variant="outline"
          disabled={describing}
          onClick={() => void describe()}
        >
          {describing ? <Loader2 className="animate-spin" /> : <Bot />}
          Describe it to an agent
        </Button>
      </header>
      <WhoCanUseItField value={where} onChange={setChoice} teams={audience} />
      <AgentFormBody
        draft={draft}
        onChange={setDraft}
        form={form}
        previewTeam={team ? undefined : activeTeam}
        catalogTeam={team ?? activeTeam ?? PERSONAL_SPACE}
        contentRoot={
          !team || (team.org === activeTeam?.org && team.team === activeTeam?.team)
            ? contentRoot
            : ""
        }
      />
      {invalid && <InvalidInput {...invalid} />}
      {error && (
        <p data-testid="agent-save-error" className="text-sm text-destructive">
          {error}
        </p>
      )}
      <div className="flex items-center gap-3">
        <span className="text-xs text-muted-foreground">
          Saved here as a file in the context repo; teammates get a team agent when you push.
        </span>
        <Button
          size="sm"
          className="ml-auto"
          disabled={!ready || saving}
          onClick={() => void save()}
        >
          {saving && <Loader2 className="animate-spin" />}
          Create agent
        </Button>
        <Button size="sm" variant="ghost" disabled={saving} onClick={cancel}>
          Cancel
        </Button>
      </div>
    </section>
  );
}
