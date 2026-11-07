// SPDX-License-Identifier: MIT
import type { Org, OrgConfigFlags } from "@/shared/types/domain";

export type RegistryFile = "OrgYml" | "PeopleYml" | "TeamsYml";
export type FileLoadErrorKind =
  | "NotFound"
  | "ReadError"
  | "TransientReadError"
  | "Parse"
  | "Invariant";

export interface FileLoadError {
  file: RegistryFile;
  path: string;
  kind: FileLoadErrorKind;
  message: string;
}

export interface RawPersonEntry {
  github_user_id: number;
  github_username: string;
  display_name: string | null;
  avatar_url: string | null;
}

export interface RawOrgYml {
  slug: string;
  display_name: string;
  // ISO-8601 UTC, mirroring chrono::DateTime<Utc>.
  created_at: string;
  config_flags: OrgConfigFlags;
}

export interface RawPeopleYml {
  people: RawPersonEntry[];
}

export interface RawSourceRepoEntry {
  owner: string;
  repo: string;
}

export interface RawTeamEntry {
  name: string;
  source_repos: RawSourceRepoEntry[];
  /** Mirrors Rust `RawTeamEntry.landing_page`: the team's home page, relative to
   *  its content folder. Absent ⇒ README.md / README.html / index.html. */
  landing_page?: string;
}

export interface RawTeamsYml {
  teams: RawTeamEntry[];
}

export interface PartialOrg {
  org_yml: RawOrgYml | null;
  people_yml: RawPeopleYml | null;
  teams_yml: RawTeamsYml | null;
}

export interface LoadedOrg {
  slug: string;
  root: string;
  org: Org | null;
  partial: PartialOrg;
  file_errors: FileLoadError[];
}
