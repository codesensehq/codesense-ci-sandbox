// SPDX-License-Identifier: MIT
//! The seeded TUI fixture: its `.ptylog` is deterministic, matches the
//! checked-in file, carries every frame kind the parity tests rely on, and the
//! live binary redraws at the new size on `SIGWINCH`.

use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use codesense_pty_core::tui_fixture;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};

fn checked_in_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/terminal/tui-fixture.ptylog")
}

enum Record {
    Output { seq_end: u64, data: Vec<u8> },
    Resize { at_seq: u64, cols: u16, rows: u16 },
}

fn parse(file: &str) -> Vec<Record> {
    file.lines()
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            match fields.as_slice() {
                ["O", seq_end, data] => Record::Output {
                    seq_end: seq_end.parse().unwrap(),
                    data: base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .unwrap(),
                },
                ["R", at_seq, cols, rows] => Record::Resize {
                    at_seq: at_seq.parse().unwrap(),
                    cols: cols.parse().unwrap(),
                    rows: rows.parse().unwrap(),
                },
                _ => panic!("malformed ptylog line: {line}"),
            }
        })
        .collect()
}

fn stream(records: &[Record]) -> Vec<u8> {
    records
        .iter()
        .flat_map(|record| match record {
            Record::Output { data, .. } => data.clone(),
            Record::Resize { .. } => Vec::new(),
        })
        .collect()
}

/// A control sequence `ESC [ <params> <final>` at its byte offset.
struct Csi {
    at: usize,
    params: String,
    final_byte: u8,
}

fn control_sequences(bytes: &[u8]) -> Vec<Csi> {
    let mut found = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == 0x1b && bytes[i + 1] == b'[' {
            let start = i + 2;
            let mut end = start;
            while end < bytes.len() && !(0x40..=0x7e).contains(&bytes[end]) {
                end += 1;
            }
            if end < bytes.len() {
                found.push(Csi {
                    at: i,
                    params: String::from_utf8_lossy(&bytes[start..end]).into_owned(),
                    final_byte: bytes[end],
                });
            }
            i = end + 1;
        } else {
            i += 1;
        }
    }
    found
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[test]
fn tui_fixture_is_deterministic() {
    let first = tui_fixture::ptylog(38);
    assert_eq!(first, tui_fixture::ptylog(38), "same seed, same bytes");
    assert_ne!(first, tui_fixture::ptylog(39), "a different seed differs");
    let checked_in = std::fs::read_to_string(checked_in_fixture()).expect("checked-in fixture");
    assert!(
        checked_in == first,
        "tests/fixtures/terminal/tui-fixture.ptylog is stale; regenerate it with \
         `cargo run -p codesense-pty-core --example tui_fixture --features tui-fixture -- \
         --emit-ptylog tests/fixtures/terminal/tui-fixture.ptylog --seed 38` from the repo root"
    );
    assert!(checked_in.len() <= 1024 * 1024, "fixture exceeds 1 MB");
}

