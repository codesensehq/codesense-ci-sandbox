// SPDX-License-Identifier: MIT
import { SLOT_GRANT_CEILINGS } from "@/features/dashboard/lib/verb-registry.generated";
import { getPanel, listPanels } from "../right-pane/panel-registry";
import { SCRATCH_PANEL_ID } from "../right-pane/right-pane-layout";

export type SlotId = "right-pane" | "dock" | "file-nav";
export type SlotPresentation = "docked" | "overlay";
/** Host declarations only. Overlay is a presentation of a region, never another region. */
export interface SlotDescriptor {
  id: SlotId;
  region: SlotId;
  multiplicity: "one" | "many";
  presentations: readonly SlotPresentation[];
  grantCeiling: readonly string[];
  nativeDefault: Readonly<Record<string, NativeDefault>>;
  hostOwned: readonly string[];
  teamBinding: "fixed" | "follows-context";
  /** Slot context reports `narrow` below this width. */
  narrowBelowPx: number;
}
/** A built-in body: a right-pane panel or a component its host renders. */
export interface NativeDefault {
  panelId?: string;
  label: string;
  entry: "panel" | "component";
}
const SURFACES = {
  notes: { panelId: SCRATCH_PANEL_ID, label: "Notes", entry: "panel" as const },
};
const DOCK_SURFACES = {
  "agent-list": { label: "Agents list", entry: "component" as const },
};
const chrome = [
  "section chrome",
  "tab strip",
  "drag",
  "resize",
  "maximize button",
  "badge position",
] as const;
export const SLOT_REGISTRY: Readonly<Record<SlotId, SlotDescriptor>> = {
  "right-pane": {
    id: "right-pane",
    region: "right-pane",
    multiplicity: "many",
    presentations: ["docked", "overlay"],
    grantCeiling: SLOT_GRANT_CEILINGS["right-pane"],
    nativeDefault: SURFACES,
    hostOwned: chrome,
    teamBinding: "fixed",
    narrowBelowPx: 300,
  },
  "file-nav": {
    id: "file-nav",
    region: "file-nav",
    multiplicity: "one",
    presentations: ["docked"],
    grantCeiling: SLOT_GRANT_CEILINGS["file-nav"],
    nativeDefault: { files: { label: "Files", entry: "component" } },
    hostOwned: ["width", "collapse", "resize"],
    teamBinding: "follows-context",
    narrowBelowPx: 260,
  },
  dock: {
    id: "dock",
    region: "dock",
    multiplicity: "one",
    presentations: ["docked", "overlay"],
    grantCeiling: SLOT_GRANT_CEILINGS.dock,
    nativeDefault: DOCK_SURFACES,
    hostOwned: [
      "mount",
      "width",
      "fold",
      "resize",
      "menus",
      "popovers",
      "quick view",
      "stop dialog",
    ],
    teamBinding: "follows-context",
    // The built-in Agents list goes narrow here; a dock dashboard switches with it.
    narrowBelowPx: 260,
  },
};
/** The dock frame's instance id (one per window), distinct from any right-pane panel. */
export const DOCK_FRAME_PANEL_ID = "slot:dock";
export const canonicalSurface = (surface?: string) =>
  surface === "human-inbox" ? "inbox" : surface;
export function nativeSurface(slot: SlotId, surface?: string): NativeDefault | undefined {
  const { nativeDefault, multiplicity } = SLOT_REGISTRY[slot];
  // A one-body region has one built-in, whatever surface its dashboard declares.
  return (
    nativeDefault[canonicalSurface(surface) ?? ""] ??
    (multiplicity === "one" ? Object.values(nativeDefault)[0] : undefined)
  );
}
/** Keep PanelDefinition and its registry as the right pane's sole body owner. */
export const rightPanePanels = { get: getPanel, list: listPanels };

export const FILE_NAV_FRAME_PANEL_ID = "slot:file-nav";
