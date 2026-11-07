// SPDX-License-Identifier: MIT
import { flushProfileState } from "@/shared/lib/flush-profile-state";
import { createSerialWriteQueue } from "@/shared/stores/serial-write-queue";
import { rendererProfileStatus } from "@/shared/lib/local-profile-session";
import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import {
  listenForPrivateAuthorityRetirement,
  observePrivateAuthority,
  type PrivateAuthorityStatus,
} from "@/shared/lib/private-terminal-authority";

export interface LocalProfile {
  principal: string;
  displayName: string;
  machineOwner: { kind: "legacyGithub"; id: number } | { kind: "local"; id: string } | null;
  /** The viewer's own face, either `pack:id` or a `data:` URL. Null falls back
   *  to initials of `displayName`. Private per-user per-machine, never synced. */
  identityAvatar?: string | null;
}
export interface LocalProfileStatus {
  active: LocalProfile | null;
  profiles: LocalProfile[];
  privateAuthority: PrivateAuthorityStatus | null;
  metadataRecovery?: { pending: boolean; error: string | null };
}
export function numericPrincipal(principal: string): number {
  const value = Number(principal);
  if (
    !/^[1-9][0-9]*$/.test(principal) ||
    !Number.isSafeInteger(value) ||
    String(value) !== principal
  ) {
    throw new Error(
      "This local profile cannot be represented safely by the numeric storage adapter.",
    );
  }
  return value;
}
/** The avatar edit the viewer asks for. `pack` picks an avatar-pack face;
 *  `custom` uploads raw image bytes; `clear` drops the chosen avatar. */
export type IdentityAvatarInput =
  | { kind: "pack"; ref: string }
  | { kind: "custom"; bytes: Uint8Array }
  | { kind: "clear" };

interface LocalProfileState {
  status: LocalProfileStatus | null;
  initialized: boolean;
  busy: boolean;
  error: string | null;
  initialize: () => Promise<void>;
  select: (principal: string, displayName?: string) => Promise<void>;
  setIdentity: (edit: {
    displayName?: string | null;
    avatar?: IdentityAvatarInput | null;
  }) => Promise<void>;
}
let generation = 0;
const enqueueIdentityWrite = createSerialWriteQueue();
export const useLocalProfileStore = create<LocalProfileState>((set, get) => ({
  status: null,
  initialized: false,
  busy: false,
  error: null,
  initialize: async () => {
    const mine = ++generation;
    try {
      const boot = await rendererProfileStatus();
      const status = get().initialized
        ? await invoke<LocalProfileStatus>("local_profile_status")
        : boot;
      if (status.active?.principal !== boot.active?.principal) {
        window.location.reload();
        return;
      }
      if (mine !== generation) return;
      if (status.active) {
        try {
          numericPrincipal(status.active.principal);
        } catch (error) {
          // Keep every choice available without exposing an unsafe numeric identity
          // to workspace consumers. Native selection and the boot pin are unchanged.
          set({ status: { ...status, active: null }, initialized: true, error: String(error) });
          return;
        }
      }
      set({ status, initialized: true, error: null });
      void listenForPrivateAuthorityRetirement().then(() => {
        if (mine === generation) observePrivateAuthority(status.privateAuthority);
      });
    } catch (error) {
      if (mine === generation) set({ initialized: true, error: String(error) });
    }
  },
  setIdentity: async ({ displayName, avatar }) => {
    const expectedPrincipal = get().status?.active?.principal;
    if (!expectedPrincipal) throw new Error("No local profile is active");
    const write = enqueueIdentityWrite(async () => {
      const payload: {
        displayName?: string;
        avatar?:
          | { kind: "pack"; ref: string }
          | { kind: "custom"; bytes: number[] }
          | { kind: "clear" };
      } = {};
      if (displayName !== undefined && displayName !== null) payload.displayName = displayName;
      if (avatar) {
        payload.avatar =
          avatar.kind === "custom" ? { kind: "custom", bytes: Array.from(avatar.bytes) } : avatar;
      }
      const profile = await invoke<LocalProfile>("set_local_identity", {
        ...payload,
        expectedPrincipal,
      });
      set((state) => {
        if (!state.status) return state;
        const profiles = state.status.profiles.map((p) =>
          p.principal === profile.principal ? profile : p,
        );
        const active =
          state.status.active?.principal === profile.principal ? profile : state.status.active;
        return { status: { ...state.status, active, profiles } };
      });
    });
    await write;
  },
  select: async (principal, displayName) => {
    if (get().busy) return;
    ++generation;
    set({ busy: true, error: null });
    try {
      numericPrincipal(principal);
      if (get().status?.active) {
        await flushProfileState();
        // Finish identity edits already accepted for this principal before native selection.
        await enqueueIdentityWrite(async () => undefined);
      }
      const status = await invoke<LocalProfileStatus>("select_local_profile", {
        principal,
        displayName,
      });
      observePrivateAuthority(status.privateAuthority);
      // Reload drops every transient principal-scoped cache and captured UI session.
      window.location.reload();
    } catch (error) {
      set({ busy: false, error: String(error) });
    }
  },
}));
