// SPDX-License-Identifier: MIT
export interface Viewer {
  id?: string | null;
  sessions?: number | null;
  last_seen_at?: number | null;
  email: string;
  invite: "drafted" | "sent" | "waiting_limit" | "suppressed" | "none" | "unknown" | null;
  sent_at: number | null;
  added_at?: number | null;
  /** Opened this dashboard through their CodeSense account at least once. Never "an account exists". */
  joined?: boolean | null;
}
export interface HostedDashboard {
  session_days?: number | null;
  slug: string;
  title: string;
  state: "draft" | "published" | "unpublished";
  /** `team` is reserved and is currently the same as `private`. */
  visibility: "private" | "team";
  current_version: number | null;
  pushed_at: string | null;
  checked_at: string | null;
  share_count: number;
  shares: Viewer[];
  live_queries: unknown;
  live_streams: unknown;
  live_control: string | null;
  live_actions: unknown;
}
export interface InventoryRow {
  slug: string;
  title: string;
  folder: string | null;
  location: "local" | "hosted" | "both";
  localState: "draft" | "publishing" | "published" | "unpublished" | null;
  hosted: HostedDashboard | null;
  pushedAt: string | null;
  checkedAt: string | null;
  error: string | null;
  differentKey: boolean;
  team: InventoryTeam | null;
}
export interface InventoryTeam {
  org: string;
  team: string;
  sharedFrom: string | null;
  relativeFolder: string;
}
export interface DashboardInventory {
  host: string | null;
  source: "local" | "hosted" | "unsupported" | "unavailable" | "refused" | "stale";
  checkedAt: string;
  error: string | null;
  rows: InventoryRow[];
  teams: InventoryTeamHeader[];
}
export interface InventoryTeamHeader {
  org: string;
  team: string;
}
export interface DashboardDetail {
  dashboard: HostedDashboard;
  capabilities: { label: string; available: boolean; detail: string }[];
}
export interface PublicationStatus {
  slug: string;
  hostedCopy: "live" | "removed" | "retrying" | "auth_failed" | "not_sent" | "still_live" | null;
  hostedCopyError: string | null;
  viewers: string[];
  shares: Viewer[];
}
export interface ControlLinks {
  take: string;
  end: string;
  expiresHours: number;
}
