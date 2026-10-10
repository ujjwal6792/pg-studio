//! Byte-oriented helpers for tailing Drizzle Studio log files.
//!
//! The tail loop in [`crate::app`] used to `read_to_string` each chunk
//! (re-validating UTF-8 over already-seen bytes), then split lines with
//! `partial.find('\n')` + `drain(..idx)` per line. That memmoves the
//! remainder once per line — O(M²) on a long partial line — and lets
//! `partial` grow without bound when the writer never emits `\n`
//! (e.g. `\r` progress-bar overwrites).
//!
//! This module deepens the parsing side into pure, unit-testable pieces:
//!
//! * [`split_lines`] — memchr-style single-pass scan over a byte buffer.
//!   It reports complete lines plus the consumed prefix length, so the
//!   caller drains the prefix **once** instead of once per line.
//! * [`PartialLine`] — pending-byte accumulator with a 1 MiB cap. Overflow
//!   drops the oldest bytes (recent output matters most) and Lone `\r`
//!   acts as a line boundary, so `\r`-heavy streams stay bounded too.
//! * [`decode_bytes`] — lossy UTF-8 decode for one chunk, so invalid
//!   sequences become U+FFFD instead of dropping the whole read.
//! * [`last_n_lines`] / [`read_last_lines`] — bounded "last N lines" used
//!   at session-restore time without reading whole files.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Cap for [`PartialLine`]'s pending bytes (1 MiB).
///
/// A single log line longer than this is almost certainly a runaway writer
/// (or `\r` overwrites without `\n`); the head is dropped and
/// [`PartialLine::was_truncated`] records the loss.
pub const MAX_PARTIAL_BYTES: usize = 1024 * 1024;

/// Block size for the backwards scan in [`read_last_lines`].
const READ_BLOCK: u64 = 8192;

/// Upper bound on how far back [`read_last_lines`] scans for newlines, so
/// restoring a session against a huge single-line file stays bounded.
const MAX_SCAN_BYTES: u64 = 2 * 1024 * 1024;

