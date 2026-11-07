// SPDX-License-Identifier: MIT
//! Record a real program's terminal stream as a `.ptylog` through the product's
//! own session registry and recorder (a debug build, `CODESENSE_PTY_RECORD`).
//!
//! ```text
//! cargo run -p codesense-pty-core --example record_pty -- \
//!   --out <dir> --id <session> [--cwd <dir>] [--size 100x30] \
//!   [--step sleep:<ms> | --step type:<text> | --step key:<enter|esc|ctrl-c|up|down> |
//!    --step resize:<cols>x<rows>]... -- <program> [args...]
//! ```
//!
//! Steps run in order; the session is killed after the last one. The recording
//! is `<out>/<session>.ptylog`.

use std::sync::Arc;
use std::time::Duration;

use codesense_pty_core::domain::{
    PortUnavailable, PtySessionId, TerminalDomain, TerminalDomainEvent, TerminalEventSink, ViewerId,
};
use codesense_pty_core::layout::ServerIdentity;
use codesense_pty_core::runner::{LocalPtyRunner, PtyRunner, PtySpec};
use codesense_pty_core::{SessionMeta, SessionRegistration, TerminalHost};
use portable_pty::{CommandBuilder, PtySize};

struct Discard;

impl TerminalEventSink for Discard {
    fn emit(&self, _event: TerminalDomainEvent) -> Result<(), PortUnavailable> {
        Ok(())
    }
}

enum Step {
    Sleep(Duration),
    Bytes(Vec<u8>),
    Resize(u16, u16),
}

struct Args {
    out: String,
    id: String,
    cwd: Option<String>,
    size: (u16, u16),
    steps: Vec<Step>,
    program: Vec<String>,
}

fn parse_size(value: &str) -> Result<(u16, u16), String> {
    let (cols, rows) = value
        .split_once('x')
        .ok_or_else(|| format!("size must be <cols>x<rows>, got {value}"))?;
    Ok((
        cols.parse().map_err(|e| format!("cols {cols}: {e}"))?,
        rows.parse().map_err(|e| format!("rows {rows}: {e}"))?,
    ))
}

fn parse_step(value: &str) -> Result<Step, String> {
    let (kind, arg) = value
        .split_once(':')
        .ok_or_else(|| format!("step must be <kind>:<arg>, got {value}"))?;
    Ok(match kind {
        "sleep" => Step::Sleep(Duration::from_millis(
            arg.parse().map_err(|e| format!("sleep {arg}: {e}"))?,
        )),
        "type" => Step::Bytes(arg.as_bytes().to_vec()),
        "key" => Step::Bytes(
            match arg {
                "enter" => "\r",
                "esc" => "\x1b",
                "ctrl-c" => "\x03",
                "up" => "\x1b[A",
                "down" => "\x1b[B",
                other => return Err(format!("unknown key {other}")),
            }
            .as_bytes()
            .to_vec(),
        ),
        "resize" => {
            let (cols, rows) = parse_size(arg)?;
            Step::Resize(cols, rows)
        }
        other => return Err(format!("unknown step {other}")),
    })
}

fn parse_args() -> Result<Args, String> {
    let mut raw = std::env::args().skip(1);
    let mut args = Args {
        out: String::new(),
        id: String::new(),
        cwd: None,
        size: (100, 30),
        steps: Vec::new(),
        program: Vec::new(),
    };
    while let Some(flag) = raw.next() {
        if flag == "--" {
            args.program = raw.by_ref().collect();
            break;
        }
        let value = raw.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--out" => args.out = value,
            "--id" => args.id = value,
            "--cwd" => args.cwd = Some(value),
            "--size" => args.size = parse_size(&value)?,
            "--step" => args.steps.push(parse_step(&value)?),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if args.out.is_empty() || args.id.is_empty() || args.program.is_empty() {
        return Err("--out, --id and a program after -- are required".into());
    }
    Ok(args)
}

fn main() -> Result<(), String> {
    let args = parse_args()?;
    // The registry reads the recording directory when the session registers.
    std::env::set_var("CODESENSE_PTY_RECORD", &args.out);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let host = TerminalHost::new(
        ServerIdentity::ephemeral("record-pty"),
        Arc::new(Discard),
        runtime.handle().clone(),
    );

    let mut command = CommandBuilder::new(&args.program[0]);
    command.args(&args.program[1..]);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    if let Some(cwd) = &args.cwd {
        command.cwd(cwd);
    }
    let (cols, rows) = args.size;
    let pty = runtime
        .block_on(LocalPtyRunner.spawn(PtySpec {
            command,
            size: PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            },
        }))
        .map_err(String::from)?;
    host.register(
        pty,
        SessionRegistration {
            session_id: args.id.clone(),
            meta: SessionMeta::default(),
            home_cwd: None,
            osc_enabled: false,
            is_pool_member: false,
            scrollback: codesense_pty_core::emulator::DEFAULT_SCROLLBACK,
        },
    )?;

    let domain: &dyn TerminalDomain = &host;
    let session = PtySessionId::new(args.id.as_str());
    let viewer = ViewerId::new("record-pty");
    for step in &args.steps {
        match step {
            Step::Sleep(duration) => std::thread::sleep(*duration),
            Step::Bytes(bytes) => domain
                .write(&session, bytes)
                .map_err(|e| format!("write: {e:?}"))?,
            Step::Resize(cols, rows) => domain
                .resize(&viewer, &session, *cols, *rows)
                .map_err(|e| format!("resize: {e:?}"))?,
        }
    }
    domain.kill(&session).map_err(|e| format!("kill: {e:?}"))?;
    Ok(())
}
