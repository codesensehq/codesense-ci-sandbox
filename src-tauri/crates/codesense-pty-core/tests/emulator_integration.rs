// SPDX-License-Identifier: MIT
//! The real emulator worker behind a real session registry: recorded streams
//! replayed through a fake PTY and the real batcher must leave the worker's
//! terminal exactly as a headless xterm fed the recording directly (the
//! checked-in `tests/fixtures/terminal/<name>.dump.json`, written by
//! `pnpm run fixtures:dump`), and a restarted worker must pick every session
//! up at its current position.
//!
//! Needs the bundled worker (`pnpm run build:emulator`, or
//! `CODESENSE_EMULATOR_WORKER`). Without one the tests skip, unless `CI=1` or
//! `CODESENSE_REQUIRE_EMULATOR=1`, where a missing worker fails them.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use codesense_pty_core::domain::{
    PortUnavailable, PtySessionId, TerminalDomain, TerminalDomainEvent, TerminalEventSink,
    ViewerId, ViewportId,
};
use codesense_pty_core::emulator::{
    EmulatorConfig, EmulatorLink, EmulatorStatus, EmulatorSupervisor, SessionEmulator, TextMode,
    WORKER_BIN,
};
use codesense_pty_core::layout::ServerIdentity;
use codesense_pty_core::runner::PtySession;
use codesense_pty_core::term_state::StreamId;
use codesense_pty_core::{SessionMeta, SessionRegistration, TerminalHost};
use portable_pty::{Child, ChildKiller, ExitStatus, MasterPty, PtySize};

const ANSWER: Duration = Duration::from_secs(20);

/// The bundled worker, or `None` (with a note) when this machine has none and
/// nothing requires one.
fn worker() -> Option<PathBuf> {
    let path = std::env::var_os("CODESENSE_EMULATOR_WORKER")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let triple = match (std::env::consts::OS, std::env::consts::ARCH) {
                ("macos", "aarch64") => "aarch64-apple-darwin",
                ("macos", "x86_64") => "x86_64-apple-darwin",
                ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
                ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
                other => panic!("no emulator worker is built for {other:?}"),
            };
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../binaries")
                .join(format!("{WORKER_BIN}-{triple}"))
        });
    if path.is_file() {
        return Some(path);
    }
    let required = std::env::var_os("CI").is_some_and(|v| v == "1" || v == "true")
        || std::env::var_os("CODESENSE_REQUIRE_EMULATOR").is_some_and(|v| v == "1");
    assert!(
        !required,
        "no emulator worker at {} — build it with `pnpm run build:emulator`",
        path.display()
    );
    eprintln!(
        "skipping: no emulator worker at {} (build it with `pnpm run build:emulator`)",
        path.display()
    );
    None
}

struct NoopSink;

impl TerminalEventSink for NoopSink {
    fn emit(&self, _: TerminalDomainEvent) -> Result<(), PortUnavailable> {
        Ok(())
    }
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().unwrap())
}

/// Keeps every event.
#[derive(Default)]
struct RecordingSink(Mutex<Vec<TerminalDomainEvent>>);

impl TerminalEventSink for RecordingSink {
    fn emit(&self, event: TerminalDomainEvent) -> Result<(), PortUnavailable> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
}

/// A host running the real worker from `dir`.
fn host(worker: &Path, dir: &Path) -> TerminalHost {
    host_with_sink(worker, dir, Arc::new(NoopSink))
}

fn host_with_sink(worker: &Path, dir: &Path, sink: Arc<dyn TerminalEventSink>) -> TerminalHost {
    let host = TerminalHost::new(
        ServerIdentity::ephemeral("emulator-integration"),
        sink,
        runtime().handle().clone(),
    );
    host.start_emulator(EmulatorConfig::new(
        worker.to_path_buf(),
        dir.join("runtime"),
    ))
    .expect("start the emulator worker");
    host
}

/// The PTY side of a session whose output the test writes itself.
#[derive(Debug, Clone, Default)]
struct FakeChild {
    killed: Arc<Mutex<bool>>,
}

impl ChildKiller for FakeChild {
    fn kill(&mut self) -> std::io::Result<()> {
        *self.killed.lock().unwrap() = true;
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(self.clone())
    }
}

