// SPDX-License-Identifier: MIT
import { useRoleQueues } from "@/shared/hooks/use-role-queues";
import { useEffect, useRef, useState } from "react";
import { roleSave, type RoleConfig, type RoleQueue } from "@/tauri/commands";
import { cn } from "@/lib/utils";
import { resolveAgentFace } from "@/features/viewer/lib/agent-list-view";
import {
  useTeamAgentIdentities,
  type TeamAgentIdentity,
} from "@/features/viewer/hooks/use-team-agent-identities";
import type { FaceSource } from "@/shared/lib/avatar-packs";
import { NO_TEAM_SENTINEL, useActiveTeamKey } from "@/shared/lib/team-key";
import { useRoleConfigs, reloadRoleConfigs } from "@/shared/hooks/use-role-configs";
import { useTeamAvatarPacks } from "@/shared/hooks/use-avatar-packs";
import { rolePresentation } from "@/shared/lib/role-presentation";
import { QueueRole } from "@/shared/components/queue-role";
import { AgentFaceView } from "@/features/viewer/components/agent-identity";
import { ConfirmActionDialog } from "@/features/settings/components/confirm-action-dialog";
import { InlineAlert } from "@/components/ui/inline-alert";
import { Field } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { AppButton as Button } from "@/shared/components/app-button";
import { EmptyState } from "@/shared/components/empty-state";
import { Inbox, Info } from "lucide-react";
import { FacePicker } from "./face-picker";
import { SELECT_TRIGGER } from "./agent-form-fields";
import { PortaledSelect } from "@/components/ui/portaled-select";
import { useAgentsStore } from "../stores/agents-store";
import { useRoleEditorStore } from "../stores/role-editor-store";

export function RoleLeaveGuard() {
  const pending = useRoleEditorStore((s) => s.pendingLeave);
  const confirm = useRoleEditorStore((s) => s.confirmLeave);
  const cancel = useRoleEditorStore((s) => s.cancelLeave);
  return (
    <ConfirmActionDialog
      open={pending !== null}
      onOpenChange={(open) => {
        if (!open) cancel();
      }}
      title="Discard unsaved changes?"
      description="This role has edits that aren't saved. Leaving drops them."
      confirmLabel="Discard"
      loadingLabel="Discarding…"
      loading={false}
      onConfirm={confirm}
      testId="role-discard-confirm"
    />
  );
}

export function RolesView() {
  const active = useActiveTeamKey();
  const selectedTeam = useRoleEditorStore((s) => s.teamKey);
  const selected = useRoleEditorStore((s) => s.role);
  const [displayedTeam, setDisplayedTeam] = useState(active);
  const teamKey = selectedTeam ?? displayedTeam;
  const previousActive = useRef(active);
  useEffect(() => {
    if (previousActive.current === active) return;
    previousActive.current = active;
    useRoleEditorStore.getState().requestLeave(() => {
      setDisplayedTeam(active);
      useRoleEditorStore.setState({ teamKey: null, role: null });
    });
  }, [active]);
  const { roles, error } = useRoleConfigs(teamKey);
  const { queues, error: queueError } = useRoleQueues(teamKey);
  const [newId, setNewId] = useState("");
  const unconfigured = queues.filter(
    (q) =>
      !roles.some((r) => r.id === q.role) &&
      (q.registrations.some((r) => r.live) ||
        q.counts.pending + q.counts.claimed + q.counts.accepted + q.counts.acknowledged > 0),
  );
  const chosen = selected ?? roles[0]?.id ?? unconfigured[0]?.role ?? null;
  const select = (role: string) => {
    if (role === chosen) return;
    useRoleEditorStore
      .getState()
      .requestLeave(() => useRoleEditorStore.setState({ role, teamKey }));
  };
  const agents = useAgentsStore((s) => s.rows) ?? [];
  const avatars = useTeamAvatarPacks([teamKey])(teamKey);
  const identities = useTeamAgentIdentities(teamKey);
  if (teamKey === NO_TEAM_SENTINEL)
    return (
      <EmptyState
        className="flex-1 pt-20"
        icon={<Inbox />}
        title="Choose a team to manage its roles."
      />
    );
  return (
    <div className="flex min-h-0 flex-1" data-testid="roles-view">
      <div className="w-[22rem] shrink-0 overflow-y-auto border-r border-border">
        <form
          className="flex items-center gap-2 border-b border-border/70 px-3 py-2.5"
          onSubmit={(e) => {
            e.preventDefault();
            if (newId.trim() && newId.trim() !== "human") {
              select(newId.trim().replace(/^role:/, ""));
              setNewId("");
            }
          }}
        >
          <Input
            className="h-8 min-w-0"
            aria-label="New queue id"
            placeholder="New queue id"
            value={newId}
            onChange={(e) => setNewId(e.target.value)}
          />
          <Button type="submit" size="sm" variant="outline" disabled={!newId.trim()}>
            Add
          </Button>
        </form>
        {error && (
          <InlineAlert severity="error" className="m-2">
            Couldn&apos;t read roles: {error}
          </InlineAlert>
        )}
        {queueError && (
          <InlineAlert severity="error" className="m-2">
            Couldn&apos;t read queues: {queueError}
          </InlineAlert>
        )}
        {[
          { title: "Set up", ids: roles.map((r) => r.id) },
          { title: "Not set up", ids: unconfigured.map((q) => q.role) },
        ].map((group) => (
          <section key={group.title}>
            <h3 className="flex h-[30px] items-center gap-1.5 px-3.5 pt-1.5 text-xs font-semibold text-muted-foreground">
              {group.title}
              <span className="ml-auto font-medium tabular-nums">{group.ids.length}</span>
            </h3>
            {group.ids.map((id) => {
              const q = queues.find((q) => q.role === id);
              const config = roles.find((r) => r.id === id);
              const missing =
                config?.defaultHandler && !agents.some((a) => a.key === config.defaultHandler);
              return (
                <button
                  type="button"
                  key={id}
                  aria-current={chosen === id}
                  onClick={() => select(id)}
                  className={`mx-1.5 flex w-[calc(100%-0.75rem)] flex-col gap-0.5 rounded-lg px-2.5 py-2 text-left outline-none hover:bg-muted/55 focus-visible:ring-2 focus-visible:ring-ring ${chosen === id ? "bg-muted" : ""}`}
                >
                  <span className="flex items-center gap-2">
                    <QueueRole role={id} teamKey={teamKey} size={22} />
                    <HolderFaces
                      holders={liveHolders(q)}
                      faces={avatars.faces}
                      identities={identities}
                    />
                  </span>
                  <span className="text-xs text-muted-foreground">
                    {queueSummary(q)}
                    {missing && " · Handler not found"}
                  </span>
                </button>
              );
            })}
          </section>
        ))}
      </div>
      <div className="min-w-0 flex-1 overflow-y-auto">
        {chosen ? (
          <RoleEditor
            key={`${teamKey}/${chosen}`}
            id={chosen}
            teamKey={teamKey}
            config={roles.find((r) => r.id === chosen)}
            queue={queues.find((q) => q.role === chosen)}
          />
        ) : (
          <EmptyState
            className="pt-20"
            icon={<Inbox />}
            title="No roles yet"
            body="Add a role to give its queue a name, icon and default handler."
          />
        )}
      </div>
    </div>
  );
}

