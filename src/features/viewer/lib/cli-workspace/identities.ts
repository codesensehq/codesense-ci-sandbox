// SPDX-License-Identifier: MIT
/**
 * OC-4 — CLI workspace capability contract: the nine distinct identities.
 *
 * The OC-1 capability map found selection, foreground, focus, zoom, and
 * persistence conflated inside the ~11k-line viewer store, where any string id
 * can stand in for any other. This module makes each identity a distinct
 * *branded* type: they are all `string` at runtime (zero cost, wire-compatible),
 * but the compiler rejects passing one where another is required, so an invalid
 * substitution is a build error rather than a silent misroute.
 *
 * Contract-only: these are type declarations. No implementation is moved here —
 * the frontend extraction that routes real call sites through them is OC-9, the
 * Rust side is OC-10. Nothing in this file imports React, Zustand, xterm, the
 * DOM, Tauri, or the viewer store.
 */

declare const identityBrand: unique symbol;

/** A nominal wrapper over a primitive: `Branded<string, "X">` is assignable from
 *  itself but not from `Branded<string, "Y">`, and never from a bare `string`
 *  except through the identity's constructor. */
export type Branded<T, B extends string> = T & { readonly [identityBrand]: B };

/** The team that owns the epic rails and story workspaces. */
export type TeamId = Branded<string, "TeamId">;
/** A grouped rail of story workspaces (the epic group in the left nav). */
export type EpicRailId = Branded<string, "EpicRailId">;
/** A story workspace — the selection unit a pill activates. */
export type StoryWorkspaceId = Branded<string, "StoryWorkspaceId">;
/** The foreground tile a workspace presents (shared / Console / split slot). */
export type ForegroundTileId = Branded<string, "ForegroundTileId">;
/** A terminal tab within a workspace (one shell among several). */
export type TerminalTabId = Branded<string, "TerminalTabId">;
/** The Rust-owned PTY session backing a terminal tab. */
export type PtySessionId = Branded<string, "PtySessionId">;
/** A transient xterm viewport bound to a PTY session while visible. */
export type ViewportId = Branded<string, "ViewportId">;
/** The renderer instance (WebGL/DOM) a viewport currently drives. */
export type RendererId = Branded<string, "RendererId">;
/** The DOM focus target (the xterm textarea) an activation may claim. */
export type DomFocusId = Branded<string, "DomFocusId">;

/** Constructs a branded identity from its raw wire value. One definition for all
 *  nine identities (the TS analogue of the Rust `id_newtype!` macro) — the cast is
 *  written once, not per identity. */
const brandId =
  <B extends Branded<string, string>>() =>
  (value: string): B =>
    value as B;

export const TeamId = brandId<TeamId>();
export const EpicRailId = brandId<EpicRailId>();
export const StoryWorkspaceId = brandId<StoryWorkspaceId>();
export const ForegroundTileId = brandId<ForegroundTileId>();
export const TerminalTabId = brandId<TerminalTabId>();
export const PtySessionId = brandId<PtySessionId>();
export const ViewportId = brandId<ViewportId>();
export const RendererId = brandId<RendererId>();
export const DomFocusId = brandId<DomFocusId>();

/** The wire/store value behind any identity. Use only at a serialization or
 *  storage boundary — never to launder one identity into another. */
export function identityValue(id: Branded<string, string>): string {
  return id;
}
