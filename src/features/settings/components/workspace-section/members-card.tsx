// SPDX-License-Identifier: MIT
import { useState } from "react";
import { Loader2, UserPlus } from "lucide-react";
import { toast } from "sonner";
import { VirtualList } from "@/shared/components/virtual-list";
import type { Org, Person } from "@/shared/types/domain";
import type { LocalEditOutcome, RegistryEdit } from "@/tauri/commands";
import { commitRegistryEdit } from "@/shared/lib/commit-registry-edit";
import { useSubmitAction } from "@/shared/hooks/use-submit-action";
import { rowMatches } from "@/features/settings/lib/row-matches";
import { useDialogTarget } from "@/features/settings/lib/use-dialog-target";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import { PermissionGate } from "@/features/settings/components/permission-gate";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { findRowSpec } from "./lib/workspace-row-specs";
import { SettingsCard } from "./settings-card";
import { MemberItem } from "./member-item";
import { personStableId } from "@/shared/lib/person-reference";
import { presentPerson } from "@/shared/lib/person-presentation";
import { useOrgStore } from "@/shared/stores/org-store";
import { useProviderCatalog } from "@/shared/hooks/use-provider-catalog";

/** Threshold above which the member list switches from flex-wrap pills to a
 *  vertical virtualized list. Below this count the pill layout is preserved
 *  for visual continuity; above it, virtualization avoids rendering hundreds
 *  of MemberItem instances at once. */
const VIRTUALIZATION_THRESHOLD = 30;
const MEMBER_ROW_HEIGHT = 32;

interface MembersCardProps {
  org: Org;
  query: string;
}

export function MembersCard({ org, query }: MembersCardProps) {
  const spec = findRowSpec("members");
  const removeDialog = useDialogTarget<Person>();
  const [addOpen, setAddOpen] = useState(false);
  const providerCatalog = useProviderCatalog();

  const currentUserId = useCurrentUser()?.github_user_id ?? null;

  if (!rowMatches(spec, query)) return null;

  const members = org.people;
  const count = members.length;

  // SW-ID-2: add a person to the roster. Not an invitation — resolving the
  // username to a GitHub profile and appending to `people.yml` (local-only,
  // no commit — SW-ID-8) simply makes them assignable under open access
  // (SW-OB-7).
  const addButton = (
    <PermissionGate required="any-writer">
      <button
        type="button"
        onClick={() => setAddOpen(true)}
        data-testid="settings-members-add-button"
        className="inline-flex items-center gap-1 rounded border border-border bg-background px-2 py-1 text-xs hover:bg-accent"
      >
        <UserPlus className="h-3.5 w-3.5" aria-hidden="true" />
        Add
      </button>
    </PermissionGate>
  );

  return (
    <SettingsCard
      title="Members"
      count={count}
      headerAction={addButton}
      data-testid="settings-card-members"
    >
      {count === 0 && (
        <p className="text-xs text-muted-foreground" data-testid="settings-members-empty">
          No members yet. Add your first teammate to make them assignable.
        </p>
      )}
      {count > 0 && count <= VIRTUALIZATION_THRESHOLD && (
        <div
          role="list"
          className="flex flex-wrap gap-1.5"
          data-testid="settings-members-list"
          aria-label="Workspace members"
        >
          {members.map((m) => (
            <MemberItem
              key={personStableId(m)}
              member={m}
              orgSlug={org.slug}
              isSelf={currentUserId === m.github_user_id}
              onRemoveClick={removeDialog.show}
              providerCatalog={providerCatalog}
            />
          ))}
        </div>
      )}
      {count > VIRTUALIZATION_THRESHOLD && (
        <VirtualizedMemberList
          members={members}
          orgSlug={org.slug}
          currentUserId={currentUserId}
          onRemoveClick={removeDialog.show}
          providerCatalog={providerCatalog}
        />
      )}

      <RemoveMemberDialog org={org} target={removeDialog.target} onClose={removeDialog.clear} />
      <AddMemberDialog org={org} open={addOpen} onClose={() => setAddOpen(false)} />
    </SettingsCard>
  );
}

