// SPDX-License-Identifier: MIT
import type { Org } from "@/shared/types/domain";
import { rowMatches } from "@/features/settings/lib/row-matches";
import { SettingsCard } from "./settings-card";
import { ConfigRows } from "./config-rows";
import { findRowSpec } from "./lib/workspace-row-specs";

interface ConfigCardProps {
  org: Org;
  query: string;
}

// SW-OB-8: `branch-protection` is a remote-git concern that goes dark
// under local-only (the notice's admin gate never fires now that the
// permission fetch is retired), so it's dropped from the config search
// specs and the card no longer renders BranchProtectionNotice.
const CONFIG_SPECS = [findRowSpec("workspace-name"), findRowSpec("workspace-remote")];

export function ConfigCard({ org, query }: ConfigCardProps) {
  if (query && !CONFIG_SPECS.some((spec) => rowMatches(spec, query))) return null;

  return (
    <SettingsCard title="Config" data-testid="settings-card-config">
      <ConfigRows org={org} query={query} />
    </SettingsCard>
  );
}
