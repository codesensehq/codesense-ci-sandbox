// SPDX-License-Identifier: MIT
import type { FileRedline, PublishFile } from "./types";

export interface OpenDiffParams {
  repo: string;
  rev: string;
  path: string;
  storyId?: string;
  range?: { start: number; end: number; side?: "after" };
}

export type OpenDiffResult =
  | { status: "opened" }
  | { status: "commitNotLocal"; reason: string; fetchCommand: string }
  | { status: "worktreeRemoved" | "refused" | "unavailable"; reason: string };

export type OpenDiffRead =
  | Exclude<OpenDiffResult, { status: "opened" }>
  | { status: "opened"; redline: FileRedline; files: PublishFile[]; resolvedRev: string };
