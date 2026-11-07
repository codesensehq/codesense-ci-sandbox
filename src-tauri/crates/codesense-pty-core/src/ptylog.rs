// SPDX-License-Identifier: MIT
//! The `.ptylog` line format shared by the dev recorder and the scripted TUI
//! fixture (tab-separated, one record per line, in stream order):
//! - `O\t<seq_end>\t<base64 delivered slice>` — an output delivery ending at
//!   absolute byte offset `seq_end`.
//! - `R\t<at_seq>\t<cols>\t<rows>` — a PTY resize, applied when the stream stood
//!   at byte offset `at_seq`.

use base64::Engine as _;

pub(crate) fn output_line(seq_end: u64, data: &[u8]) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(data);
    format!("O\t{seq_end}\t{encoded}\n")
}

pub(crate) fn resize_line(at_seq: u64, cols: u16, rows: u16) -> String {
    format!("R\t{at_seq}\t{cols}\t{rows}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_and_resize_lines_have_the_documented_format() {
        assert_eq!(output_line(42, b"\x1b[2;3Hhi"), "O\t42\tG1syOzNIaGk=\n");
        assert_eq!(output_line(0, b""), "O\t0\t\n");
        assert_eq!(resize_line(42, 120, 40), "R\t42\t120\t40\n");
    }
}
