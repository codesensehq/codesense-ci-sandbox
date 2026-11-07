// SPDX-License-Identifier: MIT
// SW-DX-1: tag-schema type hub. The Rust-mirrored shapes live in `@/tauri/commands`
// (the single source of truth for the command contract); re-export them here so
// tag-schema consumers can import types + display config from one place.

export type { TagType, TagDef, TagSchema } from "@/tauri/commands";
export type { TagDisplay } from "./tag-display";
export { tagDisplay, rowDisplay } from "./tag-display";
