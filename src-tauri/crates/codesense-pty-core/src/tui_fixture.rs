// SPDX-License-Identifier: MIT
//! A seeded, cursor-addressed TUI: the one terminal workload whose bytes are
//! known exactly. It draws the frame kinds the real agent CLIs never emit
//! together — absolute cursor addressing (CUP) with line and display erases, a
//! scroll region (DECSTBM) that scrolls up and reverse-indexes down, wide
//! characters, alternate-screen enter and exit, and a full redraw at every new
//! size — from one frame script with two outputs:
//!
//! - [`ptylog`]: the script as a `.ptylog` (no PTY involved), byte-identical
//!   for a seed; the checked-in `tests/fixtures/terminal/tui-fixture.ptylog`.
//! - [`run_live`]: the same frames on a real tty, redrawn on every `SIGWINCH`,
//!   looping until a terminating signal.

use crate::ptylog::{output_line, resize_line};

/// The size the scripted stream starts at.
pub const START_SIZE: (u16, u16) = (80, 24);

/// The sizes the scripted stream resizes through, in order. The second one
/// lands while the alternate screen is active.
const SCRIPTED_RESIZES: [(u16, u16); 3] = [(100, 30), (60, 20), (120, 32)];

/// The text every frame's status row ends with once the script is done.
pub const END_MARKER: &str = "tui-fixture end";

const WORDS: &[&str] = &[
    "alpha", "bravo", "cursor", "delta", "echo", "frame", "grid", "height", "index", "jolt",
    "kernel", "line", "margin", "node", "offset", "parse", "query", "region", "scroll", "tile",
    "width", "端末", "表示", "漢字", "幅広", "画面",
];

/// splitmix64: a tiny deterministic generator, so a seed pins every byte.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A value in `0..bound` (`bound` > 0).
    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// Display width of one char: 2 for the CJK words in [`WORDS`], else 1.
fn char_width(c: char) -> usize {
    if ('\u{2e80}'..='\u{9fff}').contains(&c) {
        2
    } else {
        1
    }
}

/// `text` cut to at most `width` display columns, never splitting a wide char.
fn fit(text: &str, width: usize) -> String {
    let mut used = 0;
    text.chars()
        .take_while(|c| {
            used += char_width(*c);
            used <= width
        })
        .collect()
}

/// One step of the frame script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Bytes written to the terminal.
    Output(Vec<u8>),
    /// The terminal was resized; the next output is the redraw at that size.
    Resize { cols: u16, rows: u16 },
}

/// Draws frames for one terminal size. Every draw returns the bytes of one
/// frame; nothing is written until the caller writes them.
struct Painter {
    rng: Rng,
    cols: u16,
    rows: u16,
    frame: u32,
}

impl Painter {
    fn new(seed: u64, (cols, rows): (u16, u16)) -> Self {
        Self {
            rng: Rng(seed),
            cols,
            rows,
            frame: 0,
        }
    }

    fn resize(&mut self, (cols, rows): (u16, u16)) {
        self.cols = cols;
        self.rows = rows;
    }

    /// Top and bottom rows (1-based) of the scroll region: everything between
    /// the two header rows and the two footer rows.
    fn region(&self) -> (u16, u16) {
        (3, self.rows.saturating_sub(2).max(4))
    }

    fn line(&mut self) -> String {
        let count = 3 + self.rng.below(6);
        let words: Vec<&str> = (0..count)
            .map(|_| WORDS[self.rng.below(WORDS.len() as u64) as usize])
            .collect();
        words.join(" ")
    }

    fn cup(out: &mut Vec<u8>, row: u16, col: u16) {
        out.extend_from_slice(format!("\x1b[{row};{col}H").as_bytes());
    }

    fn text_row(&mut self, out: &mut Vec<u8>, row: u16, text: &str) {
        Self::cup(out, row, 1);
        out.extend_from_slice(fit(text, self.cols as usize).as_bytes());
        out.extend_from_slice(b"\x1b[K");
    }

    /// Header, right-edge marker, and status row: the rows that pin a frame to
    /// its size.
    fn chrome(&mut self, out: &mut Vec<u8>, status: &str) {
        let (cols, rows) = (self.cols, self.rows);
        let header = format!("tui-fixture {cols}x{rows} frame {}", self.frame);
        Self::cup(out, 1, 1);
        out.extend_from_slice(b"\x1b[1m");
        out.extend_from_slice(fit(&header, cols as usize - 1).as_bytes());
        out.extend_from_slice(b"\x1b[0m\x1b[K");
        Self::cup(out, 1, cols);
        out.push(b'|');
        self.text_row(out, 2, &"-".repeat(cols as usize));
        self.text_row(
            out,
            rows.saturating_sub(1).max(1),
            &"-".repeat(cols as usize),
        );
        let status = format!("[{cols}x{rows}] {status}");
        self.text_row(out, rows, &status);
    }