// ---------- Shared commit orchestration ----------

/**
 * Submit orchestration shared by the members-pane registry-edit dialogs
 * (add / remove). Owns the current-user lookup, the in-flight `submitting`
 * flag, the not-signed-in guard, and the success/error toasts — so neither
 * dialog re-implements the commit dance. `onDone` fires after a successful
 * commit (or the not-signed-in bail) so the caller can close/reset.
 */
function useRegistryEditSubmit(orgSlug: string) {
  const githubUserId = useCurrentUser()?.github_user_id ?? null;
  // SW-GT-6 (T8): the in-flight flag + success/error toast + try/catch now live
  // in the shared `useSubmitAction`. This hook keeps only the members-pane
  // specifics: the not-signed-in guard and the `commitRegistryEdit` call.
  const { submitting, submit: submitAction } = useSubmitAction();

  const submit = async (
    edit: RegistryEdit,
    opts: {
      success: string;
      errorPrefix: string;
      onDone: () => void;
      onSuccess?: (outcome: LocalEditOutcome) => void;
      onError?: (message: string) => void;
    },
  ) => {
    if (submitting) return;
    if (githubUserId === null) {
      toast.error("Not signed in");
      opts.onDone();
      return;
    }
    let outcome: LocalEditOutcome | null = null;
    await submitAction(
      // SW-OB-8: local-only commit regardless of `org.remote_url`.
      async () => {
        outcome = (await commitRegistryEdit(githubUserId, orgSlug, edit)) ?? null;
      },
      {
        ...opts,
        onDone: () => {
          if (outcome) opts.onSuccess?.(outcome);
          opts.onDone();
        },
      },
    );
  };

  return { submitting, submit };
}

// ---------- Add person ----------

