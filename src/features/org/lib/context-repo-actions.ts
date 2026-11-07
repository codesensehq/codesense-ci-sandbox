// SPDX-License-Identifier: MIT
/**
 * Connect / create / locate a context repo (SW-OC-16). Thin user-facing wrappers over the
 * existing local backends (`attach_context_repository`, `create_local_organization`,
 * `detach_context_repository`); selection and continuation stay in `completeLocalSetup`.
 */
import { open } from "@tauri-apps/plugin-dialog";

import { getCurrentUser } from "@/shared/lib/get-current-user";
import { classifyConnectError, type ConnectFailure } from "@/shared/lib/codesense-docs";
import {
  attachContextRepository,
  createLocalOrganization,
  detachContextRepository,
} from "@/tauri/commands";

import { beginLocalSetup, completeLocalSetup, endLocalSetup } from "./local-setup-completion";

export const CHOOSE_CONTEXT_REPO_TITLE = "Choose your team's context repo folder";

/** The OS folder picker. `null` when the user cancels. */
export async function chooseFolder(title = CHOOSE_CONTEXT_REPO_TITLE): Promise<string | null> {
  const path = await open({ directory: true, multiple: false, title });
  return typeof path === "string" ? path : null;
}

export type ContextRepoOutcome = { ok: true } | { ok: false; failure: ConnectFailure } | null;

async function run(
  task: () => Promise<Parameters<typeof completeLocalSetup>[1]>,
  created: boolean,
): Promise<ContextRepoOutcome> {
  if (!beginLocalSetup()) return null;
  const principal = getCurrentUser()?.principal;
  try {
    const result = await task();
    return principal && completeLocalSetup(principal, result, created) ? { ok: true } : null;
  } catch (error) {
    return { ok: false, failure: classifyConnectError(error) };
  } finally {
    endLocalSetup();
  }
}

/** Connect the checkout at `path` and switch to it. `null` = superseded (profile changed). */
export function connectContextRepo(path: string): Promise<ContextRepoOutcome> {
  return run(() => attachContextRepository(path), false);
}

/** Create a new context repo at `path`, connect it and switch to it. */
export function createContextRepo(
  name: string,
  path: string,
  fromExample: boolean,
): Promise<ContextRepoOutcome> {
  return run(() => createLocalOrganization(name, path, fromExample), true);
}

/**
 * A connected context repo moved: drop the stale registration and connect the folder the
 * user located. The checkout itself is never touched.
 */
export function locateContextRepo(slug: string, path: string): Promise<ContextRepoOutcome> {
  return run(async () => {
    await detachContextRepository(slug);
    return attachContextRepository(path);
  }, false);
}