impl Child for FakeChild {
    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        Ok((*self.killed.lock().unwrap()).then(|| ExitStatus::with_exit_code(0)))
    }

    fn wait(&mut self) -> std::io::Result<ExitStatus> {
        Ok(ExitStatus::with_exit_code(0))
    }

    fn process_id(&self) -> Option<u32> {
        None
    }
}

struct FakeMaster {
    size: Mutex<PtySize>,
}

impl MasterPty for FakeMaster {
    fn resize(&self, size: PtySize) -> Result<(), anyhow::Error> {
        *self.size.lock().unwrap() = size;
        Ok(())
    }

    fn get_size(&self) -> Result<PtySize, anyhow::Error> {
        Ok(*self.size.lock().unwrap())
    }

    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> {
        anyhow::bail!("the fake PTY's reader is handed over at construction")
    }

    fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> {
        anyhow::bail!("the fake PTY's writer is handed over at construction")
    }

    fn process_group_leader(&self) -> Option<libc::pid_t> {
        None
    }

    fn as_raw_fd(&self) -> Option<std::os::unix::io::RawFd> {
        None
    }

    fn tty_name(&self) -> Option<PathBuf> {
        None
    }
}

/// Register `session` on `host` over a fake PTY of `cols`×`rows`; the returned
/// stream is the PTY's output end.
fn fake_session(host: &TerminalHost, session: &str, (cols, rows): (u16, u16)) -> UnixStream {
    let (output, reader) = UnixStream::pair().unwrap();
    let master = FakeMaster {
        size: Mutex::new(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }),
    };
    let pty = PtySession::new(
        Box::new(master),
        Box::new(FakeChild::default()),
        Box::new(reader),
        Box::new(std::io::sink()),
    );
    host.register(
        pty,
        SessionRegistration {
            session_id: session.to_string(),
            meta: SessionMeta::default(),
            home_cwd: None,
            osc_enabled: false,
            is_pool_member: false,
            // The reference terminals keep xterm's default scrollback.
            scrollback: 1000,
        },
    )
    .expect("register the fake session");
    output
}

/// The session's stream position: the bytes its PTY produced so far.
fn head(host: &TerminalHost, session: &PtySessionId) -> u64 {
    host.read_terminal_text(session, 1, TextMode::Normal)
        .unwrap()
        .seq
}

fn wait_for_head(host: &TerminalHost, session: &PtySessionId, seq: u64) {
    let deadline = Instant::now() + ANSWER;
    while head(host, session) < seq {
        assert!(Instant::now() < deadline, "stream stuck before byte {seq}");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(head(host, session), seq);
}

enum Record {
    Output(Vec<u8>),
    Resize { at_seq: u64, cols: u16, rows: u16 },
}

fn fixture(name: &str) -> (Vec<Record>, serde_json::Value) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tests/fixtures/terminal");
    let log = std::fs::read_to_string(dir.join(format!("{name}.ptylog"))).unwrap();
    let records = log
        .lines()
        .map(
            |line| match line.split('\t').collect::<Vec<_>>().as_slice() {
                ["O", _, data] => Record::Output(
                    base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .unwrap(),
                ),
                ["R", at_seq, cols, rows] => Record::Resize {
                    at_seq: at_seq.parse().unwrap(),
                    cols: cols.parse().unwrap(),
                    rows: rows.parse().unwrap(),
                },
                _ => panic!("malformed ptylog line: {line}"),
            },
        )
        .collect();
    (records, dump_fixture(name))
}

fn dump_fixture(name: &str) -> serde_json::Value {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tests/fixtures/terminal");
    serde_json::from_str(&std::fs::read_to_string(dir.join(format!("{name}.dump.json"))).unwrap())
        .unwrap()
}

/// Every recording starts at this size (`PTYLOG_START` in the frontend).
const START: (u16, u16) = (100, 30);

