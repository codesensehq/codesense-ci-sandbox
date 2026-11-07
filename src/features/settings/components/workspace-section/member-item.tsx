// SPDX-License-Identifier: MIT
import { memo, useEffect, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { toast } from "sonner";
import type { Person } from "@/shared/types/domain";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import { useOrgStore } from "@/shared/stores/org-store";
import { PermissionGate } from "@/features/settings/components/permission-gate";
import { PersonAvatar } from "@/shared/components/person-avatar";
import { presentPerson } from "@/shared/lib/person-presentation";
import { commitRegistryEdit } from "@/shared/lib/commit-registry-edit";
import { validateGitHubUsername } from "@/shared/lib/github-username";
import type { useProviderCatalog } from "@/shared/hooks/use-provider-catalog";
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
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";

interface MemberItemProps {
  member: Person;
  orgSlug: string;
  isSelf: boolean;
  onRemoveClick: (member: Person) => void;
  providerCatalog: ReturnType<typeof useProviderCatalog>;
}

export const MemberItem = memo(function MemberItem({
  member,
  orgSlug,
  isSelf,
  onRemoveClick,
  providerCatalog,
}: MemberItemProps) {
  const [linkOpen, setLinkOpen] = useState(false);
  const [linking, setLinking] = useState(false);
  const [provider, setProvider] = useState("github");
  const [linkHandle, setLinkHandle] = useState("");
  const [linkError, setLinkError] = useState<string | null>(null);
  const currentUserId = useCurrentUser()?.github_user_id ?? null;
  const updatePersonInStore = useOrgStore((s) => s.updatePersonInStore);
  const linkGeneration = useRef(0);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      linkGeneration.current += 1;
    };
  }, []);
  const presentation = presentPerson(member, providerCatalog.labels);
  const githubHandleValidation = validateGitHubUsername(linkHandle);
  const invalidHandle =
    provider === "github" ? !githubHandleValidation.ok : linkHandle.trim().length === 0;
  // SW-FG-4b (AC-7): a handle-less teammate has no GitHub login — render them by
  // display name with no bogus `@` / `github.com/null` link. The "No code host
  // linked" affordance and the link-a-host flow are FG-4c.
  const handle = member.github_username;
  const githubUrl = handle ? `https://github.com/${handle}` : null;
  const rowLabel = presentation.chips.length === 0 && handle ? `@${handle}` : presentation.label;
  const testKey = handle ?? presentation.id;

  const copyUrl = async () => {
    if (!githubUrl) return;
    try {
      await navigator.clipboard.writeText(githubUrl);
      toast.success("GitHub URL copied");
    } catch {
      toast.error("Clipboard not available");
    }
  };

  const linkIdentity = async () => {
    if (currentUserId === null || linking || !provider.trim() || invalidHandle) return;
    setLinking(true);
    setLinkError(null);
    const generation = ++linkGeneration.current;
    try {
      const outcome = await commitRegistryEdit(currentUserId, orgSlug, {
        type: "AddIdentityToPerson",
        person_id: presentation.id,
        provider: provider.trim(),
        handle: linkHandle.trim(),
        provider_user_id: null,
      });
      if (!mounted.current || generation !== linkGeneration.current) return;
      if (outcome?.person) updatePersonInStore(orgSlug, outcome.person);
      setLinkOpen(false);
      setLinkHandle("");
      toast.success(`Linked @${linkHandle.trim()}`);
    } catch (error) {
      if (mounted.current && generation === linkGeneration.current) {
        const message = error instanceof Error ? error.message : String(error);
        setLinkError(message);
        toast.error(`Failed to link identity: ${message}`);
      }
    } finally {
      if (mounted.current && generation === linkGeneration.current) setLinking(false);
    }
  };

  return (
    <div role="listitem" className="relative" data-testid={`settings-member-pill-${testKey}`}>
      {/* SW-FX-79: the right-click menu now rides the shared portaled Radix
          `ContextMenu` instead of a hand-rolled `absolute top-full` panel +
          full-screen backdrop — so it escapes clipping and collides to stay
          on-screen. Radix `ContextMenu` is uncontrolled (its `Root` owns the
          open state), so selecting an item closes the menu; the async refresh
          reports via toast, and reopening mid-flight shows the in-item spinner. */}
      <div className="flex min-w-0 items-center gap-1.5">
        <ContextMenu>
          <ContextMenuTrigger asChild>
            <button
              type="button"
              onClick={() => {
                if (githubUrl) openUrl(githubUrl).catch(() => {});
              }}
              className="inline-flex items-center gap-1.5 rounded-full bg-muted px-2.5 py-1 text-xs hover:bg-accent"
            >
              <PersonAvatar person={member} login={member.github_username} size="md" />
              <span className="truncate">{rowLabel}</span>
              {presentation.secondaryHandle && rowLabel !== `@${presentation.secondaryHandle}` && (
                <span className="text-muted-foreground">@{presentation.secondaryHandle}</span>
              )}
            </button>
          </ContextMenuTrigger>
          <ContextMenuContent className="min-w-[180px] text-xs">
            {githubUrl && member.github_user_id !== null && (
              <ContextMenuItem onSelect={() => void copyUrl()}>Copy GitHub URL</ContextMenuItem>
            )}
            {presentation.linked && (
              <PermissionGate required="any-writer">
                <ContextMenuItem
                  onSelect={() => {
                    setLinkError(null);
                    setLinkOpen(true);
                  }}
                >
                  Add another provider
                </ContextMenuItem>
              </PermissionGate>
            )}
            <PermissionGate required="admin">
              {!isSelf && (
                <ContextMenuItem
                  data-testid={`settings-member-remove-${testKey}`}
                  variant="destructive"
                  onSelect={() => onRemoveClick(member)}
                >
                  Remove from workspace
                </ContextMenuItem>
              )}
            </PermissionGate>
          </ContextMenuContent>
        </ContextMenu>
        {presentation.chips.map((chip) => (
          <span
            key={chip}
            className="rounded border border-border px-1 text-[10px] text-muted-foreground"
          >
            {chip}
          </span>
        ))}
        {!presentation.linked && (
          <>
            <span className="text-[11px] text-muted-foreground">No code host linked</span>
            <PermissionGate required="any-writer">
              <button
                type="button"
                aria-label={`Link a code host for ${presentation.label}`}
                className="rounded px-1 text-[11px] font-medium hover:bg-accent focus-visible:outline focus-visible:outline-2"
                onClick={() => {
                  setLinkError(null);
                  setLinkOpen(true);
                }}
              >
                Link a code host
              </button>
            </PermissionGate>
          </>
        )}
      </div>
      <AlertDialog open={linkOpen} onOpenChange={(open) => !linking && setLinkOpen(open)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Link a code host for {presentation.label}</AlertDialogTitle>
            <AlertDialogDescription>
              Add the provider identity that belongs to this workspace person.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <label className="flex flex-col gap-1 text-xs">
            Provider
            <select
              value={provider}
              onChange={(event) => {
                setProvider(event.target.value);
                setLinkError(null);
              }}
              disabled={linking}
              aria-invalid={linkError !== null}
              aria-describedby={linkError ? "member-link-submit-error" : undefined}
              className="rounded-md border border-input bg-background px-3 py-2 text-sm"
            >
              {providerCatalog.options.map(({ id, label }) => (
                <option key={id} value={id}>
                  {label}
                </option>
              ))}
            </select>
          </label>
          <label className="flex flex-col gap-1 text-xs">
            Handle
            <input
              value={linkHandle}
              onChange={(event) => {
                setLinkHandle(event.target.value);
                setLinkError(null);
              }}
              disabled={linking}
              aria-invalid={linkError !== null}
              aria-describedby={linkError ? "member-link-submit-error" : undefined}
              className="rounded-md border border-input bg-background px-3 py-2 text-sm"
            />
          </label>
          {linkError && (
            <p
              id="member-link-submit-error"
              role="alert"
              className="text-xs text-destructive"
              data-testid="member-link-error"
            >
              Could not link this identity: {linkError}
            </p>
          )}
          <AlertDialogFooter>
            <AlertDialogCancel disabled={linking}>Cancel</AlertDialogCancel>
            <AlertDialogAction
              disabled={linking || !provider.trim() || invalidHandle}
              onClick={(event) => {
                event.preventDefault();
                void linkIdentity();
              }}
            >
              <span role={linking ? "status" : undefined} aria-live="polite">
                {linking ? "Linking…" : "Link"}
              </span>
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
});