type Holder = RoleQueue["registrations"][number];

const liveHolders = (queue: RoleQueue | undefined): Holder[] =>
  (queue?.registrations ?? []).filter((r) => r.live);

/** Who holds a queue and what waits in it, in words: "2 hold it now · 1 message waiting". */
function queueSummary(queue: RoleQueue | undefined): string {
  const n = liveHolders(queue).length;
  const waiting = queue?.counts.pending ?? 0;
  const holders = n === 0 ? "No one holds it" : n === 1 ? "1 holds it now" : `${n} hold it now`;
  const messages = waiting === 1 ? "1 message waiting" : `${waiting} messages waiting`;
  return `${holders} · ${messages}`;
}

/** The faces of the agents holding a queue, up to three, each cut out of the one beside it. */
function HolderFaces({
  holders,
  faces,
  identities,
}: {
  holders: readonly Holder[];
  faces: FaceSource;
  identities: readonly TeamAgentIdentity[];
}) {
  if (holders.length === 0) return null;
  return (
    <span className="flex">
      {holders.slice(0, 3).map((h, i) => {
        const name = h.instanceName ?? h.instanceId;
        // The face its session wears in the agents list (its picked face, its saved agent's, the
        // one dealt to it); only a holder the list doesn't show falls back to one from its name.
        const face =
          identities.find(
            (i) =>
              i.row.handle === h.instanceId || (!!h.sessionId && i.row.sessionId === h.sessionId),
          )?.face ??
          resolveAgentFace(
            { avatar: null, agentRevision: null, handle: h.instanceId, displayName: name },
            { name, faces },
          );
        return (
          <span
            key={h.instanceId}
            data-testid="role-holder-face"
            title={name}
            className={cn("rounded-full ring-2 ring-background", i > 0 && "-ml-1.5")}
          >
            <AgentFaceView face={face} size={20} />
          </span>
        );
      })}
    </span>
  );
}