#[test]
fn batcher_feeds_worker_in_order() {
    let Some(worker) = worker() else { return };
    let dir = tempfile::tempdir().unwrap();
    let host = host(&worker, dir.path());
    let viewer = ViewerId::new("app");

    for name in ["codex-tui", "claude-ink", "tui-fixture"] {
        let session = PtySessionId::new(name);
        let mut output = fake_session(&host, name, START);
        let (records, expected) = fixture(name);
        let mut size = START;
        let mut size_changes = 0;
        let mut written = 0u64;
        for record in records {
            match record {
                Record::Output(data) => {
                    output.write_all(&data).unwrap();
                    written += data.len() as u64;
                }
                Record::Resize { at_seq, cols, rows } => {
                    // Applied where the recording applied it: after exactly
                    // the bytes before it.
                    wait_for_head(&host, &session, at_seq);
                    host.resize(&viewer, &session, cols, rows).unwrap();
                    if (cols, rows) != size {
                        size = (cols, rows);
                        size_changes += 1;
                    }
                }
            }
        }
        wait_for_head(&host, &session, written);
        let dump = host.emulator_dump(&session, ANSWER).unwrap();
        assert_eq!(
            serde_json::Value::Object(dump.buffer.clone()),
            expected,
            "{name}: the worker's terminal differs from the reference"
        );
        assert_eq!(dump.seq_end, head(&host, &session), "{name}: seq_end");
        assert_eq!(dump.resize_gen, size_changes, "{name}: resize_gen");
        assert!(!dump.history_lost);
    }

    // An invalid byte mid-line reaches the worker as the U+FFFD the display
    // gets (the fixture dump is a display fed that string).
    let session = PtySessionId::new("invalid-utf8");
    let mut output = fake_session(&host, "invalid-utf8", START);
    output.write_all(b"before \xff after\r\nnext line").unwrap();
    let delivered = "before \u{FFFD} after\r\nnext line".len() as u64;
    wait_for_head(&host, &session, delivered);
    let dump = host.emulator_dump(&session, ANSWER).unwrap();
    assert_eq!(
        serde_json::Value::Object(dump.buffer),
        dump_fixture("invalid-utf8")
    );
    assert_eq!(dump.seq_end, delivered);

    let text = host
        .emulator_text(&session, 10, TextMode::Normal, ANSWER)
        .unwrap();
    assert_eq!(text.lines, ["before \u{FFFD} after", "next line"]);
    assert!(!text.alt_screen);
}

