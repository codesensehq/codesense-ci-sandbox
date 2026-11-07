// SPDX-License-Identifier: MIT
export interface ProviderOption {
  id: string;
  label: string;
}
const options: ProviderOption[] = [
  { id: "github", label: "GitHub" },
  { id: "gitlab", label: "GitLab" },
];
const labels = new Map(options.map(({ id, label }) => [id, label]));
/** Labels for manually recorded identities, not connected service accounts. */
export function useProviderCatalog() {
  return { options, labels };
}
