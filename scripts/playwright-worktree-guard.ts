// SPDX-License-Identifier: MIT
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import { WORKTREE_MARKER_PATH } from "./vite-plugin-worktree-marker";

type FetchFn = (url: string, init?: { signal?: AbortSignal }) => Promise<Response>;

/** Repo root of the checkout this module lives in, independent of the launch directory. */
const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/** Generous: a cold or busy machine must not fail a legitimate same-worktree run. */
const MARKER_TIMEOUT_MS = 10_000;

/**
 * Playwright `globalSetup` guard (SV-PBF-163): `reuseExistingServer` would otherwise drive a
 * dev server from another worktree. `globalSetup` runs after Playwright has started or reused
 * its server, so something is answering: it must positively prove it serves this checkout.
 * Rejects for a foreign or unmarked server, and when the marker cannot be read in time.
 */
export async function assertSameWorktreeServer(
  baseURL: string,
  repoRoot: string,
  fetchFn: FetchFn = fetch,
  timeoutMs = MARKER_TIMEOUT_MS,
): Promise<void> {
  const url = `${baseURL.replace(/\/$/, "")}${WORKTREE_MARKER_PATH}`;
  const ours = fs.realpathSync(repoRoot);
  const how =
    `If that is not meant to run here, stop it (kill that pid yourself), use another port ` +
    `(PORT=<free port> for the mock config), or start your own server and set BASE_URL to it.`;
  let marker: { root?: unknown; pid?: unknown } | undefined;
  try {
    const signal = AbortSignal.timeout(timeoutMs);
    const res = await fetchFn(url, { signal });
    const body = res.ok ? await res.text() : "";
    try {
      marker = JSON.parse(body);
    } catch {
      marker = undefined;
    }
  } catch (e) {
    throw new Error(
      `Could not verify which worktree serves ${baseURL}: ${url} failed or timed out after ${timeoutMs}ms ` +
        `(${e instanceof Error ? e.message : String(e)}). Not reusing an unverified server for ${ours}. ${how}`,
    );
  }
  if (typeof marker?.root !== "string") {
    throw new Error(
      `${baseURL} is answered by an unknown or foreign server (no ${WORKTREE_MARKER_PATH} marker), ` +
        `not this worktree (${ours}). ${how}`,
    );
  }
  if (marker.root !== ours) {
    throw new Error(
      `${baseURL} is served by another worktree: ${marker.root} (pid ${marker.pid}); this run is ${ours}. ${how}`,
    );
  }
}

/** Guard for a config's own server URL; skipped when `BASE_URL` points at a server you started. */
export async function guardConfigServer(baseURL: string, fetchFn?: FetchFn): Promise<void> {
  if (process.env.BASE_URL) return;
  await assertSameWorktreeServer(baseURL, REPO_ROOT, fetchFn);
}
