# Recorded terminal streams (`.ptylog`)

Real byte streams from real CLIs, recorded through the PTY layer's own session
registry and dev recorder, plus one generated stream from a seeded scripted TUI. Every display-parity and backend-emulator test replays
these with `src/shared/terminal/__fixtures__/ptylog.ts`.

## Format

One record per line, tab-separated, in stream order:

| Record | Fields                   | Meaning                                                                                         |
| ------ | ------------------------ | ----------------------------------------------------------------------------------------------- |
| `O`    | `seq_end`, base64 bytes  | Output delivered to the display; `seq_end` is the absolute byte offset at the end of the batch. |
| `R`    | `at_seq`, `cols`, `rows` | A PTY resize, applied when the stream stood at byte `at_seq`.                                   |

Both are written under the per-session lock at the point the stream is ordered,
so replaying the file top to bottom applies every resize at its byte offset.

## Files

| File                 | Program                                                                                          | What it holds                                                                                                                                                                                                                                                                                                                                                                                         |
| -------------------- | ------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `codex-tui.ptylog`   | `codex-cli 0.158.0` (`codex -c check_for_update_on_startup=false -c model_reasoning_effort=low`) | Alternate-screen TUI. Full-frame redraws by absolute cursor address (`ESC [ r ; c H`), synchronized-output brackets (`ESC [ ? 2026 h/l`), alternate-screen enter and exit. 3 resizes mid-stream, each answered within 10 ms by a full frame. **Never emits a scroll region (`ESC [ t ; b r`)**: 0.158 renders the whole frame on the alternate screen.                                                |
| `claude-ink.ptylog`  | Claude Code `2.1.141` (`claude --model sonnet`)                                                  | Ink inline UI on the normal screen: welcome banner, prompt box, spinner, and streamed answer, redrawn by relative cursor moves (`ESC [ n A`, `ESC [ n C`) and line erases (`ESC [ K`). 3 resizes, each answered within 25 ms by a full repaint: cursor home (`ESC [ H`), then `ESC [ 2 K` on every row, then the UI again. **Never emits `ESC [ r ; c H`, a scroll region, or the alternate screen.** |
| `append-only.ptylog` | `/bin/sh` (macOS `sh-3.2`) running `seq 1 3000`                                                  | The control: numbered lines, no cursor addressing. 2 resizes, each answered by the prompt redraw.                                                                                                                                                                                                                                                                                                     |

Both agent recordings use the same prompt, sent as typed text and Enter:

> Do not run any commands or use any tools. Write a numbered list of 60 short one-line facts about octopuses.

Each was recorded at 100×30, resized to 80×24 and 120×32 while the answer
streamed, then back to 100×30, then ended with Ctrl-C twice. The working
directory was an empty `/tmp/th1-demo`, already trusted by both CLIs (no trust dialog in either file).

## How they were made

```sh
cd src-tauri && cargo build -p codesense-pty-core --example record_pty
../target/debug/examples/record_pty --out <dir> --id codex-tui --cwd /tmp/th1-demo \
  --size 100x30 --step sleep:5000 --step "type:<prompt>" --step sleep:500 --step key:enter \
  --step sleep:8000 --step resize:80x24 --step sleep:3000 --step resize:120x32 \
  --step sleep:30000 --step resize:100x30 --step sleep:3000 --step key:ctrl-c \
  --step sleep:500 --step key:ctrl-c --step sleep:1000 -- codex <flags above>
```

`record_pty` spawns the program through the product's registry with
`CODESENSE_PTY_RECORD` set, so the recording comes from the same write points
the app uses. The app records too: run a debug build with
`CODESENSE_PTY_RECORD=<dir>` and every terminal session writes
`<dir>/<session>.ptylog`.

## The generated fixture

`tui-fixture.ptylog` is not a recording. It is the output of the seeded,
cursor-addressed TUI in `src-tauri/crates/codesense-pty-core/src/tui_fixture.rs`
(cargo feature `tui-fixture`, off in release builds), which draws what neither
real CLI emits together: absolute cursor addressing (`ESC [ r ; c H`) with line
and display erases, a scroll region (`ESC [ t ; b r`) scrolled up past its
bottom margin and down by reverse index (`ESC M`), wide characters, alternate
screen enter and exit, and a full redraw after each of 3 resizes (80×24 →
100×30 → 60×20 on the alternate screen → 120×32). It starts at 80×24 and ends on
the normal screen with `[120x32] tui-fixture end` on the last row. Each output is
split into one to three seeded slices, so a slice can end mid-sequence or
mid-character. No PTY is involved, so the file is byte-identical for a seed; the
Rust test `tests/tui_fixture.rs` fails if the checked-in file is stale.
Regenerate it from the repo root:

```sh
cargo run -p codesense-pty-core --example tui_fixture --features tui-fixture -- \
  --emit-ptylog tests/fixtures/terminal/tui-fixture.ptylog --seed 38
```

Run the same example without `--emit-ptylog` to get the live harness workload:
it draws on the real tty, redraws at the new size on every `SIGWINCH`, and exits
cleanly on `SIGTERM`, `SIGINT` or `SIGHUP`.

## No secrets

The recordings hold no credentials, tokens, or home paths. The Claude Code
welcome banner's account name and email, and each CLI's session id, were
replaced with same-length placeholders (`Tester`, `someone@example.invalid`,
`00000000-0000-0000-0000-000000000000`) across the whole stream, so every
`seq_end`, `at_seq`, and cursor position is unchanged. `ptylog.test.ts` fails
if an email address, session id, or `/Users/` path appears.

## Trimming

Each file is under 1 MB. If a new recording is larger, trim only by dropping
lines from the tail at an `O` boundary. Never remove a prefix or a middle span:
that drops mode sets or an alternate-screen enter and corrupts the terminal
state every later byte depends on.