    /// A full redraw: reset the scroll region, home, clear the display, then
    /// address every row.
    fn redraw(&mut self, status: &str) -> Vec<u8> {
        self.frame += 1;
        let mut out = b"\x1b[r\x1b[H\x1b[2J".to_vec();
        let (top, bottom) = self.region();
        for row in top..=bottom {
            let line = self.line();
            self.text_row(&mut out, row, &line);
        }
        self.chrome(&mut out, status);
        out
    }

    /// An in-place update: a few body rows rewritten by CUP + erase-in-line, a
    /// mid-row overwrite whose left side is erased, and the tail erased below.
    fn update(&mut self) -> Vec<u8> {
        self.frame += 1;
        let mut out = Vec::new();
        let (top, bottom) = self.region();
        let span = (bottom - top + 1) as u64;
        for _ in 0..1 + self.rng.below(3) {
            let row = top + self.rng.below(span) as u16;
            let line = self.line();
            self.text_row(&mut out, row, &line);
        }
        let row = top + self.rng.below(span) as u16;
        let col = 1 + self.rng.below(u64::from(self.cols / 2).max(1)) as u16;
        Self::cup(&mut out, row, col);
        out.extend_from_slice(b"\x1b[1K");
        let word = WORDS[self.rng.below(WORDS.len() as u64) as usize];
        let room = (self.cols - col + 1) as usize;
        out.extend_from_slice(fit(word, room).as_bytes());
        if bottom > top {
            let row = bottom - self.rng.below(2) as u16;
            Self::cup(&mut out, row, 1);
            out.extend_from_slice(b"\x1b[J");
        }
        let status = format!("update {}", self.frame);
        self.chrome(&mut out, &status);
        out
    }

    /// Set a scroll region, scroll it up by writing past its bottom margin,
    /// then scroll it down by reverse index at its top margin, then reset it.
    fn scroll(&mut self) -> Vec<u8> {
        self.frame += 1;
        let (top, bottom) = self.region();
        let mut out = format!("\x1b[{top};{bottom}r").into_bytes();
        Self::cup(&mut out, bottom, 1);
        for _ in 0..2 + self.rng.below(4) {
            out.extend_from_slice(b"\n\r");
            let line = self.line();
            out.extend_from_slice(fit(&line, self.cols as usize).as_bytes());
            out.extend_from_slice(b"\x1b[K");
        }
        Self::cup(&mut out, top, 1);
        for _ in 0..1 + self.rng.below(2) {
            out.extend_from_slice(b"\x1bM");
            let line = self.line();
            out.extend_from_slice(b"\r");
            out.extend_from_slice(fit(&line, self.cols as usize).as_bytes());
            out.extend_from_slice(b"\x1b[K");
        }
        out.extend_from_slice(b"\x1b[r");
        let status = format!("scroll {}", self.frame);
        self.chrome(&mut out, &status);
        out
    }

    fn enter_alt(&mut self) -> Vec<u8> {
        let mut out = b"\x1b[?1049h".to_vec();
        out.extend(self.redraw("alt screen"));
        out
    }

    fn exit_alt(&mut self) -> Vec<u8> {
        let mut out = b"\x1b[?1049l".to_vec();
        out.extend(self.redraw("normal screen"));
        out
    }
}

/// The frame script for `seed`: inline frames on the normal screen, a resize,
/// scroll-region frames, the alternate screen with a resize inside it, a
/// resize back on the normal screen, and a final frame ending in
/// [`END_MARKER`] with the cursor shown.
pub fn script(seed: u64) -> Vec<Event> {
    let mut painter = Painter::new(seed, START_SIZE);
    let mut events = Vec::new();
    let mut resizes = SCRIPTED_RESIZES.iter().copied();
    let mut resize = |painter: &mut Painter, events: &mut Vec<Event>| {
        let (cols, rows) = resizes.next().expect("scripted resize");
        painter.resize((cols, rows));
        events.push(Event::Resize { cols, rows });
        events.push(Event::Output(painter.redraw("resized")));
    };

    let mut first = b"\x1b[?25l".to_vec();
    first.extend(painter.redraw("start"));
    events.push(Event::Output(first));
    for _ in 0..3 {
        events.push(Event::Output(painter.update()));
    }
    events.push(Event::Output(painter.scroll()));
    resize(&mut painter, &mut events);
    events.push(Event::Output(painter.scroll()));
    events.push(Event::Output(painter.update()));

    events.push(Event::Output(painter.enter_alt()));
    events.push(Event::Output(painter.update()));
    events.push(Event::Output(painter.scroll()));
    resize(&mut painter, &mut events);
    events.push(Event::Output(painter.update()));
    events.push(Event::Output(painter.exit_alt()));

    resize(&mut painter, &mut events);
    events.push(Event::Output(painter.update()));
    events.push(Event::Output(painter.scroll()));
    let mut last = painter.redraw(END_MARKER);
    last.extend_from_slice(b"\x1b[?25h");
    events.push(Event::Output(last));
    events
}