#[test]
fn restart_recreates_with_positions() {
    let Some(worker) = worker() else { return };
    let dir = tempfile::tempdir().unwrap();
    let host = host(&worker, dir.path());
    let viewer = ViewerId::new("app");
    let session = PtySessionId::new("restart");
    let mut output = fake_session(&host, "restart", (80, 24));
    output.write_all(b"before the crash\r\n").unwrap();
    host.resize(&viewer, &session, 90, 24).unwrap();
    wait_for_head(&host, &session, 18);
    assert_eq!(
        host.emulator_dump(&session, ANSWER).unwrap().buffer["lines"][0],
        "before the crash"
    );

    let Some(EmulatorStatus::Running { pid, .. }) = host.emulator_status() else {
        panic!("worker not running: {:?}", host.emulator_status());
    };
    // SAFETY: signalling the worker this test's host started.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) }, 0);
    let deadline = Instant::now() + ANSWER;
    loop {
        match host.emulator_status() {
            Some(EmulatorStatus::Running { pid: new, .. }) if new != pid => break,
            _ => assert!(Instant::now() < deadline, "worker not restarted"),
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    output.write_all(b"after").unwrap();
    wait_for_head(&host, &session, 23);
    let dump = host.emulator_dump(&session, ANSWER).unwrap();
    assert!(dump.history_lost);
    assert_eq!(dump.seq_end, 23);
    assert_eq!(dump.resize_gen, 1);
    assert_eq!(dump.buffer["cols"], 90);
    assert_eq!(
        dump.buffer["lines"][0],
        "─── earlier output unavailable: terminal service restarted ───"
    );
    assert_eq!(dump.buffer["lines"][1], "after");
}

/// A sustained 10 MB/s firehose into one session for 10 s: the worker's lag
/// stays bounded and is back to 0 within a second of the burst ending.
/// `cargo test --release -p codesense-pty-core --test emulator_integration
/// firehose -- --ignored --nocapture`
#[test]
#[ignore = "budget measurement; 10 s"]
fn firehose_lag_is_bounded() {
    let Some(worker) = worker() else { return };
    let dir = tempfile::tempdir().unwrap();
    let host = host(&worker, dir.path());
    let session = PtySessionId::new("firehose");
    let mut output = fake_session(&host, "firehose", (120, 40));
    let line = "\x1b[36mfirehose\x1b[0m 0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ\r\n";
    let chunk = line.repeat(10 * 1024 * 1024 / 100 / line.len());
    let started = Instant::now();
    let mut max_lag = 0u64;
    let mut sent = 0u64;
    // 100 chunks a second, each ~100 KiB.
    for tick in 0..1000u32 {
        output.write_all(chunk.as_bytes()).unwrap();
        sent += chunk.len() as u64;
        max_lag = max_lag.max(host.emulator_lag_bytes(&session).unwrap());
        let next = started + Duration::from_millis(10) * (tick + 1);
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    }
    let burst = started.elapsed();
    wait_for_head(&host, &session, sent);
    let ended = Instant::now();
    while host.emulator_lag_bytes(&session).unwrap() > 0 {
        assert!(
            ended.elapsed() < Duration::from_secs(5),
            "lag never drained"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let drained = ended.elapsed();
    println!(
        "| firehose | {:.1} MB in {burst:?} | max emulator_lag_bytes {max_lag} | drained {drained:?} after the burst |",
        sent as f64 / 1e6
    );
    assert!(max_lag <= 16 * 1024 * 1024, "lag {max_lag} unbounded");
    assert!(
        drained <= Duration::from_secs(1),
        "drained after {drained:?}"
    );
}

#[test]
fn snapshot_reflects_exact_seq() {
    let Some(worker) = worker() else { return };
    let dir = tempfile::tempdir().unwrap();
    let link = EmulatorLink::new();
    let supervisor = EmulatorSupervisor::start(
        EmulatorConfig::new(worker, dir.path().join("runtime")),
        link.clone(),
    )
    .unwrap();
    assert!(matches!(
        supervisor.status(),
        EmulatorStatus::Running { .. }
    ));

    let mut session = SessionEmulator::start(&link, (80, 24), 1_000, 0);
    // A batch big enough that xterm parses it over several time slices, so the
    // snapshot is enqueued while its write callback is still pending.
    let first = "first batch\r\n".repeat(80_000);
    let first_end = first.len() as u64;
    session.feed(first_end, &first);
    let pending = session.snapshot(first_end).expect("a worker is attached");
    session.feed(first_end + 6, "SECOND");

    let snapshot = pending.snapshot(ANSWER).unwrap();
    assert_eq!(snapshot.seq, first_end);
    assert_eq!(
        (snapshot.cols, snapshot.rows, snapshot.resize_gen),
        (80, 24, 0)
    );
    assert!(snapshot.vt.contains("first batch"));
    assert!(
        !snapshot.vt.contains("SECOND"),
        "the snapshot holds output enqueued after it"
    );

    let after = session
        .snapshot(first_end + 6)
        .expect("a worker is attached")
        .snapshot(ANSWER)
        .unwrap();
    assert_eq!(after.seq, first_end + 6);
    assert!(after.vt.contains("SECOND"));
}

/// A stream event after a subscribe, as a display applies it.
enum Live {
    Output(String),
    Resized { cols: u16, rows: u16 },
}

/// `stream`'s events past its snapshot, in order. Everything else the stream
/// carried is a duplicate of what the snapshot holds, which fails the test.
fn live_after(
    events: &[TerminalDomainEvent],
    stream: StreamId,
    seq: u64,
    resize_gen: u32,
) -> Vec<Live> {
    events
        .iter()
        .filter_map(|event| match event {
            TerminalDomainEvent::StreamOutput {
                stream: s,
                data,
                seq: seq_end,
            } if *s == stream => {
                assert!(
                    *seq_end > seq,
                    "output ending at {seq_end} is also in the snapshot at {seq}"
                );
                Some(Live::Output(data.clone()))
            }
            TerminalDomainEvent::StreamResized {
                stream: s,
                resize_gen: generation,
                cols,
                rows,
                ..
            } if *s == stream => {
                assert!(
                    *generation > resize_gen,
                    "resize {generation} is also in the snapshot at {resize_gen}"
                );
                Some(Live::Resized {
                    cols: *cols,
                    rows: *rows,
                })
            }
            _ => None,
        })
        .collect()
}

/// Subscribe to a replayed recording at many points — between a resize and
/// the next output, between two resizes with no output between them, and
/// while the worker is still parsing the batch before — then rebuild a
/// terminal from each snapshot and what its stream carried after it. Every
/// rebuild ends exactly as the live terminal does, and as the recording's
/// reference.
#[test]
fn subscribe_snapshot_then_live_no_gap() {
    let Some(worker) = worker() else { return };
    let dir = tempfile::tempdir().unwrap();
    let sink = Arc::new(RecordingSink::default());
    let host = host_with_sink(&worker, dir.path(), sink.clone());
    let viewer = ViewerId::new("app");

    for name in ["codex-tui", "tui-fixture"] {
        let session = PtySessionId::new(name);
        let mut output = fake_session(&host, name, START);
        let (records, expected) = fixture(name);
        let outputs = records
            .iter()
            .filter(|r| matches!(r, Record::Output(_)))
            .count();
        // Seeded subscribe points: after these output records.
        let mut seed = 0x9e37_79b9_u64 ^ name.len() as u64;
        let mut points = HashSet::new();
        while points.len() < 64.min(outputs) {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            points.insert((seed >> 33) as usize % outputs);
        }
        let mut subscriptions = Vec::new();
        let subscribe = |subscriptions: &mut Vec<_>| {
            subscriptions.push(
                host.subscribe(
                    &session,
                    &ViewportId::new(format!("vp-{}", subscriptions.len())),
                )
                .unwrap(),
            );
        };
        let mut written = 0u64;
        let mut index = 0;
        let mut size = START;
        for record in &records {
            match record {
                Record::Output(data) => {
                    output.write_all(data).unwrap();
                    written += data.len() as u64;
                    if points.contains(&index) {
                        // Right after the write: the batch may still be on
                        // its way, or parsing in the worker.
                        subscribe(&mut subscriptions);
                    }
                    if index == outputs / 2 {
                        // Two resizes with no output between them, and a
                        // subscribe between them and after them.
                        wait_for_head(&host, &session, written);
                        host.resize(&viewer, &session, size.0 + 3, size.1 + 1)
                            .unwrap();
                        subscribe(&mut subscriptions);
                        host.resize(&viewer, &session, size.0, size.1).unwrap();
                        subscribe(&mut subscriptions);
                    }
                    index += 1;
                }
                Record::Resize { at_seq, cols, rows } => {
                    wait_for_head(&host, &session, *at_seq);
                    host.resize(&viewer, &session, *cols, *rows).unwrap();
                    size = (*cols, *rows);
                    // Between a resize and the next output.
                    subscribe(&mut subscriptions);
                }
            }
        }
        wait_for_head(&host, &session, written);
        let live = host.emulator_dump(&session, ANSWER).unwrap();
        assert_eq!(
            serde_json::Value::Object(live.buffer.clone()),
            expected,
            "{name}: the live terminal differs from the reference"
        );
        let events = sink.0.lock().unwrap().clone();

        for (n, subscription) in subscriptions.iter().enumerate() {
            let snapshot = &subscription.snapshot;
            let rebuild = PtySessionId::new(format!("{name}-rebuild-{n}"));
            let mut display = fake_session(&host, rebuild.as_str(), (snapshot.cols, snapshot.rows));
            display.write_all(snapshot.vt.as_bytes()).unwrap();
            let mut fed = snapshot.vt.len() as u64;
            for item in live_after(
                &events,
                subscription.stream_id,
                snapshot.seq,
                snapshot.resize_gen,
            ) {
                match item {
                    Live::Output(data) => {
                        display.write_all(data.as_bytes()).unwrap();
                        fed += data.len() as u64;
                    }
                    Live::Resized { cols, rows } => {
                        wait_for_head(&host, &rebuild, fed);
                        host.resize(&viewer, &rebuild, cols, rows).unwrap();
                    }
                }
            }
            wait_for_head(&host, &rebuild, fed);
            let rebuilt = host.emulator_dump(&rebuild, ANSWER).unwrap();
            assert_eq!(
                rebuilt.buffer, live.buffer,
                "{name}: the display rebuilt from snapshot {n} (seq {}, resize_gen {}) differs",
                snapshot.seq, snapshot.resize_gen
            );
        }
    }
}

/// Register `name` and replay its recording into it, resizes applied where
/// they were recorded; returns once every byte reached the stream.
fn replay_fixture(host: &TerminalHost, name: &str) -> PtySessionId {
    let viewer = ViewerId::new("app");
    let session = PtySessionId::new(name);
    let mut output = fake_session(host, name, START);
    let mut written = 0u64;
    for record in fixture(name).0 {
        match record {
            Record::Output(data) => {
                output.write_all(&data).unwrap();
                written += data.len() as u64;
            }
            Record::Resize { at_seq, cols, rows } => {
                wait_for_head(host, &session, at_seq);
                host.resize(&viewer, &session, cols, rows).unwrap();
            }
        }
    }
    wait_for_head(host, &session, written);
    session
}

/// Raw terminal output with its escape sequences removed: how readers used
/// to turn terminal output into text.
fn escape_stripped(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[test]
fn visible_text_is_what_the_screen_shows() {
    let Some(worker) = worker() else { return };
    let dir = tempfile::tempdir().unwrap();
    let host = host(&worker, dir.path());
    let domain: &dyn TerminalDomain = &host;
    for name in ["codex-tui", "claude-ink", "tui-fixture"] {
        let session = replay_fixture(&host, name);
        let visible = codesense_pty_core::domain::visible_text(domain, &session, 10_000).unwrap();
        let normal = host
            .emulator_text(&session, 10_000, TextMode::Normal, ANSWER)
            .unwrap();
        let expected = if normal.alt_screen {
            host.emulator_text(&session, 10_000, TextMode::Screen, ANSWER)
                .unwrap()
        } else {
            normal
        };
        assert_eq!(visible.text, expected.lines.join("\n"), "{name}");
        assert_eq!(visible.alt_screen, expected.alt_screen, "{name}");
        assert_eq!(visible.seq, head(&host, &session), "{name}: seq");
        assert!(!visible.text.contains('\x1b'), "{name}: escape in text");
    }

    // A full-screen program on the alternate screen: its rows, not the shell
    // scrollback behind it.
    let session = PtySessionId::new("alt");
    let mut output = fake_session(&host, "alt", START);
    let bytes = b"shell line\r\n\x1b[?1049h\x1b[H\x1b[2Jfull screen";
    output.write_all(bytes).unwrap();
    wait_for_head(&host, &session, bytes.len() as u64);
    let visible = codesense_pty_core::domain::visible_text(domain, &session, 10_000).unwrap();
    assert!(visible.alt_screen);
    assert_eq!(visible.text.lines().next(), Some("full screen"));
    assert_eq!(visible.text.split('\n').count(), usize::from(START.1));
    assert!(!visible.text.contains("shell line"));

    // A program redrawing in place on the normal screen pushes each frame into
    // scrollback: the screen read is only the frame showing now.
    let session = PtySessionId::new("frames");
    let mut output = fake_session(&host, "frames", START);
    let mut bytes = Vec::new();
    for frame in 0..60 {
        bytes.extend_from_slice(format!("frame {frame}\r\n").as_bytes());
    }
    output.write_all(&bytes).unwrap();
    wait_for_head(&host, &session, bytes.len() as u64);
    let all = codesense_pty_core::domain::visible_text(domain, &session, 10_000).unwrap();
    let screen = codesense_pty_core::domain::screen_text(domain, &session).unwrap();
    assert!(all.text.contains("frame 0"));
    assert!(!screen.alt_screen);
    assert!(
        !screen.text.lines().any(|line| line.trim_end() == "frame 0"),
        "{}",
        screen.text
    );
    assert!(screen.text.contains("frame 59"));
    assert_eq!(screen.text.split('\n').count(), usize::from(START.1));

    // The old reader on the same Codex recording: the stripped raw output
    // holds lines the screen never shows (redraws overlaid, cursor moves lost).
    let session = PtySessionId::new("codex-tui");
    let visible = codesense_pty_core::domain::visible_text(domain, &session, 10_000).unwrap();
    let raw: Vec<u8> = fixture("codex-tui")
        .0
        .into_iter()
        .filter_map(|record| match record {
            Record::Output(data) => Some(data),
            Record::Resize { .. } => None,
        })
        .flatten()
        .collect();
    let stripped = escape_stripped(&String::from_utf8_lossy(&raw));
    let fragments: Vec<&str> = stripped
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !visible.text.contains(line))
        .collect();
    assert!(!fragments.is_empty(), "the Codex recording has no teeth");
}