#[test]
fn tui_fixture_has_required_features() {
    let records = parse(&tui_fixture::ptylog(38));
    let bytes = stream(&records);
    let csis = control_sequences(&bytes);

    let cup = |csi: &Csi| csi.final_byte == b'H' && csi.params.contains(';');
    assert!(csis.iter().any(cup), "no absolute CUP");
    assert!(
        csis.iter().any(|c| c.final_byte == b'K'),
        "no erase-in-line"
    );
    assert!(
        csis.iter().any(|c| c.final_byte == b'J'),
        "no erase-in-display"
    );
    assert!(find(&bytes, b"\x1bM").is_some(), "no reverse index");
    assert!(
        String::from_utf8_lossy(&bytes).contains('端'),
        "no wide characters"
    );

    // A scroll region, then a CUP to its bottom margin, then a line feed there
    // (which scrolls the region), all before the region is reset.
    let scrolled = csis.iter().enumerate().any(|(i, set)| {
        let Some((top, bottom)) = set
            .params
            .split_once(';')
            .filter(|_| set.final_byte == b'r')
        else {
            return false;
        };
        assert!(top.parse::<u16>().unwrap() < bottom.parse::<u16>().unwrap());
        let reset = csis[i + 1..]
            .iter()
            .find(|c| c.final_byte == b'r')
            .map_or(bytes.len(), |c| c.at);
        csis[i + 1..]
            .iter()
            .filter(|c| c.at < reset && cup(c))
            .any(|c| {
                c.params.split(';').next() == Some(bottom) && bytes[c.at..reset].contains(&b'\n')
            })
    });
    assert!(scrolled, "no scroll inside a DECSTBM region");

    let enter = find(&bytes, b"\x1b[?1049h").expect("alt-screen enter");
    let exit = find(&bytes, b"\x1b[?1049l").expect("alt-screen exit");
    assert!(enter < exit);

    let mut resizes = 0;
    let mut last_at = 0;
    let mut applied = 0;
    for (i, record) in records.iter().enumerate() {
        match record {
            Record::Output { seq_end, data } => {
                assert_eq!(*seq_end - applied, data.len() as u64, "contiguous stream");
                applied = *seq_end;
            }
            Record::Resize { at_seq, cols, rows } => {
                resizes += 1;
                assert!(*at_seq >= last_at, "at_seq runs backwards");
                assert_eq!(*at_seq, applied, "resize sits where the stream stands");
                last_at = *at_seq;
                let Some(Record::Output { seq_end, .. }) = records.get(i + 1) else {
                    panic!("resize {i} is not followed by output");
                };
                assert!(at_seq <= seq_end);
                let redraw = &bytes[*at_seq as usize..];
                assert!(
                    redraw.starts_with(b"\x1b[r\x1b[H\x1b[2J"),
                    "output after resize {i} is not a full redraw"
                );
                let header = format!("tui-fixture {cols}x{rows}");
                assert!(find(redraw, header.as_bytes()).is_some());
                let edge = format!("\x1b[1;{cols}H|");
                assert!(find(redraw, edge.as_bytes()).is_some());
            }
        }
    }
    assert!(resizes >= 2, "only {resizes} resizes");
}

/// The example binary cargo built alongside this test.
fn example_binary() -> PathBuf {
    let deps = std::env::current_exe().unwrap();
    let target = deps.parent().unwrap().parent().unwrap();
    target.join("examples").join("tui_fixture")
}

fn wait_for(output: &Mutex<Vec<u8>>, from: usize, needle: &[u8], within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if find(&output.lock().unwrap()[from..], needle).is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn live_mode_redraws_on_winch() {
    let size = |cols, rows| PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    };
    let pair = native_pty_system().openpty(size(80, 24)).unwrap();
    let mut child = pair
        .slave
        .spawn_command(CommandBuilder::new(example_binary()))
        .expect("spawn the tui_fixture example (built by `cargo test --features tui-fixture`)");
    drop(pair.slave);

    let output = Arc::new(Mutex::new(Vec::new()));
    let mut reader = pair.master.try_clone_reader().unwrap();
    let sink = Arc::clone(&output);
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });

    assert!(
        wait_for(&output, 0, b"tui-fixture 80x24", Duration::from_secs(5)),
        "no first frame at 80x24"
    );
    let before = output.lock().unwrap().len();
    pair.master.resize(size(100, 30)).unwrap();
    assert!(
        wait_for(&output, before, b"\x1b[1;100H|", Duration::from_secs(1)),
        "no frame addressed for 100 columns within 1 s of the resize"
    );
    assert!(wait_for(
        &output,
        before,
        b"tui-fixture 100x30",
        Duration::from_millis(100)
    ));

    let pid = child.process_id().expect("child pid") as libc::pid_t;
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "live mode did not exit on SIGTERM"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success(), "live mode exited uncleanly: {status:?}");
}
