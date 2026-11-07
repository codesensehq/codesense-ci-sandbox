// SPDX-License-Identifier: MIT
import type { SettingRowSpec } from "@/features/settings/lib/row-matches";
import { useLocalProfileStore } from "@/shared/stores/local-profile-store";

export const ACCOUNT_SECTION_SPEC: SettingRowSpec = {
  key: "account",
  label: "Local profile",
  keywords: ["local", "profile", "account", "identity"],
};

/** Local namespace selection is independent of agent login and machine enrollment. */
export function AccountSection({ query = "" }: { query?: string }) {
  const status = useLocalProfileStore((s) => s.status);
  const busy = useLocalProfileStore((s) => s.busy);
  const error = useLocalProfileStore((s) => s.error);
  const select = useLocalProfileStore((s) => s.select);
  const initialize = useLocalProfileStore((s) => s.initialize);
  const activeProfile = status?.active ?? null;
  if (query && !"local profile account identity".includes(query.toLowerCase())) return null;
  return (
    <section className="space-y-3 p-4">
      <h1 className="text-lg font-semibold">Local profile</h1>
      <p className="text-sm text-muted-foreground">
        Choose whose local workspace to open. Profiles keep their files and routines separate. Your
        agent manages service login.
      </p>
      {status?.metadataRecovery?.error && (
        <p role="status" className="text-sm text-muted-foreground">
          Local files remain available. Historical machine access metadata could not be recovered:{" "}
          {status.metadataRecovery.error}
          {status.metadataRecovery.pending ? " CodeSense will retry on a later startup." : ""}
        </p>
      )}
      {activeProfile && activeProfile.machineOwner === null && (
        <div className="space-y-2">
          <p className="text-sm text-muted-foreground">
            Set up this local profile to manage machine access. Existing machine ownership checks
            still apply.
          </p>
          <button
            type="button"
            disabled={busy}
            onClick={() => void select(activeProfile.principal)}
          >
            Set up machine access
          </button>
        </div>
      )}
      {error && (
        <div role="alert">
          <p>{error}</p>
          <button onClick={() => void initialize()}>Retry</button>
        </div>
      )}
      {status?.profiles.map((profile) => (
        <button
          key={profile.principal}
          type="button"
          disabled={busy || status.active?.principal === profile.principal}
          onClick={() => void select(profile.principal)}
          className="block rounded border px-3 py-2"
        >
          {profile.displayName}{" "}
          <span className="text-xs text-muted-foreground">({profile.principal})</span>
          {status.active?.principal === profile.principal ? " — Selected" : ""}
        </button>
      ))}
    </section>
  );
}
