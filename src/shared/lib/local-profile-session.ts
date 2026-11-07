// SPDX-License-Identifier: MIT
import { invoke } from "@tauri-apps/api/core";
import type { LocalProfileStatus } from "@/shared/stores/local-profile-store";

let bootStatus: Promise<LocalProfileStatus> | null = null;
/** Immutable renderer identity: delayed saves can never follow a profile switch. */
export function rendererProfileStatus(): Promise<LocalProfileStatus> {
  if (!bootStatus) {
    bootStatus = invoke<LocalProfileStatus>("local_profile_status").catch((error: unknown) => {
      bootStatus = null;
      throw error;
    });
  }
  return bootStatus;
}
export async function rendererPrincipal(): Promise<string | null> {
  return (await rendererProfileStatus()).active?.principal ?? null;
}
