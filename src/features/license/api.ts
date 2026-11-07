// SPDX-License-Identifier: MIT
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** The beta build channels. Internal builds have no license commands. */
export type LicenseChannel = "beta" | "beta-staging";

/** The payload of `license_state` and of the `license://state` event. There is no license: it
 *  carries the channel and the update state. */
export interface LicenseView {
  channel: LicenseChannel;
  /** The build the background updater staged for the next quit, with its release notes. */
  update: { staged_version: string; notes: string } | null;
  /** Set when the next build has to be installed by hand: the app's folder can't be written
   *  without an administrator, or several update attempts in a row failed. */
  update_attention?: "not_writable" | "failing" | "install_failed" | "records_unwritable" | null;
  /** This build gets no updates: it was built without its channel's updater config. */
  updates_off?: boolean;
  /** SW-PB-23: the staged build can be installed now and the app relaunched. */
  restart_ready?: boolean;
}

export const LICENSE_STATE_EVENT = "license://state";

export const licenseState = () => invoke<LicenseView>("license_state");

export const licenseCheckNow = () => invoke<LicenseView>("license_check_now");

/** SW-PB-23: installs the staged build and relaunches. Rejects with the reason when it can't. */
export const licenseRestartToUpdate = () => invoke<null>("license_restart_to_update");

/** SW-PB-23: drops an armed restart that didn't happen (the button's reset). */
export const licenseCancelRestart = () => invoke<null>("license_cancel_restart");

/** SW-PB-23: how many agent runs without a terminal are going (a restart ends them). */
export const licenseHeadlessRuns = () => invoke<number>("license_headless_runs");

/** Subscribes to every state change. Resolves to the unlisten function. */
export function onLicenseState(handler: (view: LicenseView) => void): Promise<UnlistenFn> {
  return listen<LicenseView>(LICENSE_STATE_EVENT, (event) => handler(event.payload));
}