function RoleEditor({
  id,
  teamKey,
  config,
  queue,
}: {
  id: string;
  teamKey: string;
  config?: RoleConfig;
  queue?: RoleQueue;
}) {
  const base = config ?? { id, name: id, icon: null, defaultHandler: null };
  const [draft, setDraft] = useState<RoleConfig>(base);
  const [saved, setSaved] = useState<RoleConfig>(base);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const avatars = useTeamAvatarPacks([teamKey])(teamKey);
  const rows = useAgentsStore((s) => s.rows);
  const agents = (rows ?? []).filter(
    (a) =>
      !a.archived &&
      (a.scope === "builtin" ||
        (a.org === teamKey.split(":")[0] &&
          (a.scope === "personal" || a.team === teamKey.split(":")[1]))),
  );
  const changed = JSON.stringify(draft) !== JSON.stringify(saved);
  useEffect(() => {
    useRoleEditorStore.setState({ dirty: changed });
    return () => useRoleEditorStore.setState({ dirty: false });
  }, [changed]);
  const dirty = useRoleEditorStore((s) => s.dirty);
  useEffect(() => {
    if (!dirty) setDraft(saved);
  }, [dirty, saved]);
  const lastConfig = useRef(config);
  useEffect(() => {
    if (config === lastConfig.current) return;
    lastConfig.current = config;
    if (!changed && config) {
      setSaved(config);
      setDraft(config);
    }
  }, [config, changed]);
  const presentation = rolePresentation(id, [draft], avatars.faces);
  const auto = rolePresentation(id, [], avatars.faces);
  const handler = agents.find((a) => a.key === draft.defaultHandler);
  const missing = draft.defaultHandler && rows !== null && !handler;
  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      const result = await roleSave(teamKey, draft);
      setSaved(result);
      setDraft(result);
      await reloadRoleConfigs(teamKey);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };
  return (
    <div className="flex max-w-3xl flex-col gap-5 p-6" data-testid="role-editor">
      <p className="flex items-start gap-2.5 rounded-lg bg-muted px-3 py-2.5 text-[13px] leading-snug">
        <Info aria-hidden="true" className="mt-0.5 size-3.5 shrink-0 text-muted-foreground" />A role
        is a shared inbox an agent can hold. Messages sent to the role reach whoever holds it, and
        wait in the queue when no one does.
      </p>
      <header className="flex items-center gap-3">
        <AgentFaceView
          face={{ kind: "image", ref: presentation.ref, src: presentation.src }}
          size={56}
        />
        <div>
          <h2 className="text-lg font-semibold tracking-tight">{draft.name}</h2>
          <p className="mt-0.5 text-xs text-muted-foreground">
            {teamKey} · {queueSummary(queue)}
          </p>
        </div>
      </header>
      <Field
        label="Name"
        hint={`Renaming keeps the queue id (${id}), so mail and holders stay put.`}
      >
        <Input value={draft.name} onChange={(e) => setDraft({ ...draft, name: e.target.value })} />
      </Field>
      <section>
        <h3 className="mb-2 text-sm font-medium">Icon</h3>
        <div className="max-h-72 overflow-y-auto rounded-lg border border-border py-3">
          <FacePicker
            compact
            packs={[...avatars.packs].sort(
              (a, b) => Number(b.id === "builtin-chess") - Number(a.id === "builtin-chess"),
            )}
            value={draft.icon}
            autoLabel="Default"
            autoDescription="The piece picked from this queue id"
            autoFace={{ kind: "image", ref: auto.ref, src: auto.src }}
            onChange={(icon) => setDraft({ ...draft, icon })}
          />
        </div>
      </section>
      <label className="flex flex-col gap-2 text-xs text-muted-foreground">
        Default handler
        {handler?.avatar && avatars.faces.resolve(handler.avatar) && (
          <AgentFaceView
            face={{
              kind: "image",
              ref: handler.avatar,
              src: avatars.faces.resolve(handler.avatar)!,
            }}
            size={30}
          />
        )}
        <PortaledSelect
          ariaLabel="Default handler"
          triggerClassName={SELECT_TRIGGER}
          value={draft.defaultHandler ?? ""}
          onValueChange={(v) => setDraft({ ...draft, defaultHandler: v || null })}
          options={[
            { value: "", label: "None" },
            ...(missing
              ? [
                  {
                    value: draft.defaultHandler!,
                    label: `Handler not found · ${draft.defaultHandler}`,
                  },
                ]
              : []),
            ...agents.map((a) => ({ value: a.key, label: a.name })),
          ]}
        />
        {missing && (
          <span role="status" className="text-warning-ink">
            Handler not found · {draft.defaultHandler} was deleted or isn&apos;t in this team.
            Nothing starts for this queue until you pick one.
          </span>
        )}
        <span>
          The saved agent a routine may start for this queue. Anyone can still hold the role.
        </span>
      </label>
      {error && <InlineAlert severity="error" title="Couldn't save role" error={error} />}
      <footer className="flex gap-2">
        <Button
          disabled={saving || !draft.name.trim() || (!changed && !!config)}
          onClick={() => void save()}
        >
          {saving ? "Saving…" : "Save role"}
        </Button>
        <Button variant="outline" disabled={saving || !changed} onClick={() => setDraft(saved)}>
          Revert
        </Button>
      </footer>
    </div>
  );
}
