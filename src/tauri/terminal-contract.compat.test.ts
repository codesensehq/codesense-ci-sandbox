// SPDX-License-Identifier: MIT
/**
 * OC-4 AC6 — the terminal capability compatibility contract.
 *
 * The general IPC contract test (`commands.contract.test.ts`) proves TS⇄Rust
 * parity and pins the *total* command count. This companion pins the *terminal*
 * surface specifically: the command names, the event-channel wire, and the
 * seq-boundary / legacy-rollback DTO fields that OC-9/OC-10 must preserve while
 * they move code behind the OC-4 contract. A rename or removal of any of these is
 * an incompatible change and turns this test red.
 *
 * The compatibility rule this encodes: command names, event payloads, session
 * identities, replay/watermark semantics, and persisted formats remain
 * compatible; an incompatible change requires versioned fixtures, migration and
 * rollback behavior, and explicit approval — at which point the corresponding
 * constant here is updated deliberately, as the record of that approval. OC-4
 * only *specifies* this; OC-9/OC-10 prove parity when the implementations move.
 *
 * Parses source directly — no runtime Tauri needed.
 */
import { describe, it, expect } from "vitest";
import { readFileSync } from "fs";
import { resolve } from "path";

/** Strip `//` and block comments so a pin cannot be satisfied by a mention in
 *  prose (TS and Rust share this comment syntax). */
function stripComments(src: string): string {
  return src.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\/\/.*$/gm, "");
}

const rustFile = (rel: string) =>
  stripComments(readFileSync(resolve(__dirname, "../../src-tauri/src", rel), "utf-8"));

const commandsTs = stripComments(readFileSync(resolve(__dirname, "commands.ts"), "utf-8"));
// The PTY runtime lives in the Tauri-free `codesense-pty-core` crate; every event it
// produces reaches the frontend through the app's one event-sink adapter.
const ptyHostRs = rustFile("commands/pty_host.rs");
const libRs = rustFile("lib.rs");
const ptyBackendRs = rustFile("commands/pty_backend.rs");

/** The terminal/PTY command surface CodeSense depends on (FR15). */
const TERMINAL_COMMANDS = [
  "write_pty",
  "submit_pty_line",
  "resize_pty",
  "reserve_terminal_stream",
  "subscribe_terminal",
  "unsubscribe_terminal",
  "read_terminal_text",
  "session_is_idle",
  "session_has_live_agent",
  "sessions_have_live_agent",
  "sessions_running_processes",
  "reconfigure_warm_pool",
  // SW-LN-35: the shell terminals run, and its Settings override.
  "terminal_shell_info",
  "set_terminal_shell_override",
  // The host-owned terminal layout: a terminal opens and closes through its verbs,
  // and a launch or reload restores the tabs from its snapshot (SW-RT-7b).
  "terminal_layout_snapshot",
  "terminal_open_tab",
  "terminal_close_tab",
  "terminal_respawn_tab",
  "terminal_place_tab",
  "terminal_rename_tab",
  "terminal_move_tab",
  "terminal_set_active",
  "terminal_set_grid",
  "terminal_set_view_hint",
  "terminal_set_tags",
  "terminal_set_descriptor",
  "terminal_close_workspace",
  // The terminal server's version skew and its restart.
  "terminal_server_status",
  "restart_terminal_server",
  // SV-7: Settings manages this instance's terminal server.
  "terminal_server_overview",
  "stop_terminal_server",
  "start_terminal_server",
  "replace_terminal_server",
  "end_unowned_terminal_session",
] as const;

/** The event-channel wire, each pinned against the source that actually emits it —
 *  the app's event-sink adapter. `pty-output-stream-` is a per-stream prefix
 *  (SW-FX-51) and the only channel terminal output rides; `pty-exit` is the exit event. */
const EVENT_CHANNELS: ReadonlyArray<{ channel: string; source: string; where: string }> = [
  { channel: "pty-exit", source: ptyHostRs, where: "pty_host.rs" },
  { channel: "pty-output-stream-", source: ptyHostRs, where: "pty_host.rs" },
  { channel: "terminal-layout-changed", source: ptyHostRs, where: "pty_host.rs" },
  { channel: "terminal-runtime-event", source: ptyHostRs, where: "pty_host.rs" },
  { channel: "terminal-layout-admission-changed", source: libRs, where: "lib.rs" },
  { channel: "terminal-server-incompatible", source: ptyBackendRs, where: "pty_backend.rs" },
  { channel: "terminal-server-restarted", source: ptyBackendRs, where: "pty_backend.rs" },
  { channel: "terminal-layout-resync", source: ptyBackendRs, where: "pty_backend.rs" },
  { channel: "terminal-server-refused", source: ptyBackendRs, where: "pty_backend.rs" },
  { channel: "terminal-streams-lost", source: ptyBackendRs, where: "pty_backend.rs" },
];

function invokes(source: string, command: string): boolean {
  // Escape regex metacharacters so a future command name is matched literally.
  const escaped = command.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`invoke\\s*(?:<[^>]*>)?\\s*\\(\\s*["']${escaped}["']`).test(source);
}

describe("OC-4 terminal capability compatibility contract (AC6)", () => {
  it.each(TERMINAL_COMMANDS)(
    "pins the terminal command `%s` as an invoke in commands.ts",
    (command) => {
      expect(invokes(commandsTs, command)).toBe(true);
    },
  );

  it.each(EVENT_CHANNELS)("pins the event channel `$channel` in $where", ({ channel, source }) => {
    expect(source.includes(channel)).toBe(true);
  });

  it("pins the terminal-status-update channel (emitted by the HTTP route)", () => {
    const terminalRoute = stripComments(
      readFileSync(resolve(__dirname, "../../src-tauri/src/http/routes/terminal.rs"), "utf-8"),
    );
    expect(terminalRoute.includes("terminal-status-update")).toBe(true);
  });

  it("pins the subscribe snapshot boundary (seq and resize_gen) and the stream's resize", () => {
    // The emulator snapshot names the stream position and resize count it holds, so
    // the stream after it carries no gap and no duplicate; resizes ride the stream.
    for (const field of ["snapshot: TerminalSnapshot", "resize_gen: number", "resized:"]) {
      expect(commandsTs.includes(field), field).toBe(true);
    }
    expect(ptyHostRs.includes('"resized"')).toBe(true);
  });

  it("the pin predicate actually fires (a guard that never fails proves nothing)", () => {
    expect(invokes(`invoke("subscribe_terminal")`, "subscribe_terminal")).toBe(true);
    expect(invokes(`invoke("something_else")`, "subscribe_terminal")).toBe(false);
  });
});
