// SPDX-License-Identifier: MIT
import { create } from "zustand";

import { clearPendingJoinUrl, readPendingJoinUrl } from "@/shared/lib/pending-join";
import type { CurrentUser } from "@/shared/types/domain";

/**
 * The local guest and a join that must survive a reload.
 *
 * The local guest is a fixed, synthetic identity that keys the guest's on-disk
 * workspace the same way a numeric profile id does (see `buildCurrentUser`).
 * Persisted in localStorage (a small, isolated store) so it survives a reload.
 */

/** The synthetic user id a guest's local workspace is keyed by. Well outside
 *  the GitHub id range, non-zero (0 is the logged-out sentinel), and a safe JS
 *  integer so it round-trips through IPC unchanged. */
export const LOCAL_GUEST_USER_ID = 999_999_999_999;

export const LOCAL_GUEST_USER: CurrentUser = {
  principal: String(LOCAL_GUEST_USER_ID),
  github_user_id: LOCAL_GUEST_USER_ID,
  github_username: "guest",
  display_name: "Guest",
  avatar_url: null,
};

export function isGuestUser(user: CurrentUser | null | undefined): boolean {
  return user?.github_user_id === LOCAL_GUEST_USER_ID;
}

interface Persisted {
  /** Entered without an account. */
  guest: boolean;
  /** SW-FU-B6: the join form must (re)open after the coming sign-in or reload.
   *  While set, a signed-in user with no workspace sees the join form instead of
   *  an auto-created workspace. */
  joinPending: boolean;
}

interface FirstRunState extends Persisted {
  /** Enter without an account, as the local guest. */
  enterAsGuest: () => void;
  /** A code-host sign-in replaced the guest. */
  clearGuest: () => void;
  /** The backend still records a guest session (its on-disk marker) that this
   *  webview's storage lost: take the guest back without asking again. */
  adoptBackendGuest: () => void;
  /** SW-FU-B6: the pending join form opened: it no longer has to reopen. */
  landJoinPending: () => void;
  /** SW-FU-B6: the join finished, or the user gave it up: forget it entirely. */
  endJoin: () => void;
}

const STORAGE_KEY = "codesense.first-run";

const EMPTY: Persisted = {
  guest: false,
  joinPending: false,
};

function load(): Persisted {
  try {
    const raw = globalThis.localStorage?.getItem(STORAGE_KEY);
    if (!raw) return { ...EMPTY };
    const p = JSON.parse(raw) as Partial<Persisted>;
    return {
      guest: p.guest === true,
      joinPending: p.joinPending === true,
    };
  } catch {
    return { ...EMPTY };
  }
}

function persist(state: Persisted): void {
  try {
    globalThis.localStorage?.setItem(
      STORAGE_KEY,
      JSON.stringify({
        guest: state.guest,
        joinPending: state.joinPending,
      }),
    );
  } catch {
    // Best-effort: a storage failure only loses the state across reloads.
  }
}

export const useFirstRunStore = create<FirstRunState>((set, get) => {
  const update = (patch: Partial<Persisted>) => {
    set(patch);
    persist(get());
  };
  const initial = load();
  return {
    ...initial,
    enterAsGuest: () => update({ guest: true }),
    clearGuest: () =>
      update({
        guest: false,
        // SW-FU-B6: a guest signing in mid-join is reloaded; the open join form
        // (its URL is in storage) reopens afterwards.
        joinPending: get().joinPending || readPendingJoinUrl() !== null,
      }),
    adoptBackendGuest: () => update({ guest: true }),
    landJoinPending: () => update({ joinPending: false }),
    endJoin: () => {
      clearPendingJoinUrl();
      update({ joinPending: false });
    },
  };
});

/** Test seam: re-read the persisted state (simulates a reload). */
export function readPersistedFirstRun(): Persisted {
  return load();
}

export function resetFirstRunStoreForTest(): void {
  const p = load();
  useFirstRunStore.setState(p);
}
