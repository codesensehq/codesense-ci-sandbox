// SPDX-License-Identifier: MIT
import type { DashboardRef } from "@/tauri/commands";

export function dashboardRefKey(ref: DashboardRef): string {
  const team = ref.scope === "team" ? `:${ref.team}` : "";
  return `dashboard:${ref.scope}:${ref.org}${team}/${ref.id}`;
}