/// Lossy UTF-8 decode of one raw chunk.
///
/// Invalid sequences become U+FFFD; nothing is dropped and this never
/// panics, unlike `str::from_utf8` on a partial multi-byte char at a
/// chunk edge.
pub fn decode_bytes(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Normalise one raw decoded line: strip a single trailing `\r`
/// (covers `\r\n`), keep the rest verbatim.
///
/// Returns `None` for blank/whitespace-only lines, mirroring the old tail
/// loop's `line.trim().is_empty()` filter.
pub fn clean_line(raw: &str) -> Option<String> {
    let line = raw.strip_suffix('\r').unwrap_or(raw);
    if line.trim().is_empty() {
        None
    } else {
        Some(line.to_string())
    }
}

/// Scan `buf` once for complete lines and return them plus the consumed
/// prefix length.
///
/// * Both `\n` and lone `\r` terminate a line; `\r\n` counts as one break.
/// * Each complete segment is lossy-decoded ([`decode_bytes`]) and filtered
///   ([`clean_line`]); blank segments are skipped.
/// * The returned `usize` is the offset just past the last delimiter — the
///   caller drains `..consumed` **once** (O(M)), leaving the incomplete
///   tail in place. Returns `0` when no delimiter was found.
///
/// No allocation or memmove happens here besides the returned lines.
pub fn split_lines(buf: &[u8]) -> (Vec<String>, usize) {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < buf.len() {
        match buf[i] {
            b'\n' => {
                let mut seg = &buf[start..i];
                if seg.ends_with(b"\r") {
                    seg = &seg[..seg.len() - 1];
                }
                if let Some(line) = clean_line(&decode_bytes(seg)) {
                    lines.push(line);
                }
                start = i + 1;
            }
            b'\r' if buf.get(i + 1) != Some(&b'\n') => {
                // Lone `\r` (progress-bar overwrite): emit the segment so
                // `\r`-only streams cannot grow the pending buffer forever.
                if let Some(line) = clean_line(&decode_bytes(&buf[start..i])) {
                    lines.push(line);
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    (lines, start)
}

/// Accumulates pending (not yet newline-terminated) log bytes across reads.
///
/// Overflow past [`MAX_PARTIAL_BYTES`] drops the oldest bytes and sets the
/// truncated flag; query it with [`PartialLine::was_truncated`].
#[derive(Debug, Default)]
pub struct PartialLine {
    buf: Vec<u8>,
    truncated: bool,
}

impl PartialLine {
    /// Empty accumulator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Pending byte count.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// True when no bytes are pending.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// True once overflow has dropped old bytes.
    pub fn was_truncated(&self) -> bool {
        self.truncated
    }

    /// Drop all pending bytes and clear the truncated flag.
    pub fn clear(&mut self) {
        self.buf.clear();
        self.truncated = false;
    }

    /// Append `chunk` (capped) and extract newly completed lines.
    ///
    /// The consumed prefix is drained once; the incomplete tail stays
    /// buffered for the next feed.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        if chunk.is_empty() {
            return Vec::new();
        }
        let overflow = (self.buf.len() + chunk.len()).saturating_sub(MAX_PARTIAL_BYTES);
        if overflow > 0 {
            // Drop the oldest bytes; recent output is what the user needs.
            let drop = overflow.min(self.buf.len());
            self.buf.drain(..drop);
            // Remainder of the overflow eats into the incoming chunk head.
            let skip = overflow - drop;
            self.truncated = true;
            self.buf.extend_from_slice(&chunk[skip.min(chunk.len())..]);
        } else {
            self.buf.extend_from_slice(chunk);
        }
        // Defensive: lone-`\r` splitting in `split_lines` normally keeps
        // this bounded, but never let the buffer exceed the cap even if
        // the caller feeds without delimiters between drains.
        if self.buf.len() > MAX_PARTIAL_BYTES {
            let excess = self.buf.len() - MAX_PARTIAL_BYTES;
            self.buf.drain(..excess);
            self.truncated = true;
        }
        let (lines, consumed) = split_lines(&self.buf);
        if consumed > 0 {
            self.buf.drain(..consumed);
        }
        lines
    }
}

/// Pure last-N-lines over an in-memory byte window.
///
/// Splits on `\n` (stripping `\r`), skips blank lines, and counts a
/// non-blank unterminated tail as the final line — the same visible lines
/// `str::lines` filtering produced, without holding a whole file as UTF-8.
pub fn last_n_lines(buf: &[u8], count: usize) -> Vec<String> {
    if count == 0 || buf.is_empty() {
        return Vec::new();
    }
    let (mut lines, consumed) = split_lines(buf);
    if consumed < buf.len()
        && let Some(line) = clean_line(&decode_bytes(&buf[consumed..]))
    {
        lines.push(line);
    }
    let skip = lines.len().saturating_sub(count);
    lines.drain(..skip);
    lines
}

/// Read the last `count` non-blank lines of the file at `path`.
///
/// Scans backwards in [`READ_BLOCK`]-sized blocks (bounded by
/// [`MAX_SCAN_BYTES`]) so restoring a session never reads a whole large
/// log into memory. Missing/unreadable files yield an empty vec.
pub fn read_last_lines(path: &Path, count: usize) -> Vec<String> {
    if count == 0 {
        return Vec::new();
    }
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let len = match file.seek(SeekFrom::End(0)) {
        Ok(len) => len,
        Err(_) => return Vec::new(),
    };
    if len == 0 {
        return Vec::new();
    }
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    let mut blocks_len: u64 = 0;
    let mut newlines = 0usize;
    let mut pos = len;
    while pos > 0 && blocks_len < MAX_SCAN_BYTES {
        let take = READ_BLOCK.min(pos).min(MAX_SCAN_BYTES - blocks_len);
        pos -= take;
        if file.seek(SeekFrom::Start(pos)).is_err() {
            break;
        }
        let mut block = vec![0u8; take as usize];
        if file.read_exact(&mut block).is_err() {
            break;
        }
        newlines += block.iter().filter(|b| **b == b'\n').count();
        blocks_len += take;
        blocks.push(block);
        if newlines > count {
            break;
        }
    }
    let at_bof = pos == 0;
    let mut window: Vec<u8> = Vec::with_capacity(blocks_len as usize);
    for block in blocks.iter().rev() {
        window.extend_from_slice(block);
    }
    let mut slice = window.as_slice();
    if !at_bof {
        // The window front is mid-line; drop through the first `\n` so the
        // cut fragment is not mistaken for a full line.
        match slice.iter().position(|b| *b == b'\n') {
            Some(i) => slice = &slice[i + 1..],
            None => {
                // No full line in a bounded scan (one gigantic line):
                // show its bounded tail rather than nothing.
                let tail = &slice[slice.len().saturating_sub(READ_BLOCK as usize)..];
                return match clean_line(&decode_bytes(tail)) {
                    Some(line) => vec![line],
                    None => Vec::new(),
                };
            }
        }
    }
    last_n_lines(slice, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitter_emits_complete_lines_and_leaves_remainder() {
        let (lines, consumed) = split_lines(b"alpha\nbeta\npartial");
        assert_eq!(lines, vec!["alpha".to_string(), "beta".to_string()]);
        assert_eq!(consumed, b"alpha\nbeta\n".len());
    }

    #[test]
    fn splitter_handles_crlf_and_skips_blanks() {
        let (lines, consumed) = split_lines(b"a\r\n\n   \n\t\nb\r\ntail");
        assert_eq!(lines, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(consumed, b"a\r\n\n   \n\t\nb\r\n".len());
    }

    #[test]
    fn splitter_treats_lone_cr_as_boundary() {
        let (lines, consumed) = split_lines(b"10%\r50%\r100%\nrest");
        assert_eq!(
            lines,
            vec!["10%".to_string(), "50%".to_string(), "100%".to_string()]
        );
        assert_eq!(consumed, b"10%\r50%\r100%\n".len());
    }

    #[test]
    fn splitter_reports_zero_consumed_without_delimiter() {
        let (lines, consumed) = split_lines(b"no newline yet");
        assert!(lines.is_empty());
        assert_eq!(consumed, 0);
    }

    #[test]
    fn accumulator_joins_chunks_split_mid_line() {
        let mut p = PartialLine::new();
        assert!(p.is_empty());
        assert_eq!(p.feed(b"hel"), Vec::<String>::new());
        assert_eq!(p.len(), 3);
        assert_eq!(p.feed(b"lo\nwor"), vec!["hello".to_string()]);
        assert_eq!(p.feed(b"ld\n"), vec!["world".to_string()]);
        assert!(p.is_empty());
    }

    #[test]
    fn accumulator_caps_unbounded_partial_line() {
        let mut p = PartialLine::new();
        let huge = vec![b'x'; MAX_PARTIAL_BYTES + 4096];
        let lines = p.feed(&huge);
        assert!(lines.is_empty());
        assert!(p.len() <= MAX_PARTIAL_BYTES);
        assert!(p.was_truncated());
        // Stream stays bounded on further feeds without newlines.
        let lines = p.feed(&vec![b'y'; 4096]);
        assert!(lines.is_empty());
        assert!(p.len() <= MAX_PARTIAL_BYTES);
    }

    #[test]
    fn accumulator_cr_stream_does_not_grow_forever() {
        let mut p = PartialLine::new();
        let mut emitted = 0usize;
        for _ in 0..2000 {
            emitted += p.feed(b"progress\r").len();
        }
        assert!(emitted > 0);
        assert!(p.len() < MAX_PARTIAL_BYTES);
    }

    #[test]
    fn decode_is_lossy_not_lossing() {
        let line = decode_bytes(b"hi \xff\xfe ok");
        assert!(line.contains("hi "));
        assert!(line.contains(" ok"));
        assert!(line.contains('\u{FFFD}'));
        // A split multi-byte char at a chunk edge still decodes lossily
        // ("h" + lone lead byte 0xC3).
        assert!(decode_bytes(&"héllo".as_bytes()[..2]).contains('\u{FFFD}'));
    }

    #[test]
    fn clean_line_strips_cr_and_rejects_blanks() {
        assert_eq!(clean_line("a\r"), Some("a".to_string()));
        assert_eq!(clean_line("a"), Some("a".to_string()));
        assert_eq!(clean_line("   "), None);
        assert_eq!(clean_line("\r"), None);
        assert_eq!(clean_line(""), None);
    }

    #[test]
    fn last_n_lines_takes_tail_and_counts_unterminated_tail() {
        let buf = b"one\n\ntwo\nthree\nfour";
        assert_eq!(
            last_n_lines(buf, 2),
            vec!["three".to_string(), "four".to_string()]
        );
        assert_eq!(last_n_lines(buf, 50).len(), 4);
        assert!(last_n_lines(buf, 0).is_empty());
        assert!(last_n_lines(b"", 5).is_empty());
        assert!(last_n_lines(b"   \n\t\n", 5).is_empty());
    }
}
