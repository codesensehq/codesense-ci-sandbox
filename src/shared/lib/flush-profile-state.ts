// SPDX-License-Identifier: MIT
/** Loaded only at a switch boundary to avoid a profile/store import cycle. */
export async function flushProfileState(): Promise<void> {
  const { flushConfigForProfileSwitch } = await import("@/shared/stores/config-store");
  const { flushViewerForProfileSwitch } = await import("@/shared/stores/viewer-store");
  const { flushOrgForProfileSwitch } = await import("@/shared/stores/org-store");
  const { flushShortcutsForProfileSwitch } = await import("@/shared/stores/shortcuts-store");
  await flushConfigForProfileSwitch();
  // Org and viewer also update app-state.json: serialize their read/modify/write operations.
  // Drain any older org autosave before the viewer writes its newer snapshot.
  await flushOrgForProfileSwitch();
  await flushViewerForProfileSwitch();
  await flushShortcutsForProfileSwitch();
}
