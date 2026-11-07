// SPDX-License-Identifier: MIT
import type { Person } from "@/shared/types/domain";
import { personStableId } from "@/shared/lib/person-reference";

export interface PersonPresentation {
  id: string;
  label: string;
  handle: string | null;
  secondaryHandle: string | null;
  linked: boolean;
  chips: string[];
}

function providerKey(provider: string): string {
  return provider.trim().toLowerCase();
}

const BUILTIN_PROVIDER_LABELS = new Map([
  ["github", "GitHub"],
  ["gitlab", "GitLab"],
]);

export function presentPerson(
  person: Person,
  providerLabels: ReadonlyMap<string, string> = new Map(),
  defaultProvider = "github",
): PersonPresentation {
  const identities = person.identities ?? [];
  const handled = identities.filter((identity) => identity.handle?.trim());
  const providers = [...new Set(handled.map((identity) => providerKey(identity.provider)))].filter(
    Boolean,
  );
  const defaultProviderKey = providerKey(defaultProvider);
  const normalizedLabels = new Map(
    [...providerLabels].map(([provider, label]) => [providerKey(provider), label]),
  );
  for (const [provider, label] of BUILTIN_PROVIDER_LABELS) {
    if (!normalizedLabels.has(provider)) normalizedLabels.set(provider, label);
  }
  const descriptorOrder = new Map(
    [...normalizedLabels.keys()].map((provider, index) => [provider, index]),
  );
  providers.sort(
    (left, right) =>
      (descriptorOrder.get(left) ?? Number.MAX_SAFE_INTEGER) -
        (descriptorOrder.get(right) ?? Number.MAX_SAFE_INTEGER) || left.localeCompare(right),
  );
  const chips =
    providers.length === 1 && providers[0] === defaultProviderKey
      ? []
      : providers.map(
          (provider) =>
            normalizedLabels.get(provider) ??
            provider
              .split(/[-_]/)
              .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
              .join(" "),
        );
  const handle =
    handled
      .find((identity) => providerKey(identity.provider) === defaultProviderKey)
      ?.handle?.trim() ??
    handled[0]?.handle?.trim() ??
    person.github_username?.trim() ??
    null;
  const label = person.display_name.trim() || (handle ? `@${handle}` : personStableId(person));
  return {
    id: personStableId(person),
    label,
    handle,
    secondaryHandle: handle && label.toLowerCase() !== `@${handle}`.toLowerCase() ? handle : null,
    linked: handled.length > 0 || !!person.github_username?.trim(),
    chips,
  };
}
