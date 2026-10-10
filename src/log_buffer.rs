//! Bounded ring buffer for session and global log lines.
//!
//! Replaces unbounded `Vec<String>` log stores. Every [`LogBuffer::push`]
//! evicts the oldest line past the cap, and each line's display width
//! (`chars().count()`) is computed once at push time so 10Hz render frames
//! never rescan text.

use std::collections::VecDeque;

/// Default cap: how many trailing lines a buffer retains.
///
/// 5000 lines is far beyond any visible window (a Logs pane shows dozens)
/// while keeping a full copy-to-clipboard under ~1MB for typical line
/// lengths.
pub const DEFAULT_CAP: usize = 5000;

#[derive(Debug, Clone)]
struct Entry {
    text: String,
    /// Display width cached at push time; see [`LogBuffer::iter_with_widths`].
    width: usize,
}

/// Ring buffer of log lines with cached per-line display widths.
///
/// The interface is `push`/`len`/`iter`-shaped on purpose: producers (log
/// tailers, background jobs) only ever push, consumers (render code, copy)
/// only ever read the tail.
#[derive(Debug, Clone)]
pub struct LogBuffer {
    entries: VecDeque<Entry>,
    cap: usize,
}

impl LogBuffer {
    /// Creates an empty buffer retaining at most `cap` trailing lines.
    /// A `cap` of 0 retains nothing (every push is dropped).
    pub fn new(cap: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            cap,
        }
    }

    /// Maximum number of retained lines.
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Appends `line`, evicting the oldest line when already at capacity.
    /// Width caching makes this O(1) amortized.
    pub fn push(&mut self, line: String) {
        if self.cap == 0 {
            return;
        }
        if self.entries.len() >= self.cap {
            self.entries.pop_front();
        }
        let width = line.chars().count();
        self.entries.push_back(Entry { text: line, width });
    }

    /// Number of retained lines.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no lines are retained.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Retained lines, oldest first. Borrowed: no per-frame clone.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &str> + '_ {
        self.entries.iter().map(|e| e.text.as_str())
    }

    /// `(line, cached width)` pairs, oldest first, parallel to [`Self::iter`].
    /// Render tail math consumes the widths without rescanning text.
    pub fn iter_with_widths(&self) -> impl DoubleEndedIterator<Item = (&str, usize)> + '_ {
        self.entries.iter().map(|e| (e.text.as_str(), e.width))
    }

    /// Joins the last `n` lines with `\n`, touching only that tail —
    /// never the whole buffer. Returns `""` when empty or `n == 0`.
    pub fn snapshot_last(&self, n: usize) -> String {
        let len = self.entries.len();
        let start = len.saturating_sub(n);
        if start >= len {
            return String::new();
        }
        let mut out = String::with_capacity(
            self.entries
                .iter()
                .skip(start)
                .map(|e| e.text.len() + 1)
                .sum(),
        );
        for (i, entry) in self.entries.iter().enumerate().skip(start) {
            if i > start {
                out.push('\n');
            }
            out.push_str(&entry.text);
        }
        out
    }

    /// Drops all retained lines.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_CAP)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_evicts_oldest_first() {
        let mut buf = LogBuffer::new(3);
        for i in 0..5 {
            buf.push(format!("l{i}"));
        }
        assert_eq!(buf.len(), 3);
        assert!(!buf.is_empty());
        let lines: Vec<&str> = buf.iter().collect();
        assert_eq!(lines, ["l2", "l3", "l4"]);
        // Newest-first traversal (render tail math) sees the same window.
        let rev: Vec<&str> = buf.iter().rev().collect();
        assert_eq!(rev, ["l4", "l3", "l2"]);
    }

    #[test]
    fn default_cap_retains_configured_window() {
        assert_eq!(DEFAULT_CAP, 5000);
        let mut buf = LogBuffer::default();
        assert_eq!(buf.capacity(), DEFAULT_CAP);
        for i in 0..(DEFAULT_CAP + 5) {
            buf.push(format!("l{i}"));
        }
        assert_eq!(buf.len(), DEFAULT_CAP);
        let lines: Vec<&str> = buf.iter().collect();
        assert_eq!(lines[0], "l5");
        assert_eq!(lines[DEFAULT_CAP - 1], format!("l{}", DEFAULT_CAP + 4));
    }

    #[test]
    fn snapshot_last_is_bounded() {
        let mut buf = LogBuffer::new(10);
        assert_eq!(buf.snapshot_last(5), "");
        for line in ["a", "b", "c"] {
            buf.push(line.to_string());
        }
        assert_eq!(buf.snapshot_last(0), "");
        assert_eq!(buf.snapshot_last(2), "b\nc");
        // Oversized n clamps to the whole buffer, no trailing newline.
        assert_eq!(buf.snapshot_last(99), "a\nb\nc");
        assert_eq!(buf.snapshot_last(3), "a\nb\nc");
    }

    #[test]
    fn widths_cached_at_push_and_survive_eviction() {
        let mut buf = LogBuffer::new(2);
        buf.push("héllo".to_string()); // 5 chars, 6 bytes
        buf.push("✅ ok".to_string()); // 4 chars
        buf.push("plain".to_string()); // evicts "héllo"
        let paired: Vec<(&str, usize)> = buf.iter_with_widths().collect();
        assert_eq!(paired, [("✅ ok", 4), ("plain", 5)]);
        // Widths stay parallel to iter().
        let lines: Vec<&str> = buf.iter().collect();
        assert_eq!(paired.len(), lines.len());
        for (i, (text, _)) in paired.iter().enumerate() {
            assert_eq!(*text, lines[i]);
        }
    }

    #[test]
    fn clear_empties_without_changing_cap() {
        let mut buf = LogBuffer::new(2);
        buf.push("x".to_string());
        buf.clear();
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.snapshot_last(10), "");
        assert_eq!(buf.capacity(), 2);
        buf.push("y".to_string());
        assert_eq!(buf.snapshot_last(1), "y");
    }

    #[test]
    fn zero_cap_retains_nothing() {
        let mut buf = LogBuffer::new(0);
        buf.push("x".to_string());
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
    }
}