/// The script for `seed` as a `.ptylog`. Each output is delivered in one to
/// three seeded slices (a slice may end mid-sequence or mid-character, as a
/// real PTY read can); each resize sits at the byte offset the stream stands
/// at, immediately before its redraw.
pub fn ptylog(seed: u64) -> String {
    let mut chunker = Rng(seed ^ 0x7475_692d_6669_7874);
    let mut seq = 0u64;
    let mut file = String::new();
    for event in script(seed) {
        match event {
            Event::Resize { cols, rows } => file.push_str(&resize_line(seq, cols, rows)),
            Event::Output(bytes) => {
                let mut rest = bytes.as_slice();
                let mut slices = 1 + chunker.below(3);
                while !rest.is_empty() {
                    let take = if slices <= 1 {
                        rest.len()
                    } else {
                        1 + chunker.below(rest.len() as u64) as usize
                    };
                    let (slice, tail) = rest.split_at(take);
                    seq += slice.len() as u64;
                    file.push_str(&output_line(seq, slice));
                    rest = tail;
                    slices = slices.saturating_sub(1);
                }
            }
        }
    }
    file
}

#[cfg(unix)]
mod live {
    use std::sync::atomic::{AtomicBool, Ordering};

    pub(super) static RESIZED: AtomicBool = AtomicBool::new(false);
    pub(super) static STOP: AtomicBool = AtomicBool::new(false);

    extern "C" fn on_winch(_: libc::c_int) {
        RESIZED.store(true, Ordering::SeqCst);
    }

    extern "C" fn on_stop(_: libc::c_int) {
        STOP.store(true, Ordering::SeqCst);
    }

    pub(super) fn install_handlers() {
        let winch = on_winch as extern "C" fn(libc::c_int) as libc::sighandler_t;
        let stop = on_stop as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // SAFETY: both handlers only store to an atomic, which is signal-safe.
        unsafe {
            libc::signal(libc::SIGWINCH, winch);
            for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
                libc::signal(signal, stop);
            }
        }
    }

    /// The tty size of stdout, or `None` when stdout is not a tty.
    pub(super) fn tty_size() -> Option<(u16, u16)> {
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        // SAFETY: TIOCGWINSZ writes one `winsize` into the pointer given.
        let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } == 0;
        (ok && size.ws_col > 0 && size.ws_row > 0).then_some((size.ws_col, size.ws_row))
    }
}

/// Run the fixture on the real tty behind stdout: draw frames every `tick`,
/// redraw in full at the new size on every `SIGWINCH`, and on `SIGTERM`,
/// `SIGINT` or `SIGHUP` leave the alternate screen, reset the scroll region,
/// show the cursor, and return.
#[cfg(unix)]
pub fn run_live(seed: u64, tick: std::time::Duration) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::Ordering;

    live::install_handlers();
    let mut out = std::io::stdout().lock();
    let mut painter = Painter::new(seed, live::tty_size().unwrap_or(START_SIZE));
    let mut frame = b"\x1b[?25l".to_vec();
    frame.extend(painter.redraw("live"));
    let mut in_alt = false;
    let mut step = 0u64;
    loop {
        out.write_all(&frame)?;
        out.flush()?;
        std::thread::sleep(tick);
        if live::STOP.load(Ordering::SeqCst) {
            out.write_all(b"\x1b[r\x1b[?1049l\x1b[?25h")?;
            return out.flush();
        }
        if live::RESIZED.swap(false, Ordering::SeqCst) {
            painter.resize(live::tty_size().unwrap_or((painter.cols, painter.rows)));
            frame = painter.redraw("resized");
            continue;
        }
        step += 1;
        frame = match step % 8 {
            3 | 6 => painter.scroll(),
            4 if !in_alt => {
                in_alt = true;
                painter.enter_alt()
            }
            0 if in_alt => {
                in_alt = false;
                painter.exit_alt()
            }
            _ => painter.update(),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_never_splits_a_wide_char_or_overflows() {
        assert_eq!(fit("ab漢字", 3), "ab");
        assert_eq!(fit("ab漢字", 4), "ab漢");
        assert_eq!(fit("端末 表示 grid", 7), "端末 表");
    }
}
