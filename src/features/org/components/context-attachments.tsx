// SPDX-License-Identifier: MIT
import {
  beginLocalSetup,
  endLocalSetup,
  useLocalSetupActivity,
} from "../lib/local-setup-completion";
import { ContextRepoFlow, IDLE_STEP, type ContextRepoStep } from "./context-repo-flow";
import { getCurrentUser, useCurrentUser } from "@/shared/lib/get-current-user";
import { isCodesenseDocs } from "@/shared/lib/codesense-docs";
import { CONTEXT_ATTACHMENTS_REFRESH } from "../lib/context-attachment-events";
import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { AppButton } from "@/shared/components/app-button";
import { detachContextRepository, listContextAttachments } from "@/tauri/commands";
import { useOrgStore } from "@/shared/stores/org-store";
import type { OrgSummary } from "@/shared/types/domain";

/**
 * Connect or create a context repo, and the connected ones (SW-OC-16 copy). Connecting
 * changes only the registration; the checkout stays user-owned.
 */
export function ContextAttachments({
  onAttached,
  initialCreate = false,
}: {
  onAttached?: () => void;
  initialCreate?: boolean;
}) {
  const user = useCurrentUser();
  const principal = user?.principal ?? String(user?.github_user_id ?? "signed-out");
  // Account changes retire the entire request/state lifetime, including stale rows.
  return (
    <PrincipalContextAttachments
      key={principal}
      onAttached={onAttached}
      initialCreate={initialCreate}
    />
  );
}

function PrincipalContextAttachments({
  onAttached,
  initialCreate,
}: {
  onAttached?: () => void;
  initialCreate: boolean;
}) {
  const [step, setStep] = useState<ContextRepoStep>(initialCreate ? { kind: "create" } : IDLE_STEP);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const [attachments, setAttachments] = useState<OrgSummary[]>([]);
  const [error, setError] = useState<string | null>(null);
  const busy = useLocalSetupActivity((s) => s.busy);
  useEffect(() => {
    let live = true;
    let generation = 0;
    const refresh = () => {
      const request = ++generation;
      return listContextAttachments().then(
        (rows) => {
          if (live && request === generation) {
            setAttachments(rows);
            setError(null);
          }
        },
        (e: unknown) => {
          if (live && request === generation) setError(String(e));
        },
      );
    };
    window.addEventListener(CONTEXT_ATTACHMENTS_REFRESH, refresh);
    void refresh();
    const subscription = listen("context-attachments-changed", refresh).catch((e: unknown) => {
      if (live) setError(String(e));
      return () => {};
    });
    return () => {
      live = false;
      window.removeEventListener(CONTEXT_ATTACHMENTS_REFRESH, refresh);
      void subscription.then((unlisten) => unlisten());
    };
  }, []);

  async function disconnect(slug: string) {
    if (!beginLocalSetup()) return;
    const principal = getCurrentUser()?.principal;
    setError(null);
    try {
      await detachContextRepository(slug);
      if (getCurrentUser()?.principal !== principal || !mounted.current) return;
      const store = useOrgStore.getState();
      store.setOrgs(store.orgs.filter((org) => org.slug !== slug));
      setAttachments(await listContextAttachments());
    } catch (e) {
      if (mounted.current) setError(String(e));
    } finally {
      endLocalSetup();
    }
  }
  // The pinned CodeSense docs are always connected; they are not listed here.
  const connected = attachments.filter((org) => !isCodesenseDocs(org.slug));
  return (
    <section className="my-4 space-y-3" aria-label="Context repos">
      <ContextRepoFlow
        variant="buttons"
        step={step}
        setStep={setStep}
        onDone={() => mounted.current && onAttached?.()}
      />
      <p className="text-xs text-muted-foreground">
        No account needed. Your files stay in the folder you choose; you or your agents run git.
      </p>
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
      {connected.length > 0 && (
        <ul className="space-y-1.5" aria-label="Connected context repos">
          {connected.map((org) => (
            <li key={org.slug} className="rounded-md border border-border p-2 text-sm">
              <p className="font-medium">{org.display_name}</p>
              <p className="font-mono text-xs break-all text-muted-foreground">{org.org_root}</p>
              {org.availability_error && (
                <p role="alert" className="text-xs text-destructive">
                  {org.availability_error}
                </p>
              )}
              <div className="mt-1 flex items-center gap-2">
                <AppButton
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  onClick={() => void disconnect(org.slug)}
                >
                  Disconnect {org.display_name}
                </AppButton>
                <span className="text-xs text-muted-foreground">Keeps every file.</span>
              </div>
            </li>
          ))}
        </ul>
      )}
      <AppButton
        variant="ghost"
        size="sm"
        disabled={busy}
        onClick={() => window.dispatchEvent(new Event(CONTEXT_ATTACHMENTS_REFRESH))}
      >
        Refresh context repos
      </AppButton>
    </section>
  );
}