function AddMemberDialog({ org, open, onClose }: { org: Org; open: boolean; onClose: () => void }) {
  const { submitting, submit } = useRegistryEditSubmit(org.slug);
  const [value, setValue] = useState("");
  const [submitError, setSubmitError] = useState<string | null>(null);
  const addPersonToStore = useOrgStore((state) => state.addPersonToStore);

  const name = value.trim();
  const nameValid =
    name.length > 0 && [...name].length <= 100 && ![...name].some((char) => /\p{Cc}/u.test(char));
  const canSubmit = !submitting && nameValid;

  // Inline hint for a typed-but-invalid name; an untouched field is the resting state.
  let hint: string | null = null;
  if (value.length > 0 && !nameValid) {
    hint =
      name.length === 0
        ? "Enter a name."
        : [...name].length > 100
          ? "Names are at most 100 characters."
          : "Names cannot contain control characters.";
  }

  const close = () => {
    if (submitting) return;
    setValue("");
    setSubmitError(null);
    onClose();
  };

  const onConfirm = () => {
    setSubmitError(null);
    if (!nameValid) return;
    submit(
      { type: "AddPersonByName", display_name: name },
      {
        success: `Added ${name}`,
        errorPrefix: `Failed to add ${name}`,
        onDone: () => {},
        onSuccess: (outcome) => {
          if (outcome.person) addPersonToStore(org.slug, outcome.person);
          setValue("");
          onClose();
        },
        onError: setSubmitError,
      },
    );
  };

  return (
    <AlertDialog open={open} onOpenChange={(o) => !o && close()}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Add a person to the workspace</AlertDialogTitle>
          <AlertDialogDescription>
            Add someone by name. You can record their service identities manually later.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <div className="flex flex-col gap-1">
          <input
            type="text"
            autoFocus
            value={value}
            onChange={(e) => {
              setValue(e.target.value);
              setSubmitError(null);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" && canSubmit) {
                e.preventDefault();
                onConfirm();
              }
            }}
            disabled={submitting}
            placeholder="Teammate name"
            aria-label="Teammate name"
            aria-invalid={hint !== null || submitError !== null}
            aria-describedby={
              hint ? "members-add-validation" : submitError ? "members-add-submit-error" : undefined
            }
            data-testid="settings-members-add-input"
            className="h-7 w-full rounded border border-border bg-background px-2 text-xs outline-none focus:border-ring disabled:opacity-60"
          />
          {hint && (
            <p
              id="members-add-validation"
              className="text-xs text-destructive"
              data-testid="settings-members-add-invalid"
              aria-live="polite"
            >
              {hint}
            </p>
          )}
          {submitError && (
            <p
              id="members-add-submit-error"
              role="alert"
              className="text-xs text-destructive"
              data-testid="settings-members-add-error"
            >
              Could not add this person: {submitError}
            </p>
          )}
        </div>
        <AlertDialogFooter>
          <AlertDialogCancel onClick={close} disabled={submitting}>
            Cancel
          </AlertDialogCancel>
          <AlertDialogAction
            onClick={(e) => {
              // Keep the dialog open through the async resolve; AlertDialogAction
              // closes on click by default.
              e.preventDefault();
              onConfirm();
            }}
            disabled={!canSubmit}
            data-testid="settings-members-add-confirm"
          >
            {submitting ? (
              <span className="inline-flex items-center gap-1.5" role="status" aria-live="polite">
                <Loader2
                  className="h-3.5 w-3.5 animate-spin motion-reduce:animate-none"
                  aria-hidden="true"
                />
                Adding&hellip;
              </span>
            ) : (
              "Add"
            )}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

// ---------- Virtualized list (above threshold) ----------

function VirtualizedMemberList({
  members,
  orgSlug,
  currentUserId,
  onRemoveClick,
  providerCatalog,
}: {
  members: Person[];
  orgSlug: string;
  currentUserId: number | null;
  onRemoveClick: (member: Person) => void;
  providerCatalog: ReturnType<typeof useProviderCatalog>;
}) {
  return (
    <VirtualList<Person>
      items={members}
      estimateSize={MEMBER_ROW_HEIGHT}
      getKey={(m) => personStableId(m)}
      overscan={8}
      className="max-h-[300px] overflow-y-auto"
      data-testid="settings-members-list"
      aria-label="Workspace members"
      renderItem={(m) => (
        <MemberItem
          member={m}
          orgSlug={orgSlug}
          isSelf={currentUserId === m.github_user_id}
          onRemoveClick={onRemoveClick}
          providerCatalog={providerCatalog}
        />
      )}
    />
  );
}

// ---------- Remove confirmation ----------

function RemoveMemberDialog({
  org,
  target,
  onClose,
}: {
  org: Org;
  target: Person | null;
  onClose: () => void;
}) {
  const { submitting, submit } = useRegistryEditSubmit(org.slug);
  const open = target !== null;

  const onConfirm = () => {
    if (!target) return;
    submit(
      { type: "RemovePersonById", person_id: personStableId(target) },
      {
        success: `Removed ${presentPerson(target).label}`,
        errorPrefix: `Failed to remove ${presentPerson(target).label}`,
        onDone: onClose,
      },
    );
  };

  return (
    <AlertDialog open={open} onOpenChange={(o) => !o && !submitting && onClose()}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>
            Remove {target ? presentPerson(target).label : "this person"} from workspace?
          </AlertDialogTitle>
          <AlertDialogDescription>
            They&rsquo;ll be removed from <code>people.yml</code>.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel onClick={onClose} disabled={submitting}>
            Cancel
          </AlertDialogCancel>
          <AlertDialogAction
            onClick={onConfirm}
            disabled={submitting}
            data-testid="settings-member-remove-confirm"
          >
            {submitting ? "Removing\u2026" : "Remove"}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
