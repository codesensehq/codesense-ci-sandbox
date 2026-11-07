// SPDX-License-Identifier: MIT
import { listen } from "@tauri-apps/api/event";

/** A backend presentation fence, never authority for an operation. */
export interface PrivateAuthorityStatus {
  epoch: string;
  available: boolean;
}

let floor = 0n;
let floorRetired = false;
let active: string | null = null;
const retired = new Set<() => void>();
let listening: Promise<void> | null = null;
let listeningReady = false;

function epochNumber(epoch: string): bigint | null {
  if (typeof epoch !== "string" || epoch.length > 20 || !/^(0|[1-9][0-9]*)$/.test(epoch))
    return null;
  const value = BigInt(epoch);
  return value <= 18446744073709551615n ? value : null;
}

function revoke(): void {
  active = null;
  for (const invalidate of retired) invalidate();
}

/** Retirement events can overtake or trail a local-profile status response. Neither
 * ordering may reopen an old epoch; events only retire and never grant. */
export function retirePrivateAuthority(status: PrivateAuthorityStatus): void {
  const value = epochNumber(status.epoch);
  if (value === null || value < floor) return;
  if (value > floor) floorRetired = false;
  floor = value;
  floorRetired ||= !status.available;
  if (active !== null && (BigInt(active) < floor || floorRetired)) revoke();
}

export function observePrivateAuthority(status: PrivateAuthorityStatus | null | undefined): void {
  if (!status) {
    revoke();
    return;
  }
  const value = epochNumber(status.epoch);
  if (value === null || value < floor) return;
  retirePrivateAuthority(status);
  if (active !== status.epoch || !status.available) revoke();
  active = status.available && !floorRetired && listeningReady ? status.epoch : null;
}

/** Capture the current presentation fence before an asynchronous private read. */
export function currentPrivateAuthorityEpoch(): string | null {
  return active;
}

export function acceptsPrivateAuthority(epoch: string | null | undefined): boolean {
  // Absence denotes existing local/legacy output, not a private grant.
  return epoch == null || epoch === active;
}

export function onPrivateAuthorityRetired(invalidate: () => void): () => void {
  retired.add(invalidate);
  return () => retired.delete(invalidate);
}

/** Registration must not wedge local authentication. A late registration is
 * disposed and cannot grant private presentation; a later call can retry. */
export function listenForPrivateAuthorityRetirement(): Promise<void> {
  if (listening) return listening;
  const attempt = new Promise<void>((resolve) => {
    let settled = false;
    let abandoned = false;
    const timer = setTimeout(() => finish(false), 1_000);
    function finish(ready: boolean): void {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      listeningReady = ready;
      if (!ready) {
        abandoned = true;
        revoke();
        listening = null;
      }
      resolve();
    }
    void listen<PrivateAuthorityStatus>("private-authority-retired", ({ payload }) => {
      if (abandoned) return;
      retirePrivateAuthority(payload);
    }).then(
      (unlisten) => {
        if (settled) {
          unlisten();
          return;
        }
        finish(true);
      },
      () => finish(false),
    );
  });
  listening = attempt;
  return attempt;
}
